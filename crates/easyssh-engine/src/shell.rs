//! The in-process engine: one russh connection per host, shared by the
//! terminal, the file browser and tunnels, owned by the app process.
//!
//! This is the same [`Registry`] the desktop daemon uses (see
//! [`crate::registry`]): a terminal, an SFTP session and a forward on the same
//! host lease one connection, so a password or key question is asked once. The
//! connection lingers for 30 s after its last user went away, so leaving the
//! terminal for the file browser does not log in again.

use crate::events::Events;
use crate::files::Files;
use crate::login::SavedPassword;
use crate::prompts::{PromptInteraction, Prompts};
use crate::registry::{self, Key, Registry};
use crate::supervisor::is_fatal;
use crate::tunnels::{Relink, RelinkError, Tunnels};
use crate::utf8::Utf8Stream;
use crate::{Lease, SharedError};
use easyssh_ssh::{
    AgentSource, ConnectParams, Error as SshError, ExitStatus, KnownHosts, MemoryIdentity, Session,
    ShellHandle, ShellOptions,
};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::mpsc;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Where to connect and how the dialogs name it.
#[derive(Debug, Clone)]
pub struct Target {
    /// The saved host's id; with the address and the jump chain it names the
    /// shared connection.
    pub host_id: String,
    /// The host's display name.
    pub label: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// A private key held in memory, tried before the password prompt.
    pub identity: Option<MemoryIdentity>,
    /// Jump hosts, in order; each hop is reached through the one before it.
    pub jump: Vec<Target>,
    /// The saved password, offered once at the password prompt.
    pub saved_password: Option<Arc<SavedPassword>>,
    /// The outbound proxy the first hop (the first jump host, or this host)
    /// is reached through. [`crate::with_saved_login`] adds its password.
    pub proxy: Option<easyssh_ssh::Proxy>,
}

impl Target {
    /// `user@host:port`, as the registry key and `engine.connections` show it.
    pub fn account(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }

    /// What identifies the shared connection to this target.
    pub fn key(&self) -> Key {
        Key {
            host_id: self.host_id.clone(),
            target: self.account(),
            jump: self.jump.iter().map(Target::account).collect(),
            via: self.proxy.as_ref().map(|proxy| proxy.display()),
            shard: 0,
        }
    }
}

/// Receives what a shell produces. Called from the engine's tasks, never
/// from the thread that called [`Engine::open_shell`].
pub trait ShellSink: Send + Sync + 'static {
    /// Terminal output as text; a character is never split across calls.
    fn data(&self, text: String);
    /// The shell ended (or the connection dropped), with its exit code if
    /// it reported one. Called once, by the default [`ShellSink::ended`].
    fn exit(&self, code: Option<u32>);
    /// The shell ended: with a status (`Code`), killed by a signal
    /// (`Signal`), or without a status because the connection dropped or the
    /// channel closed (`Unknown`). The engine calls this; sinks that only
    /// need the code keep the default, which calls [`ShellSink::exit`].
    fn ended(&self, status: ExitStatus) {
        self.exit(status.code());
    }
}

/// A connect failure with a stable code for the UI (`auth_failed`,
/// `hostkey_rejected`, `network`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The protocol error code for a connect failure, so callers can tell a
/// fallback (`unsupported`) from the user saying no (`cancelled`).
pub fn error_code(error: &SshError) -> &'static str {
    match error {
        SshError::Unsupported(_) => "unsupported",
        SshError::Cancelled(_) => "cancelled",
        SshError::Locked(_) => "vault_locked",
        SshError::AuthFailed { .. } => "auth_failed",
        SshError::HostKeyRejected { .. } => "hostkey_rejected",
        SshError::HostKeyMismatch { .. } => "hostkey_changed",
        SshError::HostKeyRevoked { .. } => "hostkey_revoked",
        SshError::Network(_) | SshError::Timeout(_) | SshError::Disconnected => "network",
        SshError::Proxy(_) => "proxy_error",
        SshError::NotFound(_) => "not_found",
        _ => "ssh_error",
    }
}

enum Command {
    Write(Vec<u8>),
    Resize(u32, u32),
    Close,
}

pub struct Engine {
    prompts: Arc<Prompts>,
    known_hosts: Arc<dyn KnownHosts>,
    registry: Registry<Session, SharedError>,
    files: Files,
    tunnels: Tunnels,
    events: Events,
    shells: Mutex<HashMap<String, mpsc::UnboundedSender<Command>>>,
    next: AtomicU64,
    /// SSH keepalive interval for new connections (`None` turns them off).
    keepalive: Mutex<Option<Duration>>,
    /// The app's language (`zh`, `en`) for `LANG` in new shells when the
    /// process has no UTF-8 locale, as on a phone. `None`: `en_US.UTF-8`.
    language: Mutex<Option<String>>,
}

fn fail(error: &SshError) -> OpenError {
    OpenError {
        code: error_code(error),
        message: error.to_string(),
    }
}

impl Engine {
    /// `events` carries transfer and tunnel events; the questions go through
    /// `prompts`, which has its own sink.
    pub fn new(
        prompts: Arc<Prompts>,
        known_hosts: Arc<dyn KnownHosts>,
        events: Events,
    ) -> Arc<Engine> {
        Arc::new(Engine {
            prompts,
            known_hosts,
            registry: Registry::new(registry::DEFAULT_LINGER),
            files: Files::default(),
            tunnels: Tunnels::default(),
            events,
            shells: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            keepalive: Mutex::new(Some(Duration::from_secs(60))),
            language: Mutex::new(None),
        })
    }

    /// Sets the keepalive interval used by connections opened from now on.
    /// A phone on a mobile network wants a shorter one than the desktop's 60 s
    /// so a dead path is noticed sooner; `None` turns keepalives off.
    pub fn set_keepalive(&self, interval: Option<Duration>) {
        *lock(&self.keepalive) = interval;
    }

    /// Sets the app's language for shells opened from now on; see
    /// [`easyssh_ssh::shell_env`].
    pub fn set_language(&self, language: Option<String>) {
        *lock(&self.language) = language;
    }

    pub fn prompts(&self) -> &Arc<Prompts> {
        &self.prompts
    }

    pub fn files(&self) -> &Files {
        &self.files
    }

    pub fn tunnels(&self) -> &Tunnels {
        &self.tunnels
    }

    pub fn events(&self) -> &Events {
        &self.events
    }

    /// Closes the host's shared connection now (a terminal found its socket
    /// untrustworthy: the app came back from the background). The next lease
    /// connects anew, and the files and forwards that open after it share that
    /// one; the ones on the old connection fail and are opened again.
    pub async fn forget_connection(&self, target: &Target) {
        self.registry.evict(&target.key()).await;
    }

    /// How long a connection stays up after its last user went away.
    pub fn set_linger(&self, linger: Duration) {
        self.registry.set_linger(linger);
    }

    fn params(&self, target: &Target) -> ConnectParams {
        // There is no agent on a phone, so authentication is a saved key,
        // keyboard-interactive or a password.
        let mut params = ConnectParams::new(target.host.clone(), target.port, target.user.clone())
            .known_hosts(self.known_hosts.clone())
            .keepalive(*lock(&self.keepalive))
            .agent(AgentSource::Disabled);
        if let Some(identity) = target.identity.clone() {
            params = params.memory_identity(identity);
        }
        params
    }

    async fn connect(&self, target: &Target) -> Result<Session, SharedError> {
        let mut hops: Vec<ConnectParams> = target.jump.iter().map(|hop| self.params(hop)).collect();
        let mut params = self.params(target);
        // Only the first hop is reached over the network; the others go
        // through the SSH connection before them.
        match hops.first_mut() {
            Some(first) => first.proxy = target.proxy.clone(),
            None => params.proxy = target.proxy.clone(),
        }
        let interaction = PromptInteraction {
            prompts: self.prompts.clone(),
            label: target.label.clone(),
            saved_passwords: target
                .jump
                .iter()
                .chain(std::iter::once(target))
                .filter_map(|t| t.saved_password.clone())
                .collect(),
        };
        Session::connect_via(&hops, params, &interaction)
            .await
            .map_err(Arc::new)
    }

    /// A lease on the host's shared connection, connecting (and asking the
    /// user whatever it takes) when there is none.
    pub async fn lease(&self, target: &Target) -> Result<Lease, OpenError> {
        self.registry
            .acquire(target.key(), || self.connect(target))
            .await
            .map_err(|error| fail(&error))
    }

    /// Like [`Engine::open_shell`], with the host's saved password to offer at
    /// the password prompt (a saved key travels in `target.identity`; see
    /// [`crate::saved_login`]). The login only matters for the connection that
    /// is made: a host already connected is reused as it is.
    pub async fn open_shell_with_login(
        &self,
        mut target: Target,
        saved_password: Option<Arc<SavedPassword>>,
        cols: u16,
        rows: u16,
        sink: Arc<dyn ShellSink>,
    ) -> Result<String, OpenError> {
        if saved_password.is_some() {
            target.saved_password = saved_password;
        }
        self.open_shell(target, cols, rows, sink).await
    }

    /// Connects (asking whatever it takes), opens a shell on a terminal of
    /// `cols` x `rows` and returns its id. Output and the end of the shell go
    /// to `sink`. The shell holds a lease on the host's connection for as
    /// long as it lives.
    pub async fn open_shell(
        &self,
        target: Target,
        cols: u16,
        rows: u16,
        sink: Arc<dyn ShellSink>,
    ) -> Result<String, OpenError> {
        let env = easyssh_ssh::local_shell_env(lock(&self.language).as_deref(), &[]);
        let lease = self.lease(&target).await?;
        let shell = lease
            .shell(ShellOptions {
                term: "xterm-256color".into(),
                cols: u32::from(cols.max(1)),
                rows: u32::from(rows.max(1)),
                env,
                ..ShellOptions::default()
            })
            .await
            .map_err(|error| fail(&error))?;

        let id = format!("m{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (commands, receiver) = mpsc::unbounded_channel();
        lock(&self.shells).insert(id.clone(), commands);
        tokio::spawn(pump(lease, shell, receiver, sink));
        Ok(id)
    }

    /// Opens a file session on the host's shared connection and returns its
    /// id and the home directory.
    pub async fn open_files(&self, target: &Target) -> Result<(String, String), OpenError> {
        let lease = self.lease(target).await?;
        self.files.open(lease).await.map_err(|error| fail(&error))
    }

    /// Puts a transfer on the transfer queue of the host `key`: a few run at
    /// once, each on a file session of the host's shared connection that is
    /// reused for the next transfer, the others wait. Returns the transfer id
    /// and whether it waits (`transfer.started` reports its turn).
    #[allow(clippy::too_many_arguments)]
    pub fn queue_transfer(
        self: &Arc<Self>,
        key: &str,
        target: Target,
        upload: bool,
        local: std::path::PathBuf,
        remote: String,
        recursive: bool,
        sink: Events,
    ) -> (String, bool) {
        let engine = self.clone();
        let open: crate::files::Opener =
            Box::pin(async move { engine.lease(&target).await.map_err(|error| error.message) });
        self.files
            .start_queued_transfer(key, open, upload, local, remote, recursive, sink)
    }

    /// Starts the forward of `tunnel` on the host's shared connection and
    /// returns the port that listens. Progress is reported as `tunnel.status`
    /// events.
    /// When the connection drops, the tunnel connects again by itself (see
    /// [`crate::tunnels`]).
    pub async fn start_tunnel(
        self: &Arc<Self>,
        tunnel: &easyssh_core::Tunnel,
        target: &Target,
    ) -> Result<u16, OpenError> {
        if !self.tunnels.begin(tunnel, &self.events).await {
            return Err(OpenError {
                code: "already_running",
                message: "the tunnel is already running".into(),
            });
        }
        let lease = match self.lease(target).await {
            Ok(lease) => lease,
            Err(error) => {
                self.tunnels
                    .failed_with(tunnel, &error.message, &self.events);
                return Err(error);
            }
        };
        let engine = Arc::downgrade(self);
        let again = target.clone();
        let relink: Relink = Arc::new(move || {
            let engine = engine.clone();
            let target = again.clone();
            Box::pin(async move {
                let engine = engine.upgrade().ok_or_else(|| RelinkError {
                    message: "the engine stopped".into(),
                    fatal: true,
                })?;
                engine.lease(&target).await.map_err(|error| RelinkError {
                    fatal: is_fatal(error.code),
                    message: error.message,
                })
            })
        });
        match self
            .tunnels
            .start(tunnel, lease, relink, self.events.clone())
            .await
        {
            Ok(port) => Ok(port),
            Err(error) => {
                self.tunnels.failed(tunnel, &error, &self.events);
                Err(fail(&error))
            }
        }
    }

    pub async fn stop_tunnel(&self, id: &str) {
        self.tunnels.stop(id, &self.events).await;
    }

    /// `engine.connections`: how many connections were opened and which are
    /// up now. A terminal, the files and a tunnel of one host share one.
    pub async fn connections(&self) -> Value {
        self.registry.report().await
    }

    fn send(&self, id: &str, command: Command) -> Result<(), String> {
        let shells = lock(&self.shells);
        let sender = shells
            .get(id)
            .ok_or_else(|| format!("unknown session: {id}"))?;
        sender
            .send(command)
            .map_err(|_| "the session has ended".to_owned())
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
        self.send(id, Command::Write(data.to_vec()))
    }

    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        self.send(
            id,
            Command::Resize(u32::from(cols.max(1)), u32::from(rows.max(1))),
        )
    }

    /// Ends the shell and its connection. Closing an ended session is fine.
    pub fn close(&self, id: &str) {
        if let Some(sender) = lock(&self.shells).remove(id) {
            let _ = sender.send(Command::Close);
        }
    }

    /// The ids of the shells that are still open.
    pub fn open_ids(&self) -> Vec<String> {
        let mut shells = lock(&self.shells);
        shells.retain(|_, sender| !sender.is_closed());
        shells.keys().cloned().collect()
    }

    /// Closes every shell (the app is going away).
    pub fn close_all(&self) {
        for (_, sender) in lock(&self.shells).drain() {
            let _ = sender.send(Command::Close);
        }
    }

    /// Stops tunnels and transfers, ends the shells and closes every
    /// connection.
    pub async fn shutdown(&self) {
        self.tunnels.stop_all().await;
        self.files.stop_all();
        self.close_all();
        self.registry.close_all().await;
    }
}

/// Moves bytes between the channel and the UI until either side ends.
async fn pump(
    lease: Lease,
    mut shell: ShellHandle,
    mut commands: mpsc::UnboundedReceiver<Command>,
    sink: Arc<dyn ShellSink>,
) {
    let mut decoder = Utf8Stream::default();
    loop {
        tokio::select! {
            output = shell.read() => match output {
                Some(bytes) => {
                    let text = decoder.push(&bytes);
                    if !text.is_empty() {
                        sink.data(text);
                    }
                }
                None => break,
            },
            command = commands.recv() => match command {
                Some(Command::Write(data)) => {
                    if let Err(error) = shell.write(&data).await {
                        tracing::debug!("write to the shell failed: {error}");
                        break;
                    }
                }
                Some(Command::Resize(cols, rows)) => {
                    if let Err(error) = shell.resize(cols, rows).await {
                        tracing::debug!("resize failed: {error}");
                    }
                }
                Some(Command::Close) | None => {
                    let _ = shell.close().await;
                    // The connection lingers for a moment (the file browser
                    // may still want it) and closes when nobody leased it.
                    drop(lease);
                    sink.ended(ExitStatus::Unknown);
                    return;
                }
            },
        }
    }
    let status = shell.wait_exit().await;
    drop(lease);
    sink.ended(status);
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_ssh::MemoryKnownHosts;

    fn engine() -> Arc<Engine> {
        Engine::new(
            Prompts::new(crate::events::no_events()),
            Arc::new(MemoryKnownHosts::new()),
            crate::events::no_events(),
        )
    }

    fn target(host_id: &str, jump: Vec<Target>) -> Target {
        Target {
            host_id: host_id.into(),
            label: "none".into(),
            host: "127.0.0.1".into(),
            port: 1,
            user: "me".into(),
            identity: None,
            jump,
            saved_password: None,
            proxy: None,
        }
    }

    #[test]
    fn a_connection_is_named_by_host_address_and_jump_chain() {
        let hop = Target {
            host: "bastion".into(),
            port: 2222,
            user: "jump".into(),
            ..target("b", vec![])
        };
        let plain = target("h", vec![]).key();
        let jumped = target("h", vec![hop]).key();
        assert_eq!(plain.target, "me@127.0.0.1:1");
        assert!(plain.jump.is_empty());
        assert_eq!(jumped.jump, vec!["jump@bastion:2222"]);
        assert_ne!(
            plain, jumped,
            "the same host through a jump host is another connection"
        );
        assert_eq!(plain, target("h", vec![]).key());
    }

    #[test]
    fn unknown_sessions_are_errors_and_closing_them_is_harmless() {
        let engine = engine();
        assert!(engine.write("nope", b"ls\r").is_err());
        assert!(engine.resize("nope", 80, 24).is_err());
        engine.close("nope");
        assert!(engine.open_ids().is_empty());
    }

    #[test]
    fn connect_failures_carry_a_code_the_ui_can_act_on() {
        assert_eq!(error_code(&SshError::Disconnected), "network");
        assert_eq!(error_code(&SshError::Cancelled("no".into())), "cancelled");
    }

    #[tokio::test]
    async fn a_host_that_refuses_the_connection_is_a_network_error() {
        struct Quiet;
        impl ShellSink for Quiet {
            fn data(&self, _: String) {}
            fn exit(&self, _: Option<u32>) {}
        }
        // Port 1 on the loopback address is closed.
        let error = engine()
            .open_shell(target("h", vec![]), 80, 24, Arc::new(Quiet))
            .await
            .unwrap_err();
        assert_eq!(error.code, "network", "{error}");
    }

    #[tokio::test]
    async fn a_failed_connect_leaves_no_connection_and_the_files_and_tunnels_see_the_same_error() {
        let engine = engine();
        let target = target("h", vec![]);
        let files = engine.open_files(&target).await.unwrap_err();
        assert_eq!(files.code, "network", "{files}");
        let tunnel = easyssh_core::Tunnel {
            id: "t".into(),
            name: "t".into(),
            host_id: "h".into(),
            kind: easyssh_core::TunnelKind::Local,
            bind_address: None,
            bind_port: 18080,
            target_host: Some("localhost".into()),
            target_port: Some(80),
        };
        let started = engine.start_tunnel(&tunnel, &target).await.unwrap_err();
        assert_eq!(started.code, "network", "{started}");
        // The forward shows as failed rather than starting forever.
        assert_eq!(engine.tunnels().list()["tunnels"][0]["state"], "failed");
        let report = engine.connections().await;
        assert_eq!(report["opened"], 0);
        assert_eq!(report["live"], 0);
        // A second start is allowed after a failure.
        assert!(engine.start_tunnel(&tunnel, &target).await.is_err());
        engine.shutdown().await;
    }
}
