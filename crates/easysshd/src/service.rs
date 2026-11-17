//! The daemon's command dispatcher: validated protocol commands in, replies
//! and events out. See `easyssh-proto` for the contract.

use crate::agent::validate_agent_socket;
use crate::askpass::{Hub, Prompt};
use crate::log::Logger;
use crate::server::{BoxFuture, Outbox, Service};
use crate::terminal::{TermSession, DEFAULT_SCROLLBACK};
use easyssh_core::security::{validate_alias, validate_connection};
use easyssh_core::{ConfigStore, Connection, Group, OpenSsh, SshInvocation};
use easyssh_proto as proto;
use easyssh_store::{open_app_store, Store, StoreError};
use portable_pty::CommandBuilder;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Hosts discovered in `~/.ssh/config` have no saved record, so their id is
/// derived from the alias (same convention as the v1 app).
pub const SSH_CONFIG_ID_PREFIX: &str = "ssh-config:";

/// How long the daemon stays up with no sessions running and no clients.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);

const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";

#[derive(Clone)]
pub struct DaemonConfig {
    /// The v1 configuration file. Hosts, groups and settings live in the
    /// SQLite database beside it (`easyssh.db`); this file is only migrated
    /// from, on first start, and kept as a backup.
    pub config_store: ConfigStore,
    /// Program for `session.open` with a null `host_id`; the user's default
    /// shell when `None`.
    pub local_shell: Option<PathBuf>,
    /// Scrollback per session, in bytes.
    pub scrollback_bytes: usize,
    pub idle_timeout: Duration,
}

impl DaemonConfig {
    pub fn for_current_user() -> std::io::Result<Self> {
        Ok(DaemonConfig {
            config_store: ConfigStore::default_path()
                .map_err(|e| std::io::Error::other(e.to_string()))?,
            local_shell: None,
            scrollback_bytes: DEFAULT_SCROLLBACK,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
        })
    }
}

/// Settings of a [`Daemon`] that are not part of [`DaemonConfig`].
#[derive(Clone, Default)]
pub struct DaemonExtras {
    /// Build version reported in `proto.hello`; this crate's version when
    /// `None`. The app passes its own, so a daemon from another build is
    /// recognised.
    pub build_version: Option<String>,
    /// Directory of `easysshd.log`; no log file when `None`.
    pub log_dir: Option<PathBuf>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

type Clients = Arc<Mutex<HashMap<u64, Outbox>>>;

pub struct Daemon {
    cfg: DaemonConfig,
    /// The app database. One connection, used one command at a time; other
    /// processes (the app's in-process fallback) open their own.
    store: Mutex<Store>,
    sessions: Mutex<HashMap<String, Arc<TermSession>>>,
    clients: Clients,
    askpass: Arc<Hub>,
    build_version: String,
    log: Arc<Logger>,
    /// Set by `daemon.shutdown_when_idle` (or a newer client's `proto.hello`):
    /// no new sessions, exit when none runs.
    draining: AtomicBool,
}

struct Failure {
    code: &'static str,
    message: String,
}

fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

enum Outcome {
    Ok(Value),
    /// The handler already queued the reply itself.
    Replied,
}

type Handled = Result<Outcome, Failure>;

fn parse<T: DeserializeOwned>(payload: Value) -> Result<T, Failure> {
    serde_json::from_value(payload).map_err(|e| fail("invalid_command", e.to_string()))
}

impl Daemon {
    pub async fn new(cfg: DaemonConfig) -> std::io::Result<Arc<Daemon>> {
        Self::new_with(cfg, DaemonExtras::default()).await
    }

    pub async fn new_with(cfg: DaemonConfig, extras: DaemonExtras) -> std::io::Result<Arc<Daemon>> {
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let notify_clients = clients.clone();
        let resolved_clients = clients.clone();
        let log = Arc::new(match &extras.log_dir {
            Some(dir) => Logger::open(dir),
            None => Logger::disabled(),
        });
        let askpass = Hub::bind(
            Arc::new(move |p: Prompt| {
                let payload = json!({
                    "id": p.id,
                    "host": p.host,
                    "prompt": p.prompt,
                    "confirm": p.confirm,
                });
                // Every attached client may answer; the first reply wins.
                let delivered = lock(&notify_clients)
                    .values()
                    .map(|outbox| outbox.event("askpass.prompt", payload.clone()))
                    .collect::<Vec<_>>();
                delivered.contains(&true)
            }),
            Arc::new(move |id: u64| {
                // Whoever answered, every other dialog for this prompt goes away.
                for outbox in lock(&resolved_clients).values() {
                    outbox.event("askpass.resolved", json!({"id": id}));
                }
            }),
        )
        .await?;
        let build_version = extras
            .build_version
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
        log.info(&format!(
            "daemon created, build {build_version}, schema {}",
            proto::SCHEMA_VERSION
        ));
        let opened = open_app_store(cfg.config_store.path())
            .map_err(|e| std::io::Error::other(format!("cannot open the database: {e}")))?;
        match (&opened.migration, &opened.migration_error) {
            (Some(report), _) => log.info(&format!(
                "migrated the v1 configuration: {} hosts, {} groups, {} snippets, {} tunnels",
                report.hosts.imported,
                report.groups.imported,
                report.snippets.imported,
                report.tunnels.imported
            )),
            (None, Some(error)) => {
                log.error(&format!("the v1 configuration was not migrated: {error}"))
            }
            (None, None) => {}
        }
        Ok(Arc::new(Daemon {
            cfg,
            store: Mutex::new(opened.store),
            sessions: Mutex::new(HashMap::new()),
            clients,
            askpass,
            build_version,
            log,
            draining: AtomicBool::new(false),
        }))
    }

    pub fn log(&self) -> &Logger {
        &self.log
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// Stop accepting sessions; [`Daemon::wait_idle`] completes once none runs.
    pub fn start_draining(&self, why: &str) {
        if !self.draining.swap(true, Ordering::SeqCst) {
            self.log.info(&format!(
                "draining ({why}): exits when no session is running, {} running now",
                self.running_sessions()
            ));
        }
    }

    /// Close every client connection; the daemon and its sessions stay up.
    /// For exercising clients that reconnect.
    pub fn drop_connections(&self) {
        for outbox in lock(&self.clients).values() {
            outbox.close();
        }
    }

    /// Send an event to every connected client.
    fn broadcast(&self, kind: &str, payload: Value) {
        for outbox in lock(&self.clients).values() {
            outbox.event(kind, payload.clone());
        }
    }

    /// Sessions whose process is still running.
    pub fn running_sessions(&self) -> usize {
        lock(&self.sessions)
            .values()
            .filter(|s| s.is_running())
            .count()
    }

    pub fn client_count(&self) -> usize {
        lock(&self.clients).len()
    }

    /// Completes once no client has been connected and no session has been
    /// running for `idle_timeout`, or, when draining, as soon as no session
    /// runs (connected clients do not keep a draining daemon alive).
    pub async fn wait_idle(&self) {
        let timeout = self.cfg.idle_timeout;
        let tick = (timeout / 10).clamp(Duration::from_millis(20), Duration::from_secs(5));
        let mut idle_since: Option<Instant> = None;
        loop {
            let draining = self.is_draining();
            tokio::time::sleep(if draining {
                tick.min(Duration::from_millis(250))
            } else {
                tick
            })
            .await;
            if self.is_draining() && self.running_sessions() == 0 {
                self.log.info("drained: no session is running");
                return;
            }
            if self.client_count() == 0 && self.running_sessions() == 0 {
                let since = *idle_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= timeout {
                    return;
                }
            } else {
                idle_since = None;
            }
        }
    }

    /// Kill every running session (daemon shutdown).
    pub fn shutdown(&self) {
        for session in lock(&self.sessions).values() {
            session.kill();
        }
    }

    fn session(&self, id: &str) -> Result<Arc<TermSession>, Failure> {
        lock(&self.sessions)
            .get(id)
            .cloned()
            .ok_or_else(|| fail("unknown_session", format!("no session {id}")))
    }

    async fn handle_message(&self, conn: u64, outbox: &Outbox, msg: Value) {
        if let Err(errors) = proto::validate_command(&msg) {
            let id = msg
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| uuid::Uuid::parse_str(s).is_ok())
                .unwrap_or(NIL_UUID);
            let kind = msg.get("kind").and_then(Value::as_str).unwrap_or_default();
            let code = if errors.iter().any(|e| e.starts_with("unknown command kind"))
                || (!kind.is_empty() && !proto::COMMANDS.iter().any(|(k, _, _)| *k == kind))
            {
                "unknown_command"
            } else {
                "invalid_command"
            };
            // The validation messages quote the offending values; log the code only.
            self.log
                .error(&format!("conn {conn}: rejected a message: {code}"));
            outbox.send(error_reply(id, code, &errors.join("; "))).await;
            return;
        }
        let id = msg["id"].as_str().unwrap_or(NIL_UUID).to_owned();
        let kind = msg["kind"].as_str().unwrap_or_default().to_owned();
        let payload = msg["payload"].clone();
        // Only the kind and the session id are logged, never the payload.
        // Keystrokes and resizes are too frequent to be worth a line.
        let sid = payload["session_id"].as_str().map(str::to_owned);
        if !matches!(kind.as_str(), "session.write" | "session.resize") {
            self.log.info(&format!(
                "conn {conn}: {kind}{}",
                sid.as_deref()
                    .map(|s| format!(" session {s}"))
                    .unwrap_or_default()
            ));
        }
        match self.dispatch(conn, outbox, &id, &kind, payload).await {
            Ok(Outcome::Ok(value)) => {
                outbox.send(json!({"id": id, "ok": value})).await;
            }
            Ok(Outcome::Replied) => {}
            Err(f) => {
                // The code only: messages can quote what the client sent.
                self.log
                    .error(&format!("conn {conn}: {kind} failed: {}", f.code));
                outbox.send(error_reply(&id, f.code, &f.message)).await;
            }
        }
    }

    async fn dispatch(
        &self,
        conn: u64,
        outbox: &Outbox,
        req_id: &str,
        kind: &str,
        payload: Value,
    ) -> Handled {
        let ok = |v: Value| Ok(Outcome::Ok(v));
        match kind {
            "proto.hello" => {
                let p: proto::ProtoHelloParams = parse(payload)?;
                if p.schema_version != u64::from(proto::SCHEMA_VERSION) {
                    // A newer client means this daemon is the stale one: let
                    // it finish its sessions and go. An older client is the
                    // one to update; the daemon stays.
                    if p.schema_version > u64::from(proto::SCHEMA_VERSION) {
                        self.start_draining("a client speaks a newer schema");
                    }
                    self.log.error(&format!(
                        "conn {conn}: schema mismatch, client {} daemon {}",
                        p.schema_version,
                        proto::SCHEMA_VERSION
                    ));
                    return Err(fail(
                        "schema_mismatch",
                        format!(
                            "this daemon speaks schema {} but the client speaks {}; \
                             {}",
                            proto::SCHEMA_VERSION,
                            p.schema_version,
                            if p.schema_version > u64::from(proto::SCHEMA_VERSION) {
                                "the daemon will exit when its sessions end, then start the app again"
                            } else {
                                "update the app"
                            }
                        ),
                    ));
                }
                if let Some(version) = &p.app_version {
                    if *version != self.build_version {
                        self.log.info(&format!(
                            "conn {conn}: client build {version}, daemon build {}",
                            self.build_version
                        ));
                    }
                }
                ok(json!({
                    "schema_version": proto::SCHEMA_VERSION,
                    "app_version": self.build_version,
                    "draining": self.is_draining(),
                }))
            }
            "daemon.shutdown_when_idle" => {
                let _: proto::DaemonShutdownWhenIdleParams = parse(payload)?;
                self.start_draining("asked by a client");
                ok(json!({"running_sessions": self.running_sessions()}))
            }
            "hosts.list" => {
                let store = lock(&self.store);
                let connections: Vec<Connection> = store
                    .list_hosts()
                    .map_err(store_failure)?
                    .into_iter()
                    .map(|record| record.item.connection)
                    .collect();
                let groups: Vec<Group> = store
                    .list_groups()
                    .map_err(store_failure)?
                    .into_iter()
                    .map(|record| record.item)
                    .collect();
                ok(json!({"connections": connections, "groups": groups}))
            }
            "hosts.save" => {
                let p: proto::HostsSaveParams = parse(payload)?;
                let id = self.save_host(p)?;
                self.broadcast("hosts.changed", json!({}));
                ok(json!({"id": id}))
            }
            "hosts.delete" => {
                let p: proto::HostsDeleteParams = parse(payload)?;
                self.delete_host(&p.id)?;
                self.broadcast("hosts.changed", json!({}));
                ok(Value::Null)
            }
            "known_hosts.list" => {
                let _: proto::KnownHostsListParams = parse(payload)?;
                let entries: Vec<Value> = lock(&self.store)
                    .list_known_hosts()
                    .map_err(store_failure)?
                    .iter()
                    .map(|record| record.item.summary_json())
                    .collect();
                ok(json!({"entries": entries}))
            }
            "known_hosts.import" => {
                let p: proto::KnownHostsImportParams = parse(payload)?;
                let path = PathBuf::from(&p.path);
                let size = std::fs::metadata(&path)
                    .map_err(|e| fail("io_error", format!("cannot read {}: {e}", p.path)))?
                    .len();
                if size > MAX_KNOWN_HOSTS_BYTES {
                    return Err(fail(
                        "invalid_command",
                        format!("{} is larger than {MAX_KNOWN_HOSTS_BYTES} bytes", p.path),
                    ));
                }
                let report = lock(&self.store)
                    .import_known_hosts_file(&path)
                    .map_err(store_failure)?;
                let skipped: Vec<Value> = report
                    .skipped
                    .iter()
                    .map(|s| json!({"line": s.line, "reason": s.reason}))
                    .collect();
                ok(json!({
                    "imported": report.imported,
                    "duplicates": report.duplicates,
                    "skipped": skipped,
                }))
            }
            "known_hosts.remove" => {
                let p: proto::KnownHostsRemoveParams = parse(payload)?;
                lock(&self.store)
                    .soft_delete_known_host(&p.id)
                    .map_err(store_failure)?;
                ok(Value::Null)
            }
            "session.open" => {
                let p: proto::SessionOpenParams = parse(payload)?;
                self.open_session(conn, outbox, req_id, p).await
            }
            "session.write" => {
                let p: proto::SessionWriteParams = parse(payload)?;
                let session = self.session(&p.session_id)?;
                let data = p.data.into_bytes();
                tokio::task::spawn_blocking(move || session.write(&data))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(|e| fail("session_ended", e))?;
                ok(Value::Null)
            }
            "session.resize" => {
                let p: proto::SessionResizeParams = parse(payload)?;
                self.session(&p.session_id)?
                    .resize(p.cols, p.rows)
                    .map_err(|e| fail("io_error", e))?;
                ok(Value::Null)
            }
            "session.close" => {
                let p: proto::SessionCloseParams = parse(payload)?;
                // Closing an unknown (already closed) session is not an error.
                if let Some(session) = lock(&self.sessions).remove(&p.session_id) {
                    session.kill();
                }
                ok(Value::Null)
            }
            "session.list" => {
                let mut sessions: Vec<Value> =
                    lock(&self.sessions).values().map(|s| s.info()).collect();
                sessions.sort_by(|a, b| a["session_id"].as_str().cmp(&b["session_id"].as_str()));
                ok(json!({"sessions": sessions}))
            }
            "session.attach" => {
                let p: proto::SessionAttachParams = parse(payload)?;
                let session = self.session(&p.session_id)?;
                session.attach(conn, outbox, Some(req_id), p.from_seq);
                Ok(Outcome::Replied)
            }
            "session.detach" => {
                let p: proto::SessionDetachParams = parse(payload)?;
                self.session(&p.session_id)?.detach(conn);
                ok(Value::Null)
            }
            "askpass.reply" => {
                let p: proto::AskpassReplyParams = parse(payload)?;
                self.askpass.reply(p.id, p.answer);
                ok(Value::Null)
            }
            "askpass.endpoint" => ok(json!({"endpoint": self.askpass.endpoint()})),
            other => Err(fail("unknown_command", format!("unknown command {other}"))),
        }
    }

    async fn open_session(
        &self,
        conn: u64,
        outbox: &Outbox,
        req_id: &str,
        p: proto::SessionOpenParams,
    ) -> Handled {
        if self.is_draining() {
            return Err(fail(
                "draining",
                "the daemon is shutting down and takes no new sessions",
            ));
        }
        let agent_sock = match p.ssh_auth_sock.as_deref().map(validate_agent_socket) {
            Some(Ok(path)) => path,
            Some(Err(reason)) => {
                // A stale variable must not stop a terminal from opening:
                // the session falls back to the daemon's own agent.
                self.log
                    .info(&format!("ignoring the client's SSH_AUTH_SOCK: {reason}"));
                None
            }
            None => None,
        };
        let connection = match p.host_id.as_deref() {
            Some(host_id) => Some(self.resolve_host(host_id)?),
            None => None,
        };
        let shell = self.cfg.local_shell.clone();
        let host_id = p.host_id.clone();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let scrollback = self.cfg.scrollback_bytes;
        let (cols, rows) = (p.cols, p.rows);
        let spawn_id = id.clone();
        let exit_log = self.log.clone();
        let on_exit: crate::terminal::ExitHook = Box::new(move |sid, code| {
            exit_log.info(&format!("session {sid} ended, exit code {code:?}"));
        });
        let session = tokio::task::spawn_blocking(move || {
            let command = command_for(shell.as_deref(), connection, agent_sock.as_deref())?;
            TermSession::spawn(spawn_id, host_id, command, cols, rows, scrollback, on_exit)
                .map_err(|e| fail("spawn_failed", e))
        })
        .await
        .map_err(|e| fail("internal", e.to_string()))??;
        lock(&self.sessions).insert(id.clone(), session.clone());
        // The reply must precede the session's events on this connection:
        // queue it first, then subscribe (which replays whatever the child
        // already printed).
        outbox
            .send(json!({"id": req_id, "ok": {"session_id": id}}))
            .await;
        session.attach(conn, outbox, None, None);
        Ok(Outcome::Replied)
    }

    /// A saved host, or the connection derived from a `~/.ssh/config` alias,
    /// exactly as the v1 app resolves ids.
    fn resolve_host(&self, host_id: &str) -> Result<Connection, Failure> {
        if let Some(alias) = host_id.strip_prefix(SSH_CONFIG_ID_PREFIX) {
            return Ok(Connection::alias(alias, alias));
        }
        lock(&self.store)
            .get_host(host_id)
            .map_err(store_failure)?
            .map(|record| record.item.connection)
            .ok_or_else(|| fail("unknown_host", format!("unknown host: {host_id}")))
    }

    fn save_host(&self, p: proto::HostsSaveParams) -> Result<String, Failure> {
        let value = serde_json::to_value(&p.connection)
            .map_err(|e| fail("invalid_command", e.to_string()))?;
        let mut connection: Connection =
            serde_json::from_value(value).map_err(|e| fail("invalid_command", e.to_string()))?;
        validate_connection(&connection).map_err(|e| fail("invalid_host", e.to_string()))?;
        if let Some(jump) = &connection.proxy_jump {
            validate_alias(jump).map_err(|e| fail("invalid_host", e.to_string()))?;
        }
        connection.updated_at = chrono::Utc::now();
        let group: Option<Group> = p
            .group
            .map(|group| {
                serde_json::to_value(&group)
                    .and_then(serde_json::from_value)
                    .map_err(|e| fail("invalid_command", e.to_string()))
            })
            .transpose()?;
        let id = connection.id.clone();
        lock(&self.store)
            .save_host(&connection, group.as_ref())
            .map_err(store_failure)?;
        Ok(id)
    }

    fn delete_host(&self, id: &str) -> Result<(), Failure> {
        lock(&self.store)
            .remove_host(id)
            .map(|_| ())
            .map_err(store_failure)
    }
}

/// Upper bound for a `known_hosts` file the daemon agrees to import.
const MAX_KNOWN_HOSTS_BYTES: u64 = 16 * 1024 * 1024;

fn store_failure(error: StoreError) -> Failure {
    match &error {
        StoreError::Invalid(_) => fail("invalid_host", error.to_string()),
        StoreError::Io(_) => fail("io_error", error.to_string()),
        _ => fail("store_error", error.to_string()),
    }
}

fn error_reply(id: &str, code: &str, message: &str) -> Value {
    json!({"id": id, "error": {"code": code, "message": message}})
}

impl Service for Daemon {
    fn connected(&self, id: u64, outbox: &Outbox) {
        lock(&self.clients).insert(id, outbox.clone());
        self.log.info(&format!("conn {id}: connected"));
    }

    fn disconnected(&self, id: u64) {
        lock(&self.clients).remove(&id);
        self.log.info(&format!("conn {id}: disconnected"));
        for session in lock(&self.sessions).values() {
            session.detach(id);
        }
    }

    fn handle<'a>(&'a self, id: u64, outbox: &'a Outbox, msg: Value) -> BoxFuture<'a, ()> {
        Box::pin(self.handle_message(id, outbox, msg))
    }
}

/// The command a session runs: the system `ssh` for a host (validated by
/// `easyssh-core`), or a local shell.
fn command_for(
    shell: Option<&std::path::Path>,
    connection: Option<Connection>,
    agent_sock: Option<&Path>,
) -> Result<CommandBuilder, Failure> {
    let mut command = match connection {
        Some(connection) => {
            let invocation = SshInvocation::for_connection(&OpenSsh, &connection)
                .map_err(|e| fail("invalid_host", e.to_string()))?;
            let mut command = CommandBuilder::new(invocation.executable);
            command.args(invocation.args);
            command
        }
        None => match shell {
            Some(path) => CommandBuilder::new(path),
            None => local_shell(),
        },
    };
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    if let Some(sock) = agent_sock {
        command.env("SSH_AUTH_SOCK", sock);
    }
    if let Some(home) = dirs::home_dir() {
        command.cwd(home);
    }
    Ok(command)
}

#[cfg(windows)]
fn local_shell() -> CommandBuilder {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["pwsh.exe", "powershell.exe"] {
        if let Some(found) = std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
        {
            let mut command = CommandBuilder::new(found);
            command.arg("-NoLogo");
            return command;
        }
    }
    CommandBuilder::new_default_prog()
}

#[cfg(not(windows))]
fn local_shell() -> CommandBuilder {
    CommandBuilder::new_default_prog()
}
