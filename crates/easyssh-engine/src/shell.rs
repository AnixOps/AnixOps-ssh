//! Terminal sessions on russh, owned by the app process.

use crate::prompts::{PromptInteraction, Prompts};
use crate::utf8::Utf8Stream;
use easyssh_ssh::{
    AgentSource, ConnectParams, Error as SshError, ExitStatus, KnownHosts, MemoryIdentity, Session,
    ShellHandle, ShellOptions,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::mpsc;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Where to connect and how the dialogs name it.
#[derive(Debug, Clone)]
pub struct Target {
    /// The host's display name.
    pub label: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    /// A private key held in memory, tried before the password prompt.
    pub identity: Option<MemoryIdentity>,
}

/// Receives what a shell produces. Called from the engine's tasks, never
/// from the thread that called [`Engine::open_shell`].
pub trait ShellSink: Send + Sync + 'static {
    /// Terminal output as text; a character is never split across calls.
    fn data(&self, text: String);
    /// The shell ended (or the connection dropped). Called once.
    fn exit(&self, code: Option<u32>);
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
    shells: Mutex<HashMap<String, mpsc::UnboundedSender<Command>>>,
    next: AtomicU64,
}

impl Engine {
    pub fn new(prompts: Arc<Prompts>, known_hosts: Arc<dyn KnownHosts>) -> Arc<Engine> {
        Arc::new(Engine {
            prompts,
            known_hosts,
            shells: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
        })
    }

    pub fn prompts(&self) -> &Arc<Prompts> {
        &self.prompts
    }

    /// Connects (asking whatever it takes), opens a shell on a terminal of
    /// `cols` x `rows` and returns its id. Output and the end of the shell go
    /// to `sink`. There is no agent on a phone, so authentication is a saved
    /// key, keyboard-interactive or a password.
    pub async fn open_shell(
        &self,
        target: Target,
        cols: u16,
        rows: u16,
        sink: Arc<dyn ShellSink>,
    ) -> Result<String, OpenError> {
        let mut params = ConnectParams::new(target.host.clone(), target.port, target.user.clone())
            .known_hosts(self.known_hosts.clone())
            .agent(AgentSource::Disabled);
        if let Some(identity) = target.identity.clone() {
            params = params.memory_identity(identity);
        }
        let interaction = PromptInteraction {
            prompts: self.prompts.clone(),
            label: target.label.clone(),
        };
        let fail = |error: SshError| OpenError {
            code: error_code(&error),
            message: error.to_string(),
        };
        let session = Session::connect(params, &interaction).await.map_err(fail)?;
        let shell = session
            .shell(ShellOptions {
                term: "xterm-256color".into(),
                cols: u32::from(cols.max(1)),
                rows: u32::from(rows.max(1)),
            })
            .await
            .map_err(fail)?;

        let id = format!("m{}", self.next.fetch_add(1, Ordering::Relaxed));
        let (commands, receiver) = mpsc::unbounded_channel();
        lock(&self.shells).insert(id.clone(), commands);
        tokio::spawn(pump(session, shell, receiver, sink));
        Ok(id)
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
}

/// Moves bytes between the channel and the UI until either side ends.
async fn pump(
    session: Session,
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
                    let _ = session.disconnect().await;
                    sink.exit(None);
                    return;
                }
            },
        }
    }
    let status = shell.wait_exit().await;
    let _ = session.disconnect().await;
    sink.exit(match status {
        ExitStatus::Code(code) => Some(code),
        _ => None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_ssh::MemoryKnownHosts;

    fn engine() -> Arc<Engine> {
        Engine::new(
            Prompts::new(Arc::new(|_, _| {})),
            Arc::new(MemoryKnownHosts::new()),
        )
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
        let target = Target {
            label: "none".into(),
            host: "127.0.0.1".into(),
            port: 1,
            user: "me".into(),
            identity: None,
        };
        let error = engine()
            .open_shell(target, 80, 24, Arc::new(Quiet))
            .await
            .unwrap_err();
        assert_eq!(error.code, "network", "{error}");
    }
}
