//! The daemon's command dispatcher: validated protocol commands in, replies
//! and events out. See `easyssh-proto` for the contract.

use crate::agent::validate_agent_socket;
use crate::askpass::{Hub, Prompt};
use crate::engine::interact::Broadcast;
use crate::engine::{self, plan, Engine, KnownHostsPaths};
use crate::log::Logger;
use crate::server::{BoxFuture, Outbox, Service};
use crate::syncsvc::{SyncFailure, SyncOptions, SyncService};
use crate::terminal::{SpawnOptions, TermSession, DEFAULT_SCROLLBACK};
use base64::Engine as _;
use easyssh_core::security::{validate_alias, validate_connection};
use easyssh_core::{
    ConfigStore, Connection, ConnectionTarget, Group, OpenSsh, SerialSettings, SshInvocation,
};
use easyssh_engine::prompts::parse_decision;
use easyssh_proto as proto;
use easyssh_sessionlog::{LogSettings, LogStore, KEY_INFO};
use easyssh_ssh::Error as SshError;
use easyssh_store::import::{
    import_apply, import_preview, read_import_file, ApplyRequest, ImportPreview, PreviewRequest,
    MAX_IMPORT_BYTES,
};
use easyssh_store::{db_path_for, open_app_store_with, SecretFlags, Store, StoreError};
use easyssh_vault::{Binding, KeyFile, StoreAccess, Vault, VaultError, VaultSecrets};
use portable_pty::CommandBuilder;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
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
    /// The vault for saved passwords and keys. The operating system keychain
    /// when `None`; tests pass one over a `MemoryKeyStore`.
    pub vault: Option<Arc<Vault>>,
    /// The per-user runtime directory; saved private keys are written to its
    /// `keys` folder for the life of an `ssh` process. The default runtime
    /// directory when `None`.
    pub runtime_dir: Option<PathBuf>,
    /// The program OpenSSH runs for `SSH_ASKPASS`: this executable when
    /// `None` (it calls `askpass::run_helper_if_requested` first thing).
    pub askpass_helper: Option<PathBuf>,
    /// Timing and key-stretching settings of the sync service (production
    /// defaults when unset; tests use cheap ones).
    pub sync: SyncOptions,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

type Clients = Arc<Mutex<HashMap<u64, Outbox>>>;

fn os_vault() -> Vault {
    let vault = Vault::os();
    easyssh_unlock::attach_os_providers(&vault);
    vault
}

/// Locks the vault when it has been idle for the auto-lock time and tells
/// the clients. Ends with the vault.
fn spawn_auto_lock(vault: &Arc<Vault>, clients: &Clients, log: &Arc<Logger>) {
    let vault = Arc::downgrade(vault);
    let clients = clients.clone();
    let log = log.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(20));
        loop {
            tick.tick().await;
            let Some(vault) = vault.upgrade() else { return };
            if vault.tick() {
                log.info("vault locked: idle");
                for outbox in lock(&clients).values() {
                    outbox.event("vault.changed", json!({"unlocked": false}));
                }
            }
        }
    });
}

pub struct Daemon {
    /// For tasks that outlive one request (connecting may wait for the user).
    me: Weak<Daemon>,
    engine: Arc<Engine>,
    cfg: DaemonConfig,
    /// The app database. One connection, used one command at a time; other
    /// processes (the app's in-process fallback) open their own.
    store: Arc<Mutex<Store>>,
    vault: Arc<Vault>,
    key_dir_root: PathBuf,
    askpass_helper: Option<PathBuf>,
    sessions: Mutex<HashMap<String, Arc<TermSession>>>,
    /// Encrypted session logs, in the data directory beside the database.
    logs: LogStore,
    clients: Clients,
    askpass: Arc<Hub>,
    sync: Arc<SyncService>,
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
                let mut payload = json!({
                    "id": p.id,
                    "host": p.host,
                    "prompt": p.prompt,
                    "confirm": p.confirm,
                    "rememberable": p.rememberable,
                    "vault_locked": p.vault_locked,
                });
                if let Some(kind) = p.kind {
                    payload["kind"] = json!(kind);
                }
                if let Some(echo) = p.echo {
                    payload["echo"] = json!(echo);
                }
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
        let broadcast_clients = clients.clone();
        let broadcast: Broadcast = Arc::new(move |kind: &str, payload: Value| {
            lock(&broadcast_clients)
                .values()
                .filter(|outbox| outbox.event(kind, payload.clone()))
                .count()
        });
        let known_hosts = KnownHostsPaths {
            app: cfg
                .config_store
                .path()
                .parent()
                .map(|dir| dir.join("known_hosts"))
                .unwrap_or_else(|| PathBuf::from("known_hosts")),
            user: dirs::home_dir().map(|home| home.join(".ssh").join("known_hosts")),
            system: engine::known_hosts::system_known_hosts(),
        };
        let engine = Engine::new(
            askpass.clone(),
            broadcast,
            known_hosts,
            engine::registry::DEFAULT_LINGER,
        );
        let vault = extras.vault.clone().unwrap_or_else(|| Arc::new(os_vault()));
        let opened = open_app_store_with(cfg.config_store.path(), vault.cipher())
            .map_err(|e| std::io::Error::other(format!("cannot open the database: {e}")))?;
        vault.load_settings(&opened.store);
        vault.set_unlock_prompt(Arc::new(crate::unlock_prompt::DaemonUnlockPrompt::new(
            askpass.clone(),
            tokio::runtime::Handle::current(),
        )));
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
        let logs = LogStore::in_data_dir(
            db_path_for(cfg.config_store.path())
                .parent()
                .unwrap_or_else(|| Path::new(".")),
        );
        // Retention: old logs go when the daemon starts.
        let retention = opened
            .store
            .get_setting_as::<LogSettings>(LogSettings::KEY)
            .ok()
            .flatten()
            .unwrap_or_default()
            .retention_days;
        let cleaned = logs.cleanup(retention, std::time::SystemTime::now());
        if cleaned.deleted > 0 {
            log.info(&format!(
                "deleted {} session logs older than {retention} days",
                cleaned.deleted
            ));
        }
        let store = Arc::new(Mutex::new(opened.store));
        let access: Arc<dyn StoreAccess> = store.clone();
        let secrets = Arc::new(VaultSecrets::new(vault.clone(), access));
        askpass.set_answers(secrets.clone());
        engine.set_secrets(secrets);
        match engine.set_store(store.clone()) {
            Ok(0) => {}
            Ok(count) => log.info(&format!(
                "moved {count} host keys of the built-in engine into the database"
            )),
            Err(error) => log.error(&format!(
                "the old known_hosts file was not imported: {error}"
            )),
        }
        spawn_auto_lock(&vault, &clients, &log);
        let sync_clients = clients.clone();
        let sync = SyncService::start(
            store.clone(),
            vault.clone(),
            extras.sync.clone(),
            Arc::new(move |kind: &str, payload: Value| {
                for outbox in lock(&sync_clients).values() {
                    outbox.event(kind, payload.clone());
                }
            }),
        );
        Ok(Arc::new_cyclic(|me| Daemon {
            me: me.clone(),
            engine,
            cfg,
            store,
            vault,
            key_dir_root: extras
                .runtime_dir
                .clone()
                .unwrap_or_else(crate::runtime::default_runtime_dir),
            askpass_helper: extras.askpass_helper.clone(),
            sessions: Mutex::new(HashMap::new()),
            logs,
            clients,
            askpass,
            sync,
            build_version,
            log,
            draining: AtomicBool::new(false),
        }))
    }

    /// Where the built-in engine keeps and reads host keys (default: a
    /// `known_hosts` file beside the host list, and `~/.ssh/known_hosts`,
    /// `known_hosts2` and the system-wide files as read-only fallbacks).
    /// Set this way, the system-wide files are not read.
    pub fn set_known_hosts(&self, app: PathBuf, user: Option<PathBuf>) {
        self.engine.set_known_hosts(KnownHostsPaths {
            app,
            user,
            system: Vec::new(),
        });
    }

    /// How long a shared connection stays up after its last user left
    /// (default 30 seconds).
    pub fn set_connection_linger(&self, linger: Duration) {
        self.engine.set_linger(linger);
    }

    pub fn log(&self) -> &Logger {
        &self.log
    }

    pub fn is_draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }

    /// The vault of this daemon.
    pub fn vault(&self) -> &Arc<Vault> {
        &self.vault
    }

    fn vault_status(&self) -> Value {
        let status = self.vault.status();
        json!({
            "passphrase_set": status.passphrase_set,
            "unlocked": status.unlocked,
            "auto_lock_minutes": status.auto_lock_minutes,
            "keychain_available": status.keychain_available,
            "keychain_error": status.keychain_error,
            "unlock_methods": status
                .unlock_methods
                .iter()
                .map(|m| json!({
                    "name": m.name,
                    "available": m.available,
                    "enabled": m.enabled,
                    "items": m
                        .items
                        .iter()
                        .map(|i| json!({"id": i.id, "label": i.label}))
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
        })
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
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let engine = self.engine.clone();
            runtime.spawn(async move { engine.shutdown().await });
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
                if matches!(
                    kind.as_str(),
                    "hosts.save"
                        | "hosts.delete"
                        | "hosts.import_apply"
                        | "hosts.move_vault"
                        | "hosts.history_restore"
                        | "known_hosts.import"
                        | "known_hosts.remove"
                ) {
                    self.sync.kick();
                }
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
            "hosts.import_preview" => {
                let p: ImportPreviewParams = parse(payload)?;
                let preview = self.import_preview(p)?;
                ok(serde_json::to_value(preview).map_err(|e| fail("internal", e.to_string()))?)
            }
            "hosts.import_apply" => {
                let p: ApplyRequest = parse(payload)?;
                let outcome = import_apply(&lock(&self.store), &p).map_err(import_failure)?;
                if outcome.changed() {
                    self.broadcast("hosts.changed", json!({}));
                }
                ok(serde_json::to_value(outcome).map_err(|e| fail("internal", e.to_string()))?)
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
            "serial.ports" => {
                let _: proto::SerialPortsParams = parse(payload)?;
                let ports = tokio::task::spawn_blocking(crate::serial::list_ports)
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(|e| fail("io_error", e))?;
                ok(json!({
                    "ports": ports
                        .into_iter()
                        .map(|port| json!({"path": port.path, "description": port.description}))
                        .collect::<Vec<_>>()
                }))
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
                self.askpass
                    .reply(p.id, p.answer, p.remember.unwrap_or(false));
                ok(Value::Null)
            }
            "askpass.unlock" => {
                let p: proto::AskpassUnlockParams = parse(payload)?;
                let vault = self.vault.clone();
                let passphrase = zeroize::Zeroizing::new(p.passphrase);
                tokio::task::spawn_blocking(move || vault.unlock(&passphrase))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(vault_failure)?;
                self.broadcast("vault.changed", json!({"unlocked": true}));
                let hub = self.askpass.clone();
                let answered = tokio::task::spawn_blocking(move || hub.answer_from_vault(p.id))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?;
                if !answered {
                    // The vault is open now; the user types the answer as usual.
                    self.log
                        .info("askpass.unlock: no saved answer for the prompt");
                }
                ok(Value::Null)
            }
            "askpass.bind" => {
                let p: proto::AskpassBindParams = parse(payload)?;
                let binding = {
                    let store = lock(&self.store);
                    match store.get_host(&p.host_id).map_err(store_failure)? {
                        Some(record) => {
                            let flags =
                                store.host_secret_flags(&p.host_id).map_err(store_failure)?;
                            let mut binding =
                                Binding::for_connection(&record.item.connection, None);
                            binding.passphrase_expected = flags.passphrase;
                            Some(binding)
                        }
                        None => None,
                    }
                };
                // An unknown host (a `~/.ssh/config` alias) simply has nothing saved.
                if let Some(binding) = binding {
                    self.askpass.bind_process(&p.context, binding);
                }
                ok(Value::Null)
            }
            "askpass.unbind" => {
                let p: proto::AskpassUnbindParams = parse(payload)?;
                self.askpass.unbind_process(&p.context);
                ok(Value::Null)
            }
            "vault.status" => {
                let _: proto::VaultStatusParams = parse(payload)?;
                ok(self.vault_status())
            }
            "vault.unlock" => {
                let p: proto::VaultUnlockParams = parse(payload)?;
                let vault = self.vault.clone();
                let passphrase = zeroize::Zeroizing::new(p.passphrase);
                tokio::task::spawn_blocking(move || vault.unlock(&passphrase))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(vault_failure)?;
                self.broadcast("vault.changed", json!({"unlocked": true}));
                self.sync.restore();
                ok(self.vault_status())
            }
            "vault.unlock_with" => {
                let p: proto::VaultUnlockWithParams = parse(payload)?;
                let vault = self.vault.clone();
                tokio::task::spawn_blocking(move || vault.unlock_with(&p.method))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(vault_failure)?;
                self.broadcast("vault.changed", json!({"unlocked": true}));
                self.sync.restore();
                ok(self.vault_status())
            }
            "vault.enable_unlock" => {
                let p: proto::VaultEnableUnlockParams = parse(payload)?;
                let vault = self.vault.clone();
                let passphrase = zeroize::Zeroizing::new(p.passphrase);
                tokio::task::spawn_blocking(move || vault.enable_unlock(&p.method, &passphrase))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(vault_failure)?;
                self.broadcast("vault.changed", json!({"unlocked": true}));
                ok(self.vault_status())
            }
            "vault.add_unlock_item" => {
                let p: proto::VaultAddUnlockItemParams = parse(payload)?;
                let vault = self.vault.clone();
                let passphrase = zeroize::Zeroizing::new(p.passphrase);
                let label = p.label.unwrap_or_default();
                tokio::task::spawn_blocking(move || {
                    vault.add_unlock_item(&p.method, &passphrase, &label)
                })
                .await
                .map_err(|e| fail("internal", e.to_string()))?
                .map_err(vault_failure)?;
                self.broadcast("vault.changed", json!({"unlocked": true}));
                ok(self.vault_status())
            }
            "vault.remove_unlock_item" => {
                let p: proto::VaultRemoveUnlockItemParams = parse(payload)?;
                let vault = self.vault.clone();
                tokio::task::spawn_blocking(move || vault.remove_unlock_item(&p.method, &p.id))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(vault_failure)?;
                ok(self.vault_status())
            }
            "vault.disable_unlock" => {
                let p: proto::VaultDisableUnlockParams = parse(payload)?;
                let vault = self.vault.clone();
                tokio::task::spawn_blocking(move || vault.disable_unlock(&p.method))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(vault_failure)?;
                ok(self.vault_status())
            }
            "vault.lock" => {
                let _: proto::VaultLockParams = parse(payload)?;
                self.vault.lock();
                self.broadcast("vault.changed", json!({"unlocked": false}));
                ok(self.vault_status())
            }
            "vault.set_passphrase" => {
                let p: proto::VaultSetPassphraseParams = parse(payload)?;
                let vault = self.vault.clone();
                let new = zeroize::Zeroizing::new(p.passphrase);
                let current = p.current.map(zeroize::Zeroizing::new);
                tokio::task::spawn_blocking(move || match current {
                    Some(current) => vault.change_passphrase(&current, &new),
                    None => vault.set_passphrase(&new),
                })
                .await
                .map_err(|e| fail("internal", e.to_string()))?
                .map_err(vault_failure)?;
                self.broadcast(
                    "vault.changed",
                    json!({"unlocked": self.vault.is_unlocked()}),
                );
                ok(self.vault_status())
            }
            "vault.remove_passphrase" => {
                let p: proto::VaultRemovePassphraseParams = parse(payload)?;
                let vault = self.vault.clone();
                let passphrase = zeroize::Zeroizing::new(p.passphrase);
                tokio::task::spawn_blocking(move || vault.remove_passphrase(&passphrase))
                    .await
                    .map_err(|e| fail("internal", e.to_string()))?
                    .map_err(vault_failure)?;
                self.broadcast("vault.changed", json!({"unlocked": true}));
                ok(self.vault_status())
            }
            "vault.set_auto_lock" => {
                let p: proto::VaultSetAutoLockParams = parse(payload)?;
                let minutes = u32::try_from(p.minutes)
                    .map_err(|_| fail("invalid_command", "minutes out of range"))?;
                self.vault
                    .save_auto_lock(&lock(&self.store), minutes)
                    .map_err(vault_failure)?;
                ok(self.vault_status())
            }
            "vault.forget_all" => {
                let _: proto::VaultForgetAllParams = parse(payload)?;
                let hosts = self
                    .vault
                    .forget_all(&lock(&self.store))
                    .map_err(vault_failure)?;
                self.log
                    .info(&format!("forgot the saved secrets of {hosts} hosts"));
                self.broadcast("hosts.changed", json!({}));
                ok(json!({"hosts": hosts}))
            }
            "secrets.status" => {
                let p: proto::SecretsStatusParams = parse(payload)?;
                let flags = self
                    .vault
                    .flags(&lock(&self.store), &p.host_id)
                    .map_err(vault_failure)?;
                ok(flags_json(flags))
            }
            "secrets.set" => {
                let p: proto::SecretsSetParams = parse(payload)?;
                let host_id = p.host_id.clone();
                let mut update =
                    easyssh_vault::secrets_update(p.password, p.private_key, p.passphrase)
                        .map_err(|e| fail(e.code, e.message))?;
                update.proxy_password = easyssh_vault::proxy_password_change(p.proxy_password)
                    .map_err(|e| fail(e.code, e.message))?;
                let (vault, store) = (self.vault.clone(), self.store.clone());
                let id = host_id.clone();
                let flags = tokio::task::spawn_blocking(move || {
                    let store = lock(&store);
                    vault.set_host_secrets(&store, &id, &update)?;
                    vault.flags(&store, &id)
                })
                .await
                .map_err(|e| fail("internal", e.to_string()))?
                .map_err(vault_failure)?;
                self.broadcast("hosts.changed", json!({}));
                ok(flags_json(flags))
            }
            "sync.status" => {
                let _: proto::SyncStatusParams = parse(payload)?;
                self.sync.restore();
                ok(self.sync.status_json())
            }
            "sync.signup" => {
                let p: proto::SyncSignupParams = parse(payload)?;
                let status = self
                    .sync
                    .signup(
                        &p.server_url,
                        &p.username,
                        &p.passphrase,
                        p.device_name.as_deref(),
                    )
                    .await
                    .map_err(sync_failure)?;
                self.broadcast("hosts.changed", json!({}));
                ok(status)
            }
            "sync.signin" => {
                let p: proto::SyncSigninParams = parse(payload)?;
                let status = self
                    .sync
                    .signin(
                        &p.server_url,
                        &p.username,
                        &p.passphrase,
                        p.device_name.as_deref(),
                    )
                    .await
                    .map_err(sync_failure)?;
                self.broadcast("hosts.changed", json!({}));
                ok(status)
            }
            "sync.signout" => {
                let _: proto::SyncSignoutParams = parse(payload)?;
                ok(self.sync.signout().await.map_err(sync_failure)?)
            }
            "sync.now" => {
                let _: proto::SyncNowParams = parse(payload)?;
                ok(self.sync.sync_now().await)
            }
            "sync.devices.list" => {
                let _: proto::SyncDevicesListParams = parse(payload)?;
                ok(self.sync.devices_list().await.map_err(sync_failure)?)
            }
            "sync.devices.revoke" => {
                let p: proto::SyncDevicesRevokeParams = parse(payload)?;
                self.sync
                    .devices_revoke(&p.device_id)
                    .await
                    .map_err(sync_failure)?;
                ok(Value::Null)
            }
            "vaults.list" => {
                let _: proto::VaultsListParams = parse(payload)?;
                ok(self.sync.vaults_list().map_err(sync_failure)?)
            }
            "vault.create_shared" => {
                let p: proto::VaultCreateSharedParams = parse(payload)?;
                ok(self
                    .sync
                    .create_shared(&p.name)
                    .await
                    .map_err(sync_failure)?)
            }
            "vault.members.list" => {
                let p: proto::VaultMembersListParams = parse(payload)?;
                ok(self
                    .sync
                    .members_list(&p.vault_id)
                    .await
                    .map_err(sync_failure)?)
            }
            "vault.members.lookup" => {
                let p: proto::VaultMembersLookupParams = parse(payload)?;
                ok(self
                    .sync
                    .members_lookup(&p.username)
                    .await
                    .map_err(sync_failure)?)
            }
            "vault.members.add" => {
                let p: MembersAdd = parse(payload)?;
                ok(self
                    .sync
                    .members_add(&p.vault_id, &p.username, p.role, &p.fingerprint)
                    .await
                    .map_err(sync_failure)?)
            }
            "vault.members.remove" => {
                let p: proto::VaultMembersRemoveParams = parse(payload)?;
                self.sync
                    .members_remove(&p.vault_id, &p.account_id)
                    .await
                    .map_err(sync_failure)?;
                ok(Value::Null)
            }
            "vault.members.role" => {
                let p: MembersRole = parse(payload)?;
                self.sync
                    .members_role(&p.vault_id, &p.account_id, p.role)
                    .await
                    .map_err(sync_failure)?;
                ok(Value::Null)
            }
            "hosts.move_vault" => {
                let p: proto::HostsMoveVaultParams = parse(payload)?;
                // Saved secrets move with the host; that needs the vault open.
                let _ = self.vault.ensure_unlocked();
                let id = lock(&self.store)
                    .move_host_to_vault(&p.id, &p.vault_id)
                    .map_err(store_failure)?;
                self.broadcast("hosts.changed", json!({}));
                ok(json!({"id": id}))
            }
            "hosts.vaults" => {
                let _: proto::HostsVaultsParams = parse(payload)?;
                let hosts = lock(&self.store).list_hosts().map_err(store_failure)?;
                let map: serde_json::Map<String, Value> = hosts
                    .into_iter()
                    .filter(|r| r.meta.vault_id != easyssh_store::PERSONAL_VAULT_ID)
                    .map(|r| (r.meta.id, Value::String(r.meta.vault_id)))
                    .collect();
                ok(json!({"host_vaults": map}))
            }
            "hosts.history" => {
                let p: proto::HostsHistoryParams = parse(payload)?;
                let entries: Vec<Value> = lock(&self.store)
                    .history(&p.id)
                    .map_err(store_failure)?
                    .into_iter()
                    .filter(|e| e.record_table == "hosts" && e.field != "deleted")
                    .map(|e| {
                        json!({
                            "seq": e.seq,
                            "field": e.field,
                            "value": e.old_value.unwrap_or(Value::Null),
                            "changed_at": e.changed_at.to_rfc3339(),
                            "device": e.device,
                        })
                    })
                    .collect();
                ok(json!({"entries": entries}))
            }
            "hosts.history_restore" => {
                let p: proto::HostsHistoryRestoreParams = parse(payload)?;
                lock(&self.store)
                    .restore_host_field(&p.id, p.seq)
                    .map_err(store_failure)?;
                self.broadcast("hosts.changed", json!({}));
                ok(Value::Null)
            }
            "askpass.endpoint" => ok(json!({"endpoint": self.askpass.endpoint()})),
            "hostkey.reply" => {
                let p: proto::HostkeyReplyParams = parse(payload)?;
                let decision = serde_json::to_value(p.decision)
                    .ok()
                    .and_then(|v| v.as_str().map(parse_decision))
                    .unwrap_or(easyssh_ssh::Decision::Reject);
                self.engine.hostkeys().reply(p.id, decision);
                ok(Value::Null)
            }
            "engine.resolve" => {
                let p: proto::EngineResolveParams = parse(payload)?;
                let me = self.upgrade()?;
                self.background(outbox, req_id, async move {
                    let (_, resolution) = me.resolution(&p.host_id).await?;
                    Ok(json!({
                        "engine": resolution.choice.engine,
                        "reason": resolution.choice.reason,
                    }))
                })
            }
            "engine.connections" => {
                let _: proto::EngineConnectionsParams = parse(payload)?;
                ok(self.engine.connections().await)
            }
            "monitor.sample" => {
                let p: proto::MonitorSampleParams = parse(payload)?;
                let me = self.upgrade()?;
                self.background(outbox, req_id, async move {
                    let lease = me.monitor_lease(&p.host_id).await?;
                    let sample = easyssh_engine::monitor::sample(&lease, p.detail.unwrap_or(false))
                        .await
                        .map_err(ssh_fail)?;
                    Ok(json!({"output": sample.output, "latency_ms": sample.latency_ms}))
                })
            }
            "monitor.signal" => {
                let p: proto::MonitorSignalParams = parse(payload)?;
                let signal = p.signal.to_string();
                let pid = u32::try_from(p.pid.get())
                    .map_err(|_| fail("invalid_command", "pid out of range"))?;
                let me = self.upgrade()?;
                self.background(outbox, req_id, async move {
                    let lease = me.monitor_lease(&p.host_id).await?;
                    easyssh_engine::monitor::signal(&lease, pid, &signal)
                        .await
                        .map_err(|error| fail("signal_failed", error.to_string()))?;
                    Ok(Value::Null)
                })
            }
            "sftp.open" => {
                let p: proto::SftpOpenParams = parse(payload)?;
                let me = self.upgrade()?;
                self.background(outbox, req_id, async move {
                    let plan = me.russh_plan(&p.host_id).await?;
                    let lease = me.engine.lease(&plan, None).await.map_err(ssh_shared)?;
                    let (sftp_id, home) = me.engine.files.open(lease).await.map_err(ssh_fail)?;
                    Ok(json!({"sftp_id": sftp_id, "home": home}))
                })
            }
            "sftp.list" => {
                let p: proto::SftpListParams = parse(payload)?;
                let entry = self.sftp(&p.sftp_id)?;
                self.background(outbox, req_id, async move {
                    entry.list(&p.path).await.map_err(ssh_fail)
                })
            }
            "sftp.mkdir" => {
                let p: proto::SftpMkdirParams = parse(payload)?;
                let entry = self.sftp(&p.sftp_id)?;
                self.background(outbox, req_id, async move {
                    entry.mkdir(&p.path).await.map_err(ssh_fail)?;
                    Ok(Value::Null)
                })
            }
            "sftp.remove" => {
                let p: proto::SftpRemoveParams = parse(payload)?;
                let entry = self.sftp(&p.sftp_id)?;
                self.background(outbox, req_id, async move {
                    entry.remove(&p.path, p.directory).await.map_err(ssh_fail)?;
                    Ok(Value::Null)
                })
            }
            "sftp.rename" => {
                let p: proto::SftpRenameParams = parse(payload)?;
                let entry = self.sftp(&p.sftp_id)?;
                self.background(outbox, req_id, async move {
                    entry.rename(&p.from, &p.to).await.map_err(ssh_fail)?;
                    Ok(Value::Null)
                })
            }
            "sftp.close" => {
                let p: proto::SftpCloseParams = parse(payload)?;
                let me = self.upgrade()?;
                self.background(outbox, req_id, async move {
                    me.engine.files.close(&p.sftp_id).await;
                    Ok(Value::Null)
                })
            }
            "transfer.start" => {
                let p: proto::TransferStartParams = parse(payload)?;
                let entry = self.sftp(&p.sftp_id)?;
                let upload = serde_json::to_value(p.direction).ok() == Some(json!("upload"));
                let id = self.engine.files.start_transfer(
                    entry,
                    upload,
                    PathBuf::from(&p.local_path),
                    p.remote_path.clone(),
                    p.recursive.unwrap_or(false),
                    self.broadcaster(),
                );
                ok(json!({"transfer_id": id}))
            }
            "transfer.cancel" => {
                let p: proto::TransferCancelParams = parse(payload)?;
                self.engine.files.cancel_transfer(&p.transfer_id);
                ok(Value::Null)
            }
            "tunnel.start" => {
                let p: proto::TunnelStartParams = parse(payload)?;
                let tunnel: easyssh_core::Tunnel =
                    serde_json::from_value(serde_json::to_value(&p.tunnel).unwrap_or_default())
                        .map_err(|e| fail("invalid_command", e.to_string()))?;
                tunnel
                    .forward_spec()
                    .map_err(|e| fail("invalid_tunnel", e.to_string()))?;
                let me = self.upgrade()?;
                self.background(outbox, req_id, async move {
                    let port = me.start_tunnel(tunnel.clone()).await?;
                    Ok(json!({"id": tunnel.id, "bind_port": port}))
                })
            }
            "tunnel.stop" => {
                let p: proto::TunnelStopParams = parse(payload)?;
                let me = self.upgrade()?;
                self.background(outbox, req_id, async move {
                    me.engine.tunnels.stop(&p.id, &me.broadcaster()).await;
                    Ok(Value::Null)
                })
            }
            "tunnel.list" => {
                let _: proto::TunnelListParams = parse(payload)?;
                ok(self.engine.tunnels.list())
            }
            other => Err(fail("unknown_command", format!("unknown command {other}"))),
        }
    }

    fn upgrade(&self) -> Result<Arc<Daemon>, Failure> {
        self.me
            .upgrade()
            .ok_or_else(|| fail("internal", "the daemon is stopping"))
    }

    fn broadcaster(&self) -> Broadcast {
        let clients = self.clients.clone();
        Arc::new(move |kind: &str, payload: Value| {
            lock(&clients)
                .values()
                .filter(|outbox| outbox.event(kind, payload.clone()))
                .count()
        })
    }

    /// Runs slow work without holding up the connection's other commands
    /// (a russh connect may wait for the user to answer a prompt, and the
    /// answer arrives on the same connection) and replies when it is done.
    fn background<F>(&self, outbox: &Outbox, req_id: &str, work: F) -> Handled
    where
        F: Future<Output = Result<Value, Failure>> + Send + 'static,
    {
        let outbox = outbox.clone();
        let id = req_id.to_owned();
        let log = self.log.clone();
        tokio::spawn(async move {
            let reply = match work.await {
                Ok(value) => json!({"id": id, "ok": value}),
                Err(f) => {
                    log.error(&format!("a background command failed: {}", f.code));
                    error_reply(&id, f.code, &f.message)
                }
            };
            outbox.send(reply).await;
        });
        Ok(Outcome::Replied)
    }

    fn sftp(&self, id: &str) -> Result<Arc<engine::files::SftpEntry>, Failure> {
        self.engine
            .files
            .get(id)
            .ok_or_else(|| fail("unknown_sftp", format!("no file session {id}")))
    }

    /// The host and the engine decision for it. Looks at `ssh -G` only when
    /// the built-in engine is wanted, so OpenSSH users see no change.
    async fn resolution(&self, host_id: &str) -> Result<(Connection, plan::Resolution), Failure> {
        let connection = self.resolve_host(host_id)?;
        let global = self.global_engine();
        let probe = connection.clone();
        let resolution = tokio::task::spawn_blocking(move || plan::resolve(&probe, global))
            .await
            .map_err(|e| fail("internal", e.to_string()))?;
        Ok((connection, resolution))
    }

    /// The plan for a host that must run on the built-in engine.
    async fn russh_plan(&self, host_id: &str) -> Result<plan::RusshPlan, Failure> {
        let (connection, resolution) = self.resolution(host_id).await?;
        resolution.plan.ok_or_else(|| match &connection.proxy {
            Some(proxy) => proxy_needs_builtin(proxy, &resolution.choice.reason),
            None => fail(
                "wrong_engine",
                format!("this host runs on OpenSSH: {}", resolution.choice.reason),
            ),
        })
    }

    /// The connection the host's terminal already has, for the monitor.
    async fn monitor_lease(&self, host_id: &str) -> Result<engine::Lease, Failure> {
        let plan = self.russh_plan(host_id).await?;
        self.engine
            .existing(&plan)
            .ok_or_else(|| fail("not_connected", "the host is not connected"))
    }

    async fn start_tunnel(&self, tunnel: easyssh_core::Tunnel) -> Result<u16, Failure> {
        let broadcast = self.broadcaster();
        let plan = self.russh_plan(&tunnel.host_id).await?;
        if !self.engine.tunnels.begin(&tunnel, &broadcast).await {
            return Err(fail("already_running", "the tunnel is already running"));
        }
        let lease = match self.engine.lease(&plan, None).await {
            Ok(lease) => lease,
            Err(error) => {
                self.engine.tunnels.failed(&tunnel, &error, &broadcast);
                return Err(ssh_shared(error));
            }
        };
        match self
            .engine
            .tunnels
            .start(&tunnel, lease, broadcast.clone())
            .await
        {
            Ok(port) => Ok(port),
            Err(error) => {
                self.engine.tunnels.failed(&tunnel, &error, &broadcast);
                Err(ssh_fail(error))
            }
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
        if let Some(host_id) = p.host_id.as_deref().filter(|id| !is_ssh_config_id(id)) {
            let connection = self.resolve_host(host_id)?;
            if let ConnectionTarget::Serial(settings) = &connection.target {
                let session = self.spawn_serial(&p, &connection, settings.clone()).await?;
                self.publish(conn, outbox, req_id, session, None).await;
                return Ok(Outcome::Replied);
            }
        }
        let mut chosen = None;
        if let Some(host_id) = &p.host_id {
            let (connection, resolution) = self.resolution(host_id).await?;
            if resolution.plan.is_none() {
                if let Some(proxy) = &connection.proxy {
                    return Err(proxy_needs_builtin(proxy, &resolution.choice.reason));
                }
            }
            if let Some(plan) = resolution.plan {
                // The built-in engine: connecting may involve prompts, so
                // it finishes in the background and replies then.
                let me = self.upgrade()?;
                let outbox = outbox.clone();
                let req_id = req_id.to_owned();
                let reason = resolution.choice.reason;
                tokio::spawn(async move {
                    me.open_remote(conn, &outbox, &req_id, p, plan, reason, agent_sock)
                        .await;
                });
                return Ok(Outcome::Replied);
            }
            chosen = Some(resolution.choice);
        }
        let engine = chosen.map(|c| ("openssh", c.reason));
        let session = self.spawn_pty(&p, agent_sock).await?;
        self.publish(conn, outbox, req_id, session, engine).await;
        Ok(Outcome::Replied)
    }

    /// A session on a serial port. The device is opened first, so a missing
    /// port or a permission problem fails the `session.open` itself, with
    /// advice (see `serial::open_error`).
    async fn spawn_serial(
        &self,
        p: &proto::SessionOpenParams,
        connection: &Connection,
        settings: SerialSettings,
    ) -> Result<Arc<TermSession>, Failure> {
        let recorder = self
            .start_recorder(p.host_id.as_deref(), Some(connection))
            .await?;
        let host_id = p.host_id.clone();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let (cols, rows) = (p.cols, p.rows);
        let scrollback = self.cfg.scrollback_bytes;
        let shell_integration = connection.shell_integration;
        let on_exit = self.exit_hook();
        self.log.info(&format!(
            "opening serial port {} at {} baud",
            settings.port, settings.baud
        ));
        tokio::task::spawn_blocking(move || {
            let port = crate::serial::open(&settings).map_err(|e| fail(e.code, e.message))?;
            TermSession::spawn_serial(
                id,
                host_id,
                port,
                SpawnOptions {
                    cols,
                    rows,
                    scrollback,
                    shell_integration,
                    recorder,
                },
                on_exit,
            )
            .map_err(|e| fail("serial_open_failed", e))
        })
        .await
        .map_err(|e| fail("internal", e.to_string()))?
    }

    fn exit_hook(&self) -> crate::terminal::ExitHook {
        let log = self.log.clone();
        Box::new(move |sid, code| {
            log.info(&format!("session {sid} ended, exit code {code:?}"));
        })
    }

    /// A session on a PTY: the system `ssh`, or a local shell.
    async fn spawn_pty(
        &self,
        p: &proto::SessionOpenParams,
        agent_sock: Option<PathBuf>,
    ) -> Result<Arc<TermSession>, Failure> {
        let connection = match p.host_id.as_deref() {
            Some(host_id) => Some(self.resolve_host(host_id)?),
            None => None,
        };
        // Saved secrets: a private key becomes a 0600 file for this ssh
        // process, and password and passphrase prompts are answered from the
        // vault for exactly this host.
        let launch = match &connection {
            Some(connection) if !p.host_id.as_deref().is_some_and(is_ssh_config_id) => {
                let (vault, store, root) = (
                    self.vault.clone(),
                    self.store.clone(),
                    self.key_dir_root.clone(),
                );
                let connection = connection.clone();
                tokio::task::spawn_blocking(move || {
                    prepare_launch(&vault, &store, &root, &connection)
                })
                .await
                .map_err(|e| fail("internal", e.to_string()))??
            }
            _ => Launch::default(),
        };
        let askpass = match &launch.binding {
            Some(binding) => {
                let helper = match &self.askpass_helper {
                    Some(helper) => helper.clone(),
                    None => std::env::current_exe().map_err(|e| {
                        fail(
                            "spawn_failed",
                            format!("cannot find the askpass helper: {e}"),
                        )
                    })?,
                };
                let context = uuid::Uuid::new_v4().simple().to_string();
                self.askpass.bind_process(&context, binding.clone());
                let label = connection
                    .as_ref()
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                Some((
                    crate::askpass::env(&self.askpass.endpoint(), &helper, &context, &label),
                    context,
                ))
            }
            None => None,
        };
        let recorder = self
            .start_recorder(p.host_id.as_deref(), connection.as_ref())
            .await?;
        let shell = self.cfg.local_shell.clone();
        let host_id = p.host_id.clone();
        let id = uuid::Uuid::new_v4().simple().to_string();
        let scrollback = self.cfg.scrollback_bytes;
        let (cols, rows) = (p.cols, p.rows);
        let spawn_id = id.clone();
        let exit_log = self.log.clone();
        let exit_hub = self.askpass.clone();
        let (askpass_env, askpass_context) = match askpass {
            Some((env, context)) => (env, Some(context)),
            None => (Vec::new(), None),
        };
        // The key file lives exactly as long as the ssh process: the exit
        // hook (or a failed spawn, which drops the hook) deletes it.
        let key_file = Arc::new(Mutex::new(launch.key_file));
        let hook_key_file = key_file.clone();
        let hook_context = askpass_context.clone();
        let on_exit: crate::terminal::ExitHook = Box::new(move |sid, code| {
            exit_log.info(&format!("session {sid} ended, exit code {code:?}"));
            if let Some(context) = &hook_context {
                exit_hub.unbind_process(context);
            }
            drop(lock(&hook_key_file).take());
        });
        let unbind_hub = self.askpass.clone();
        let session = tokio::task::spawn_blocking(move || {
            let identity = lock(&key_file).as_ref().map(|k| k.path().to_owned());
            let (mut command, shell_integration) = command_for(
                shell.as_deref(),
                connection,
                agent_sock.as_deref(),
                identity.as_deref(),
            )?;
            for (name, value) in &askpass_env {
                command.env(name, value);
            }
            TermSession::spawn(
                spawn_id,
                host_id,
                command,
                SpawnOptions {
                    cols,
                    rows,
                    scrollback,
                    shell_integration,
                    recorder,
                },
                on_exit,
            )
            .map_err(|e| fail("spawn_failed", e))
        })
        .await
        .map_err(|e| fail("internal", e.to_string()))
        .and_then(|result| result);
        match session {
            Ok(session) => Ok(session),
            Err(failure) => {
                if let Some(context) = &askpass_context {
                    unbind_hub.unbind_process(context);
                }
                Err(failure)
            }
        }
    }

    /// Registers a new session, replies to `session.open` and attaches the
    /// requesting connection. The reply must precede the session's events on
    /// this connection: it is queued first, then the connection subscribes
    /// (which replays whatever the session already printed).
    async fn publish(
        &self,
        conn: u64,
        outbox: &Outbox,
        req_id: &str,
        session: Arc<TermSession>,
        engine: Option<(&str, String)>,
    ) {
        let mut result = json!({"session_id": session.id()});
        if let Some((engine, reason)) = engine {
            session.set_engine(engine, &reason);
            result["engine"] = json!(engine);
            result["engine_reason"] = json!(reason);
        }
        lock(&self.sessions).insert(session.id().to_owned(), session.clone());
        outbox.send(json!({"id": req_id, "ok": result})).await;
        session.attach(conn, outbox, None, None);
    }

    /// `session.open` on the built-in engine. Falls back to OpenSSH when the
    /// engine reports something it does not support.
    #[allow(clippy::too_many_arguments)]
    async fn open_remote(
        &self,
        conn: u64,
        outbox: &Outbox,
        req_id: &str,
        p: proto::SessionOpenParams,
        plan: plan::RusshPlan,
        reason: String,
        agent_sock: Option<PathBuf>,
    ) {
        let opened = self
            .engine
            .open_shell(
                &plan,
                agent_sock.clone(),
                p.cols,
                p.rows,
                self.app_language(),
            )
            .await;
        let result = match opened {
            Ok((shell, lease)) => {
                let id = uuid::Uuid::new_v4().simple().to_string();
                // The same opt-ins as on a PTY: the byte stream is the same,
                // so shell integration and the session log work alike.
                let connection = p
                    .host_id
                    .as_deref()
                    .and_then(|host_id| self.resolve_host(host_id).ok());
                let shell_integration = connection.as_ref().is_some_and(|c| c.shell_integration);
                let recorder = match self
                    .start_recorder(p.host_id.as_deref(), connection.as_ref())
                    .await
                {
                    Ok(recorder) => recorder,
                    Err(failure) => {
                        // Dropping the shell and the lease closes the channel.
                        self.log
                            .error(&format!("session.open failed: {}", failure.code));
                        outbox
                            .send(error_reply(req_id, failure.code, &failure.message))
                            .await;
                        return;
                    }
                };
                let session = TermSession::spawn_remote(
                    id,
                    p.host_id.clone(),
                    shell,
                    Box::new(lease),
                    SpawnOptions {
                        cols: p.cols,
                        rows: p.rows,
                        scrollback: self.cfg.scrollback_bytes,
                        shell_integration,
                        recorder,
                    },
                    self.exit_hook(),
                );
                Ok((session, ("russh", reason)))
            }
            Err(error) if matches!(*error, SshError::Unsupported(_)) => {
                let detail = error.to_string();
                self.log
                    .info("the built-in engine refused a host; using OpenSSH");
                let reason = engine::choice::fallback_reason(&detail);
                self.spawn_pty(&p, agent_sock)
                    .await
                    .map(|session| (session, ("openssh", reason)))
            }
            Err(error) => Err(ssh_shared(error)),
        };
        match result {
            Ok((session, engine)) => {
                self.publish(conn, outbox, req_id, session, Some(engine))
                    .await
            }
            Err(f) => {
                self.log.error(&format!("session.open failed: {}", f.code));
                outbox.send(error_reply(req_id, f.code, &f.message)).await;
            }
        }
    }

    /// The app's language setting (`zh`, `en`), or `None` when it follows
    /// the system.
    fn app_language(&self) -> Option<&'static str> {
        match lock(&self.store).get_setting_as("locale").ok().flatten() {
            Some(easyssh_core::Locale::ZhCn) => Some("zh"),
            Some(easyssh_core::Locale::En) => Some("en"),
            _ => None,
        }
    }

    /// The global engine setting (`openssh` unless the user chose `russh`).
    fn global_engine(&self) -> easyssh_core::SshEngine {
        lock(&self.store)
            .get_setting_as("ssh_engine")
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// The encrypted session log of a new session, when the host's setting
    /// (or the default) asks for one. A session that should be recorded but
    /// cannot be (the vault is locked, the disk is full) does not open:
    /// silently running unrecorded would break the promise of the setting.
    async fn start_recorder(
        &self,
        host_id: Option<&str>,
        connection: Option<&Connection>,
    ) -> Result<Option<Arc<crate::recorder::Recorder>>, Failure> {
        let settings = lock(&self.store)
            .get_setting_as::<LogSettings>(LogSettings::KEY)
            .ok()
            .flatten()
            .unwrap_or_default();
        if !settings.should_record(host_id) {
            return Ok(None);
        }
        let label = connection.map_or_else(|| "local".to_owned(), |c| c.name.clone());
        let (vault, logs, log) = (self.vault.clone(), self.logs.clone(), self.log.clone());
        tokio::task::spawn_blocking(move || {
            vault.ensure_unlocked().map_err(vault_failure)?;
            let derive = |salt: &[u8]| {
                vault
                    .derive_key(KEY_INFO, salt)
                    .map_err(|error| error.to_string())
            };
            let error_log = log.clone();
            crate::recorder::Recorder::start(
                &logs,
                &label,
                &derive,
                Arc::new(move |why: &str| {
                    error_log.error(&format!("session log stopped: {why}"));
                }),
            )
            .map(Some)
            .map_err(|why| fail("log_failed", format!("cannot write the session log: {why}")))
        })
        .await
        .map_err(|e| fail("internal", e.to_string()))?
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

    /// Parses an import file given as a path (read here, at most 8 MiB) or as
    /// base64 bytes, and marks the hosts that are stored already.
    fn import_preview(&self, p: ImportPreviewParams) -> Result<ImportPreview, Failure> {
        let file_name = p.file_name.clone().unwrap_or_default();
        let bytes = match (&p.path, &p.data) {
            (Some(path), None) => read_import_file(Path::new(path)).map_err(import_failure)?,
            (None, Some(data)) => {
                // Base64 is 4/3 of the bytes; refuse before decoding.
                if data.len() > MAX_IMPORT_BYTES / 3 * 4 + 4 {
                    return Err(fail(
                        "invalid_import",
                        format!("the file is larger than {} MiB", MAX_IMPORT_BYTES >> 20),
                    ));
                }
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|e| fail("invalid_import", format!("data is not base64: {e}")))?
            }
            _ => {
                return Err(fail(
                    "invalid_import",
                    "give exactly one of `path` and `data`",
                ))
            }
        };
        let base_dir = p.base_dir.as_deref().map(Path::new);
        import_preview(
            &lock(&self.store),
            &PreviewRequest {
                bytes: &bytes,
                file_name: &file_name,
                format: p.format.as_deref(),
                base_dir,
            },
        )
        .map_err(import_failure)
    }

    fn delete_host(&self, id: &str) -> Result<(), Failure> {
        lock(&self.store)
            .remove_host(id)
            .map(|_| ())
            .map_err(store_failure)
    }
}

fn is_ssh_config_id(id: &str) -> bool {
    id.starts_with(SSH_CONFIG_ID_PREFIX)
}

/// What a session needs from the vault before `ssh` starts.
#[derive(Default)]
struct Launch {
    key_file: Option<KeyFile>,
    binding: Option<Binding>,
}

/// Looks at what `connection` has saved. Nothing saved: no change to how the
/// session starts. A saved key is written to its temporary file (which needs
/// the vault open; a locked one fails the open with `vault_locked` so the UI
/// can ask for the passphrase and retry).
fn prepare_launch(
    vault: &Vault,
    store: &Mutex<Store>,
    runtime_dir: &Path,
    connection: &Connection,
) -> Result<Launch, Failure> {
    let store = lock(store);
    let flags = store
        .host_secret_flags(&connection.id)
        .map_err(store_failure)?;
    if !flags.any() {
        return Ok(Launch::default());
    }
    let mut key_file = None;
    if flags.private_key {
        let secrets = vault
            .host_secrets(&store, &connection.id)
            .map_err(vault_failure)?;
        if let Some(key) = &secrets.private_key {
            key_file = Some(
                KeyFile::write(runtime_dir, key)
                    .map_err(|e| fail("io_error", format!("cannot write the key file: {e}")))?,
            );
        }
    }
    let key_path = key_file
        .as_ref()
        .map(|file| file.path().to_string_lossy().into_owned());
    let mut binding = Binding::for_connection(connection, key_path);
    binding.passphrase_expected = flags.passphrase;
    Ok(Launch {
        key_file,
        binding: Some(binding),
    })
}

fn vault_failure(error: VaultError) -> Failure {
    fail(error.code(), error.to_string())
}

/// A host with an outbound proxy cannot run on OpenSSH: there is no safe
/// built-in `ProxyCommand` (it would need `nc` or `ncat`, which differ by
/// system and cannot carry a proxy password without putting it on a command
/// line), and starting `ssh` without the proxy would silently connect
/// directly. So the host does not open, and the message says why.
fn proxy_needs_builtin(proxy: &easyssh_core::ProxySettings, reason: &str) -> Failure {
    fail(
        "proxy_needs_builtin_engine",
        format!(
            "this host connects through the proxy {}, which only the built-in engine supports, \
             but it would run on OpenSSH ({reason}). Use the built-in engine for this host, or \
             remove the proxy.",
            proxy.display()
        ),
    )
}

fn flags_json(flags: SecretFlags) -> Value {
    json!({
        "password": flags.password,
        "private_key": flags.private_key,
        "passphrase": flags.passphrase,
        "proxy_password": flags.proxy_password,
    })
}

#[derive(serde::Deserialize)]
struct ImportPreviewParams {
    path: Option<String>,
    data: Option<String>,
    file_name: Option<String>,
    format: Option<String>,
    base_dir: Option<String>,
}

/// A refused import is the caller's input; a store failure is ours.
fn import_failure(error: StoreError) -> Failure {
    match &error {
        StoreError::Invalid(message) => fail("invalid_import", message.clone()),
        _ => store_failure(error),
    }
}

/// Upper bound for a `known_hosts` file the daemon agrees to import.
const MAX_KNOWN_HOSTS_BYTES: u64 = 16 * 1024 * 1024;

fn sync_failure(error: SyncFailure) -> Failure {
    fail(error.code, error.message)
}

#[derive(serde::Deserialize)]
struct MembersAdd {
    vault_id: String,
    username: String,
    role: easyssh_sync::wire::Role,
    fingerprint: String,
}

#[derive(serde::Deserialize)]
struct MembersRole {
    vault_id: String,
    account_id: String,
    role: easyssh_sync::wire::Role,
}

fn store_failure(error: StoreError) -> Failure {
    match &error {
        StoreError::Invalid(_) => fail("invalid_host", error.to_string()),
        StoreError::Io(_) => fail("io_error", error.to_string()),
        _ => fail("store_error", error.to_string()),
    }
}

fn ssh_fail(error: SshError) -> Failure {
    fail(engine::error_code(&error), error.to_string())
}

fn ssh_shared(error: engine::SharedError) -> Failure {
    fail(engine::error_code(&error), error.to_string())
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
    identity: Option<&Path>,
) -> Result<(CommandBuilder, bool), Failure> {
    let mut shell_integration = false;
    let mut command = match connection {
        Some(connection) => {
            shell_integration = connection.shell_integration;
            let invocation = SshInvocation::for_connection(&OpenSsh, &connection)
                .map_err(|e| fail("invalid_host", e.to_string()))?;
            let mut command = CommandBuilder::new(invocation.executable);
            if let Some(identity) = identity {
                // The saved key, and only it: a saved key must not be mixed
                // with whatever the agent offers first.
                command.args(["-i"]);
                command.arg(identity);
                command.args(["-o", "IdentitiesOnly=yes"]);
            }
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
    Ok((command, shell_integration))
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
