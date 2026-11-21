//! The mobile app (Android and iOS): the engine runs in this process.
//!
//! There is no daemon, no OpenSSH, no local shell and no OS keychain here. The
//! commands have the names and shapes of the desktop ones (`load_hosts`,
//! `session_open`, `sftp_*`, `transfer_*`, `tunnel_*`, `sync_call`,
//! `askpass_reply`, ...), so the React components that show prompts, files and
//! forwards are shared with the desktop UI. A terminal, the file browser and a
//! forward on one host share one connection (`easyssh_engine::registry`); it
//! lingers for 30 s after the last of them closes. A session lives exactly as
//! long as its terminal screen: when the operating system suspends the app the
//! TCP connection dies and the UI offers to reconnect.
//!
//! What the web view cannot do for itself is done here in the app process
//! (staging a picked file, the download folder) or, for the share sheet, in the
//! platform shell (`gen/android/.../AnixNative.kt`; the web view's own share on
//! iOS).

use crate::mobile_vault;
use easyssh_engine::events::{ui_event_name, Events};
use easyssh_engine::hosts::{save_host, save_tunnel, snapshot, target_for, NewHost, NewTunnel};
use easyssh_engine::known_hosts::StoreKnownHosts;
use easyssh_engine::local_files::{reserve_unique_path, sweep, Staging};
use easyssh_engine::prompts::Prompts;
use easyssh_engine::supervisor::{
    Attach, ReconnectPolicy, SessionOptions, Status, SupervisedSink, Supervisor,
};
use easyssh_engine::sync::{SyncFailure, SyncOptions, SyncService};
use easyssh_engine::tunnels::ui_status_event;
use easyssh_engine::{with_saved_login, Engine, ShellSink, Target};
use easyssh_mobile_keys::mobile_vault as build_vault;
use easyssh_store::{open_app_store_with, Store};
use easyssh_vault::{SecretSource, Vault, VaultSecrets};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::ipc::{Channel, InvokeBody, Request, Response};
use tauri::{Emitter, Manager, State};

/// Uploads that were never finished and downloads nobody shared are removed
/// after this long.
const LEFTOVER_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub(crate) struct Mobile {
    store: Arc<Mutex<Store>>,
    engine: Arc<Engine>,
    /// Attach to tmux/screen and reconnect on top of the engine.
    sessions: Supervisor,
    sync: Arc<SyncService>,
    pub(crate) vault: Arc<Vault>,
    secrets: Arc<VaultSecrets>,
    /// Files picked on the phone, waiting to be uploaded.
    staging: Staging,
    /// Where downloads land before they are shared or saved.
    downloads: PathBuf,
}

/// The parts of [`Mobile`] a blocking task needs, which must be `'static`.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) vault: Arc<Vault>,
    store: Arc<Mutex<Store>>,
}

impl Shared {
    /// Runs `work` with the database. Never call anything that can raise a
    /// system prompt inside it: the lock is held for as long as it runs.
    pub(crate) fn with_store<T>(&self, work: impl FnOnce(&Store) -> T) -> T {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        work(&store)
    }
}

impl Mobile {
    pub(crate) fn share(&self) -> Shared {
        Shared {
            vault: self.vault.clone(),
            store: self.store.clone(),
        }
    }

    fn with_store<T>(&self, work: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        work(&store)
    }

    /// Where to connect for a saved host, with its jump hosts and the saved
    /// login of the host (key and password) from the vault. Every way of opening
    /// a host (terminal, files, forward) comes through here, so they all make
    /// the same connection and share it. Reading the vault can raise the
    /// biometric prompt, so it runs off the async threads; a saved key with the
    /// vault locked is the error "the vault is locked".
    async fn target(&self, host_id: &str) -> Result<Target, String> {
        let target = self.with_store(|store| {
            let config = store.load_app_config().map_err(|error| error.to_string())?;
            target_for(&config, host_id)
        })?;
        let secrets: Arc<dyn SecretSource> = self.secrets.clone();
        tauri::async_runtime::spawn_blocking(move || with_saved_login(&secrets, target))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.message)
    }
}

/// Same shape as the desktop's session events.
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum SessionEvent {
    Data {
        data: String,
    },
    Exit {
        code: Option<u32>,
    },
    /// The connection dropped; `attempt` of the retries starts after `delay_ms`.
    Reconnecting {
        attempt: u32,
        delay_ms: u64,
    },
    /// A new connection is up and attached again.
    Resumed,
}

struct ChannelSink(Channel<SessionEvent>);

impl ShellSink for ChannelSink {
    fn data(&self, text: String) {
        let _ = self.0.send(SessionEvent::Data { data: text });
    }

    fn exit(&self, code: Option<u32>) {
        let _ = self.0.send(SessionEvent::Exit { code });
    }
}

impl SupervisedSink for ChannelSink {
    fn status(&self, status: Status) {
        let _ = self.0.send(match status {
            Status::Reconnecting { attempt, delay } => SessionEvent::Reconnecting {
                attempt,
                delay_ms: delay.as_millis() as u64,
            },
            Status::Resumed => SessionEvent::Resumed,
        });
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenedSession {
    id: String,
    engine: &'static str,
    engine_reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HostFieldsInput {
    hostname: String,
    username: Option<String>,
    port: u16,
    proxy_jump: Option<String>,
    #[serde(default)]
    proxy: Option<easyssh_core::ProxySettings>,
}

/// The part of the desktop's `HostInput` the mobile form fills in.
#[derive(Deserialize)]
struct HostInput {
    id: Option<String>,
    name: String,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    favorite: bool,
    fields: HostFieldsInput,
}

#[tauri::command]
fn load_hosts(state: State<'_, Mobile>) -> Result<Value, String> {
    state.with_store(|store| {
        let config = store.load_app_config().map_err(|error| error.to_string())?;
        Ok(snapshot(&config))
    })
}

#[tauri::command]
fn host_save(state: State<'_, Mobile>, input: HostInput) -> Result<String, String> {
    state.with_store(|store| {
        save_host(
            store,
            &NewHost {
                id: input.id,
                name: input.name,
                hostname: input.fields.hostname,
                username: input.fields.username,
                port: input.fields.port,
                group: input.group,
                tags: input.tags,
                favorite: input.favorite,
                jump: input.fields.proxy_jump,
                proxy: input.fields.proxy,
            },
        )
    })
}

#[tauri::command]
fn host_delete(state: State<'_, Mobile>, id: String) -> Result<(), String> {
    state.with_store(|store| {
        store
            .remove_host(&id)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
fn host_set_favorite(state: State<'_, Mobile>, id: String, favorite: bool) -> Result<(), String> {
    state.with_store(|store| {
        let config = store.load_app_config().map_err(|error| error.to_string())?;
        let mut connection = config
            .connections
            .into_iter()
            .find(|connection| connection.id == id)
            .ok_or("only saved hosts can be favorites")?;
        connection.favorite = favorite;
        store
            .save_host(&connection, None)
            .map_err(|error| error.to_string())
    })
}

// ---- terminal ----------------------------------------------------------

#[tauri::command]
async fn session_open(
    state: State<'_, Mobile>,
    host_id: Option<String>,
    cols: u16,
    rows: u16,
    events: Channel<SessionEvent>,
    attach: Option<String>,
    keepalive_secs: Option<u64>,
) -> Result<OpenedSession, String> {
    let host_id = host_id.ok_or("a phone has no local shell; open a host")?;
    let target = state.target(&host_id).await?;
    // Keepalive applies to connections opened from now on; 0 turns it off.
    if let Some(seconds) = keepalive_secs {
        let interval = (seconds > 0).then(|| Duration::from_secs(seconds));
        state.engine.set_keepalive(interval);
    }
    let attach = Attach::parse(attach.as_deref().unwrap_or(""));
    let options = SessionOptions {
        attach,
        // A reconnect only makes sense where it lands in the same session.
        reconnect: attach != Attach::None,
        policy: ReconnectPolicy::default(),
    };
    let password = target.saved_password.clone();
    let id = state
        .sessions
        .open_with_login(
            target,
            password,
            cols,
            rows,
            Arc::new(ChannelSink(events)),
            options,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(OpenedSession {
        id,
        engine: "russh",
        engine_reason: None,
    })
}

#[tauri::command]
fn session_write(state: State<'_, Mobile>, id: String, data: String) -> Result<(), String> {
    state
        .sessions
        .write(&id, &easyssh_engine::text_to_bytes(&data))
}

#[tauri::command]
fn session_resize(
    state: State<'_, Mobile>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state.sessions.resize(&id, cols, rows)
}

#[tauri::command]
fn session_close(state: State<'_, Mobile>, id: String) {
    state.sessions.close(&id);
}

/// The app came back from the background: replace the connection of an
/// attached session at once instead of waiting for the old socket to fail. The
/// host's shared connection is replaced too, so the files and forwards that
/// open next use the new one. Async so it runs inside the runtime the
/// reconnect task is spawned on.
#[tauri::command]
async fn session_reconnect(state: State<'_, Mobile>, id: String) -> Result<(), String> {
    state.sessions.reconnect(&id)
}

/// A phone keeps no session once its screen is gone, so detaching ends it.
#[tauri::command]
fn session_detach(state: State<'_, Mobile>, id: String) {
    state.sessions.close(&id);
}

#[tauri::command]
fn sessions_list() -> Vec<Value> {
    Vec::new()
}

#[tauri::command]
fn askpass_reply(state: State<'_, Mobile>, id: u64, answer: Option<String>) {
    state.engine.prompts().reply_secret(id, answer);
}

#[tauri::command]
fn hostkey_reply(state: State<'_, Mobile>, id: u64, decision: String) {
    state.engine.prompts().reply_hostkey(id, &decision);
}

// ---- files -------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenedFiles {
    id: String,
    home: String,
}

fn files_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn sftp_entry(state: &Mobile, id: &str) -> Result<Arc<easyssh_engine::files::SftpEntry>, String> {
    state
        .engine
        .files()
        .get(id)
        .ok_or_else(|| "the file session has ended".to_owned())
}

#[tauri::command]
async fn sftp_open(state: State<'_, Mobile>, host_id: String) -> Result<OpenedFiles, String> {
    let target = state.target(&host_id).await?;
    let (id, home) = state
        .engine
        .open_files(&target)
        .await
        .map_err(files_error)?;
    Ok(OpenedFiles { id, home })
}

#[tauri::command]
async fn sftp_list(
    state: State<'_, Mobile>,
    id: String,
    path: String,
) -> Result<Vec<Value>, String> {
    sftp_entry(&state, &id)?
        .list_view(&path)
        .await
        .map_err(files_error)
}

#[tauri::command]
async fn sftp_mkdir(state: State<'_, Mobile>, id: String, path: String) -> Result<(), String> {
    sftp_entry(&state, &id)?
        .mkdir(&path)
        .await
        .map_err(files_error)
}

#[tauri::command]
async fn sftp_remove(
    state: State<'_, Mobile>,
    id: String,
    path: String,
    directory: bool,
) -> Result<(), String> {
    sftp_entry(&state, &id)?
        .remove(&path, directory)
        .await
        .map_err(files_error)
}

#[tauri::command]
async fn sftp_rename(
    state: State<'_, Mobile>,
    id: String,
    from: String,
    to: String,
) -> Result<(), String> {
    sftp_entry(&state, &id)?
        .rename(&from, &to)
        .await
        .map_err(files_error)
}

#[tauri::command]
async fn sftp_chmod(
    state: State<'_, Mobile>,
    id: String,
    path: String,
    mode: u32,
    recursive: bool,
) -> Result<(), String> {
    sftp_entry(&state, &id)?
        .chmod(&path, mode, recursive)
        .await
        .map_err(files_error)
}

#[tauri::command]
async fn sftp_close(state: State<'_, Mobile>, id: String) -> Result<(), String> {
    state.engine.files().close(&id).await;
    Ok(())
}

/// A monitor failure the UI tells apart by `code` (`not_connected`, ...),
/// like the desktop's.
#[derive(Serialize)]
struct MonitorError {
    code: &'static str,
    message: String,
}

/// `monitor_sample`: the raw sample script output, read on the connection a
/// terminal, the files or a forward of the host already holds. Never
/// connects and never reads the vault, so it can neither prompt nor wake a
/// sleeping connection.
#[tauri::command]
async fn monitor_sample(
    state: State<'_, Mobile>,
    host_id: String,
    detail: Option<bool>,
) -> Result<Value, MonitorError> {
    // The connection key does not depend on the saved login.
    let target = state
        .with_store(|store| {
            let config = store.load_app_config().map_err(|error| error.to_string())?;
            target_for(&config, &host_id)
        })
        .map_err(|message| MonitorError {
            code: "failed",
            message,
        })?;
    let lease = state.engine.existing(&target).ok_or(MonitorError {
        code: "not_connected",
        message: "the host is not connected".into(),
    })?;
    let sample = easyssh_engine::monitor::sample(&lease, detail.unwrap_or(false))
        .await
        .map_err(|error| MonitorError {
            code: "failed",
            message: error.to_string(),
        })?;
    Ok(serde_json::json!({"output": sample.output, "latency_ms": sample.latency_ms}))
}

/// What a transfer reports to the web view (the desktop's `TransferEvent`).
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum TransferEvent {
    Started,
    Progress { bytes: u64, total: Option<u64> },
    Done,
    Failed { message: String },
    Cancelled,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartedTransfer {
    id: String,
    local: String,
    remote: String,
    queued: bool,
}

fn remote_join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Uploads `local` (a file staged by `stage_*`) into the remote directory
/// `remote`, or downloads the remote file `remote` into the app's download
/// folder. The result is reported on `events`.
#[tauri::command]
async fn transfer_start(
    state: State<'_, Mobile>,
    host_id: String,
    upload: bool,
    remote: String,
    local: Option<String>,
    events: Channel<TransferEvent>,
) -> Result<StartedTransfer, String> {
    let (local, remote) = if upload {
        let local = PathBuf::from(local.ok_or("no local file was given")?);
        // Only what `stage_*` produced can be uploaded: the web view has no
        // business reading other files of the app.
        if state.staging.id_of(&local).is_none() {
            return Err("the file was not picked on this phone".to_owned());
        }
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("the local file name is not valid UTF-8")?;
        let remote = remote_join(&remote, name);
        (local, remote)
    } else {
        let name = remote
            .rsplit('/')
            .find(|part| !part.is_empty())
            .ok_or("the remote path has no file name")?;
        let local = reserve_unique_path(&state.downloads, name)?;
        (local, remote)
    };

    // A download's name is reserved by an empty file; it goes again when the
    // transfer does not complete.
    let reserved = (!upload).then(|| local.clone());
    let target = match state.target(&host_id).await {
        Ok(target) => target,
        Err(error) => {
            if let Some(path) = &reserved {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    let staging = state.staging.clone();
    let staged = upload.then(|| local.clone());
    let sink: Events = Arc::new(move |name: &str, payload: Value| {
        match name {
            "transfer.started" => {
                let _ = events.send(TransferEvent::Started);
            }
            "transfer.progress" => {
                let bytes = payload["bytes"].as_u64().unwrap_or(0);
                let total = payload["total"].as_u64();
                let _ = events.send(TransferEvent::Progress { bytes, total });
            }
            "transfer.done" => {
                let event = match payload["status"].as_str() {
                    Some("completed") => TransferEvent::Done,
                    Some("cancelled") => TransferEvent::Cancelled,
                    _ => TransferEvent::Failed {
                        message: payload["error"]
                            .as_str()
                            .unwrap_or("the transfer failed")
                            .to_owned(),
                    },
                };
                let completed = matches!(event, TransferEvent::Done);
                let _ = events.send(event);
                if let Some(path) = reserved.as_ref().filter(|_| !completed) {
                    // Only the empty reservation: a partly written file is
                    // never under this name (it is renamed when complete).
                    if std::fs::metadata(path).is_ok_and(|meta| meta.len() == 0) {
                        let _ = std::fs::remove_file(path);
                    }
                }
                // The staged copy is not needed any more.
                if let Some(id) = staged.as_ref().and_then(|path| staging.id_of(path)) {
                    staging.discard(&id);
                }
            }
            _ => {}
        }
        1usize
    });
    // Joins the host's transfer queue: a few run at once on shared file
    // sessions, the rest wait.
    let (id, queued) = state.engine.queue_transfer(
        &host_id,
        target,
        upload,
        local.clone(),
        remote.clone(),
        false,
        sink,
    );
    Ok(StartedTransfer {
        id,
        local: local.to_string_lossy().into_owned(),
        remote,
        queued,
    })
}

#[tauri::command]
fn transfer_cancel(state: State<'_, Mobile>, id: String) {
    state.engine.files().cancel_transfer(&id);
}

// ---- picked files and downloads -----------------------------------------

/// Starts staging a file the person picked; returns its id.
#[tauri::command]
fn stage_begin(state: State<'_, Mobile>, name: String) -> Result<String, String> {
    state.staging.begin(&name)
}

/// One chunk of a picked file: the bytes are the request body, the headers say
/// which upload (`x-upload-id`) and where the chunk starts (`x-offset`).
/// Returns the new length.
#[tauri::command]
fn stage_chunk(state: State<'_, Mobile>, request: Request<'_>) -> Result<u64, String> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("the chunk must be raw bytes".to_owned());
    };
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let id = header("x-upload-id").ok_or("the upload id is missing")?;
    let offset: u64 = header("x-offset")
        .and_then(|value| value.parse().ok())
        .ok_or("the offset is missing")?;
    state.staging.append(&id, offset, bytes)
}

/// The path of a staged file, to hand to `transfer_start`.
#[tauri::command]
fn stage_finish(state: State<'_, Mobile>, id: String) -> Result<String, String> {
    state
        .staging
        .path(&id)
        .map(|path| path.to_string_lossy().into_owned())
}

#[tauri::command]
fn stage_discard(state: State<'_, Mobile>, id: String) {
    state.staging.discard(&id);
}

/// The bytes of a finished download (inside the download folder only), for the
/// web view's share sheet on iOS.
#[tauri::command]
fn read_download(state: State<'_, Mobile>, path: String) -> Result<Response, String> {
    let root = state
        .downloads
        .canonicalize()
        .map_err(|error| error.to_string())?;
    let file = PathBuf::from(&path)
        .canonicalize()
        .map_err(|_| "the download is gone".to_owned())?;
    if !file.starts_with(&root) {
        return Err("not a download".to_owned());
    }
    std::fs::read(file)
        .map(Response::new)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn forget_download(state: State<'_, Mobile>, path: String) {
    let Ok(root) = state.downloads.canonicalize() else {
        return;
    };
    if let Ok(file) = PathBuf::from(path).canonicalize() {
        if file.starts_with(&root) && file.is_file() {
            let _ = std::fs::remove_file(file);
        }
    }
}

// ---- forwards ----------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TunnelView {
    id: String,
    name: String,
    host_id: String,
    kind: easyssh_core::TunnelKind,
    bind_address: Option<String>,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
    status: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TunnelInput {
    id: Option<String>,
    name: String,
    host_id: String,
    kind: easyssh_core::TunnelKind,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
}

#[tauri::command]
fn tunnels_list(state: State<'_, Mobile>) -> Result<Vec<TunnelView>, String> {
    let config = state.with_store(|store| store.load_app_config().map_err(|e| e.to_string()))?;
    Ok(config
        .tunnels
        .into_iter()
        .map(|tunnel| TunnelView {
            status: state.engine.tunnels().ui_status_of(&tunnel.id),
            id: tunnel.id,
            name: tunnel.name,
            host_id: tunnel.host_id,
            kind: tunnel.kind,
            bind_address: tunnel.bind_address,
            bind_port: tunnel.bind_port,
            target_host: tunnel.target_host,
            target_port: tunnel.target_port,
        })
        .collect())
}

#[tauri::command]
fn tunnel_save(state: State<'_, Mobile>, input: TunnelInput) -> Result<String, String> {
    state.with_store(|store| {
        save_tunnel(
            store,
            &NewTunnel {
                id: input.id,
                name: input.name,
                host_id: input.host_id,
                kind: input.kind,
                bind_port: input.bind_port,
                target_host: input.target_host.unwrap_or_default(),
                target_port: input.target_port.unwrap_or(0),
            },
        )
    })
}

#[tauri::command]
async fn tunnel_delete(state: State<'_, Mobile>, id: String) -> Result<(), String> {
    state.engine.stop_tunnel(&id).await;
    state.with_store(|store| {
        store
            .remove_tunnel(&id)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
async fn tunnel_start(state: State<'_, Mobile>, id: String) -> Result<(), String> {
    let config = state.with_store(|store| store.load_app_config().map_err(|e| e.to_string()))?;
    let tunnel = config
        .tunnels
        .into_iter()
        .find(|tunnel| tunnel.id == id)
        .ok_or("this forward no longer exists")?;
    let target = state.target(&tunnel.host_id).await?;
    state
        .engine
        .start_tunnel(&tunnel, &target)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn tunnel_stop(state: State<'_, Mobile>, id: String) -> Result<(), String> {
    state.engine.stop_tunnel(&id).await;
    Ok(())
}

// ---- sync --------------------------------------------------------------

/// The error the UI receives: a stable code and a message to show.
#[derive(Debug, Serialize)]
struct SyncError {
    code: String,
    message: String,
}

impl From<SyncFailure> for SyncError {
    fn from(failure: SyncFailure) -> Self {
        SyncError {
            code: failure.code.to_owned(),
            message: failure.message,
        }
    }
}

#[tauri::command]
async fn sync_call(
    state: State<'_, Mobile>,
    app: tauri::AppHandle,
    kind: String,
    payload: Option<Value>,
) -> Result<Value, SyncError> {
    let result = state
        .sync
        .call(&kind, payload.unwrap_or(Value::Null))
        .await?;
    // A sign-in pulls the hosts: tell the lists to reload.
    if matches!(kind.as_str(), "sync.signin" | "sync.signup" | "sync.now") {
        let _ = app.emit("hosts-changed", ());
    }
    Ok(result)
}

/// Crash reports are a desktop feature for now; the web view still reports
/// its errors through this command, so it must exist.
#[tauri::command]
fn telemetry_report_frontend() {}

/// HTTPS to the sync server verifies certificates through the Android trust
/// store (rustls-platform-verifier). That crate calls into the JVM through a
/// small Kotlin component (`gen/android/app/build.gradle.kts` adds it), and it
/// needs the JVM and the application context once, before the first request.
/// `tao` stored both in `ndk-context` when it created the activity.
#[cfg(target_os = "android")]
fn init_certificate_verifier() {
    use jni::objects::JObject;
    use jni::JavaVM;

    let context = ndk_context::android_context();
    // SAFETY: `tao` initialised these pointers from the live activity, and
    // both stay valid for the life of the process.
    let vm = unsafe { JavaVM::from_raw(context.vm().cast()) };
    let result = vm.attach_current_thread(|env| -> Result<(), jni::errors::Error> {
        let application = unsafe { JObject::from_raw(env, context.context().cast()) };
        rustls_platform_verifier::android::init_with_env(env, application)
    });
    if let Err(error) = result {
        // Sync then fails with a certificate error; the terminal is unaffected.
        eprintln!("cannot initialise the certificate verifier: {error}");
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_device_key::init())
        .invoke_handler(tauri::generate_handler![
            load_hosts,
            host_save,
            host_delete,
            host_set_favorite,
            session_open,
            session_write,
            session_resize,
            session_close,
            session_reconnect,
            session_detach,
            sessions_list,
            askpass_reply,
            hostkey_reply,
            sftp_open,
            sftp_list,
            sftp_mkdir,
            sftp_remove,
            sftp_rename,
            sftp_chmod,
            sftp_close,
            monitor_sample,
            transfer_start,
            transfer_cancel,
            stage_begin,
            stage_chunk,
            stage_finish,
            stage_discard,
            read_download,
            forget_download,
            tunnels_list,
            tunnel_save,
            tunnel_delete,
            tunnel_start,
            tunnel_stop,
            sync_call,
            telemetry_report_frontend,
            mobile_vault::vault_status,
            mobile_vault::vault_unlock,
            mobile_vault::vault_unlock_with,
            mobile_vault::vault_enable_unlock,
            mobile_vault::vault_disable_unlock,
            mobile_vault::vault_lock,
            mobile_vault::vault_set_passphrase,
            mobile_vault::vault_remove_passphrase,
            mobile_vault::vault_setup_skip,
            mobile_vault::vault_set_auto_lock,
            mobile_vault::vault_set_background_lock,
            mobile_vault::vault_background,
            mobile_vault::vault_foreground,
            mobile_vault::vault_forget_all,
            mobile_vault::secrets_status,
            mobile_vault::secrets_set,
        ])
        .setup(|app| {
            #[cfg(target_os = "android")]
            init_certificate_verifier();

            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            // The device key sits in the Android Keystore or the iOS Keychain;
            // the vault's cipher seals the sync account and the saved
            // passwords and keys in the store.
            let vault = build_vault(tauri_plugin_device_key::bridge(app.handle()));
            // The v1 JSON file never exists on a phone; its path only names
            // the folder the database lives in.
            let opened = open_app_store_with(&dir.join("config.json"), vault.cipher())?;
            vault.load_settings(&opened.store);
            let store = Arc::new(Mutex::new(opened.store));
            let secrets = Arc::new(VaultSecrets::new(vault.clone(), store.clone()));
            mobile_vault::start_idle_timer(app.handle().clone(), vault.clone());

            let cache = app.path().app_cache_dir()?;
            let downloads = cache.join("downloads");
            let staging = Staging::new(cache.join("uploads"));
            std::fs::create_dir_all(&downloads)?;
            sweep(&downloads, LEFTOVER_AGE);
            staging.sweep(LEFTOVER_AGE);

            // Engine events become Tauri events the web view listens to.
            let handle = app.handle().clone();
            let events: Events = Arc::new(move |name: &str, payload: Value| {
                let (name, payload) = if name == "tunnel.status" {
                    ("tunnel-status".to_owned(), ui_status_event(&payload))
                } else {
                    (ui_event_name(name), payload)
                };
                let _ = handle.emit(&name, payload);
                1usize
            });
            let prompts = Prompts::new(events.clone());
            let engine = Engine::new(
                prompts,
                Arc::new(StoreKnownHosts::new(store.clone())),
                events,
            );

            let handle = app.handle().clone();
            let sync = tauri::async_runtime::block_on(async {
                SyncService::start(
                    store.clone(),
                    vault.clone(),
                    SyncOptions::default(),
                    Arc::new(move |name: &str, payload: Value| {
                        let pulled = payload["last"]["pulled"].as_u64().unwrap_or(0)
                            + payload["last"]["merged"].as_u64().unwrap_or(0);
                        let _ = handle.emit(&ui_event_name(name), payload);
                        if name == "sync.state" && pulled > 0 {
                            let _ = handle.emit("hosts-changed", ());
                        }
                    }),
                )
            });

            let sessions = Supervisor::new(engine.clone());
            app.manage(Mobile {
                store,
                engine,
                sessions,
                sync,
                vault,
                secrets,
                staging,
                downloads,
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build AnixOps SSH")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                let state = app.state::<Mobile>();
                state.sessions.close_all();
                let engine = state.engine.clone();
                tauri::async_runtime::block_on(async move { engine.shutdown().await });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_engine::local_files::safe_file_name;

    #[test]
    fn remote_paths_are_joined_with_one_slash() {
        assert_eq!(remote_join("/a", "b"), "/a/b");
        assert_eq!(remote_join("/a/", "b"), "/a/b");
        assert_eq!(remote_join("/", "b"), "/b");
        assert_eq!(safe_file_name("x/y"), "x_y");
    }
}
