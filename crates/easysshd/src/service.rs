//! The daemon's command dispatcher: validated protocol commands in, replies
//! and events out. See `easyssh-proto` for the contract.

use crate::askpass::{Hub, Prompt};
use crate::server::{BoxFuture, Outbox, Service};
use crate::terminal::{TermSession, DEFAULT_SCROLLBACK};
use easyssh_core::security::{validate_alias, validate_connection};
use easyssh_core::{AppConfig, ConfigStore, Connection, OpenSsh, SshInvocation};
use easyssh_proto as proto;
use portable_pty::CommandBuilder;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
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
    /// Where hosts live. Defaults to the file the v1 app uses.
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

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

type Clients = Arc<Mutex<HashMap<u64, Outbox>>>;

pub struct Daemon {
    cfg: DaemonConfig,
    /// Serialises read-modify-write of the host file.
    store_lock: Mutex<()>,
    sessions: Mutex<HashMap<String, Arc<TermSession>>>,
    clients: Clients,
    askpass: Arc<Hub>,
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
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let notify_clients = clients.clone();
        let askpass = Hub::bind(Arc::new(move |p: Prompt| {
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
        }))
        .await?;
        Ok(Arc::new(Daemon {
            cfg,
            store_lock: Mutex::new(()),
            sessions: Mutex::new(HashMap::new()),
            clients,
            askpass,
        }))
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
    /// running for `idle_timeout`.
    pub async fn wait_idle(&self) {
        let timeout = self.cfg.idle_timeout;
        let tick = (timeout / 10).clamp(Duration::from_millis(20), Duration::from_secs(5));
        let mut idle_since: Option<Instant> = None;
        loop {
            tokio::time::sleep(tick).await;
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
            outbox.send(error_reply(id, code, &errors.join("; "))).await;
            return;
        }
        let id = msg["id"].as_str().unwrap_or(NIL_UUID).to_owned();
        let kind = msg["kind"].as_str().unwrap_or_default().to_owned();
        let payload = msg["payload"].clone();
        match self.dispatch(conn, outbox, &id, &kind, payload).await {
            Ok(Outcome::Ok(value)) => {
                outbox.send(json!({"id": id, "ok": value})).await;
            }
            Ok(Outcome::Replied) => {}
            Err(f) => {
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
            "proto.hello" => ok(json!({"schema_version": proto::SCHEMA_VERSION})),
            "hosts.list" => {
                let config = self.load_config()?;
                ok(json!({"connections": config.connections, "groups": config.groups}))
            }
            "hosts.save" => {
                let p: proto::HostsSaveParams = parse(payload)?;
                let id = self.save_host(p)?;
                ok(json!({"id": id}))
            }
            "hosts.delete" => {
                let p: proto::HostsDeleteParams = parse(payload)?;
                self.delete_host(&p.id)?;
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
        let store = self.cfg.config_store.clone();
        let shell = self.cfg.local_shell.clone();
        let host_id = p.host_id.clone();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let scrollback = self.cfg.scrollback_bytes;
        let (cols, rows) = (p.cols, p.rows);
        let spawn_id = id.clone();
        let session = tokio::task::spawn_blocking(move || {
            let command = command_for(&store, shell.as_deref(), host_id.as_deref())?;
            TermSession::spawn(spawn_id, host_id, command, cols, rows, scrollback)
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

    fn load_config(&self) -> Result<AppConfig, Failure> {
        self.cfg
            .config_store
            .load()
            .map_err(|e| fail("config_error", e.to_string()))
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
        let _guard = lock(&self.store_lock);
        let mut config = self.load_config()?;
        let id = connection.id.clone();
        match config.connections.iter().position(|c| c.id == id) {
            Some(index) => config.connections[index] = connection,
            None => config.connections.push(connection),
        }
        self.cfg
            .config_store
            .save(&config)
            .map_err(|e| fail("config_error", e.to_string()))?;
        Ok(id)
    }

    fn delete_host(&self, id: &str) -> Result<(), Failure> {
        let _guard = lock(&self.store_lock);
        let mut config = self.load_config()?;
        config.connections.retain(|c| c.id != id);
        config.recent_connection_ids.retain(|recent| recent != id);
        self.cfg
            .config_store
            .save(&config)
            .map_err(|e| fail("config_error", e.to_string()))
    }
}

fn error_reply(id: &str, code: &str, message: &str) -> Value {
    json!({"id": id, "error": {"code": code, "message": message}})
}

impl Service for Daemon {
    fn connected(&self, id: u64, outbox: &Outbox) {
        lock(&self.clients).insert(id, outbox.clone());
    }

    fn disconnected(&self, id: u64) {
        lock(&self.clients).remove(&id);
        for session in lock(&self.sessions).values() {
            session.detach(id);
        }
    }

    fn handle<'a>(&'a self, id: u64, outbox: &'a Outbox, msg: Value) -> BoxFuture<'a, ()> {
        Box::pin(self.handle_message(id, outbox, msg))
    }
}

/// Resolves a host id into a saved or derived connection, exactly as the v1
/// app does.
fn resolve(config: &AppConfig, host_id: &str) -> Result<Connection, Failure> {
    if let Some(alias) = host_id.strip_prefix(SSH_CONFIG_ID_PREFIX) {
        return Ok(Connection::alias(alias, alias));
    }
    config
        .connections
        .iter()
        .find(|connection| connection.id == host_id)
        .cloned()
        .ok_or_else(|| fail("unknown_host", format!("unknown host: {host_id}")))
}

/// The command a session runs: the system `ssh` for a host (validated by
/// `easyssh-core`), or a local shell.
fn command_for(
    store: &ConfigStore,
    shell: Option<&std::path::Path>,
    host_id: Option<&str>,
) -> Result<CommandBuilder, Failure> {
    let mut command = match host_id {
        Some(host_id) => {
            let config = store
                .load()
                .map_err(|e| fail("config_error", e.to_string()))?;
            let connection = resolve(&config, host_id)?;
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
