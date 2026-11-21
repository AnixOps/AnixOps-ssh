//! File pane commands: browsing through a long-lived `sftp` per pane, and
//! transfers in their own `sftp` processes so each can be cancelled.

use crate::daemon::{self, DaemonState};
use crate::{askpass, hosts};
use easyssh_core::sftp::{self, RemoteFile, RemoteFileKind, SftpSession};
use easyssh_core::{OpenSsh, OpenSshError};
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use tauri::{ipc::Channel, AppHandle, Manager, State};

struct RunningTransfer {
    /// None while the transfer waits in its host's queue.
    child: Mutex<Option<Child>>,
    cancelled: AtomicBool,
    gate: Arc<Gate>,
}

/// The transfer queue of one host for the OpenSSH backend: at most
/// [`easyssh_core::transfer::concurrent_transfers`] `sftp` processes run,
/// the others wait in the order they were started.
struct Gate {
    state: Mutex<GateState>,
    changed: Condvar,
    limit: usize,
}

#[derive(Default)]
struct GateState {
    running: usize,
    next: u64,
    waiting: VecDeque<u64>,
}

impl Gate {
    fn new(limit: usize) -> Self {
        Gate {
            state: Mutex::default(),
            changed: Condvar::new(),
            limit,
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Takes a place in the queue.
    fn join(&self) -> u64 {
        let mut state = self.state();
        state.next += 1;
        let ticket = state.next;
        state.waiting.push_back(ticket);
        ticket
    }

    /// Runs now if it is `ticket`'s turn and there is room.
    fn try_enter(&self, ticket: u64) -> bool {
        let mut state = self.state();
        if state.running < self.limit && state.waiting.front() == Some(&ticket) {
            state.waiting.pop_front();
            state.running += 1;
            true
        } else {
            false
        }
    }

    /// Waits for the turn of `ticket`; false when `cancelled` came first.
    fn wait(&self, ticket: u64, cancelled: &AtomicBool) -> bool {
        let mut state = self.state();
        loop {
            if cancelled.load(Ordering::Relaxed) {
                state.waiting.retain(|t| *t != ticket);
                drop(state);
                self.changed.notify_all();
                return false;
            }
            if state.running < self.limit && state.waiting.front() == Some(&ticket) {
                state.waiting.pop_front();
                state.running += 1;
                return true;
            }
            state = self
                .changed
                .wait_timeout(state, Duration::from_millis(250))
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }

    fn leave(&self) {
        let mut state = self.state();
        state.running = state.running.saturating_sub(1);
        drop(state);
        self.changed.notify_all();
    }

    fn wake(&self) {
        self.changed.notify_all();
    }
}

/// Transfers the daemon runs (built-in engine): where their events go.
#[derive(Default)]
struct RemoteTransfers {
    channels: HashMap<String, Channel<TransferEvent>>,
    /// Events that arrived before the transfer was registered.
    early: HashMap<String, Vec<TransferEvent>>,
}

#[derive(Default)]
pub struct Files {
    sessions: Mutex<HashMap<String, Arc<Mutex<SftpSession>>>>,
    transfers: Mutex<HashMap<String, Arc<RunningTransfer>>>,
    /// File sessions that live in the daemon.
    remote_sessions: Mutex<HashSet<String>>,
    gates: Mutex<HashMap<String, Arc<Gate>>>,
    remote_transfers: Mutex<RemoteTransfers>,
}

impl Files {
    fn gate(&self, host_id: &str) -> Arc<Gate> {
        let mut gates = self.gates.lock().unwrap_or_else(|p| p.into_inner());
        gates
            .entry(host_id.to_owned())
            .or_insert_with(|| Arc::new(Gate::new(easyssh_core::transfer::concurrent_transfers())))
            .clone()
    }

    fn is_remote(&self, id: &str) -> bool {
        self.remote_sessions
            .lock()
            .map(|sessions| sessions.contains(id))
            .unwrap_or(false)
    }
}

fn is_final(event: &TransferEvent) -> bool {
    !matches!(
        event,
        TransferEvent::Progress { .. }
            | TransferEvent::Started
            | TransferEvent::Note { .. }
            | TransferEvent::Verify { .. }
    )
}

/// Delivers a daemon transfer event to the pane that started it.
pub fn remote_event(app: &AppHandle, transfer_id: &str, event: TransferEvent) {
    let files = app.state::<Files>();
    let Ok(mut transfers) = files.remote_transfers.lock() else {
        return;
    };
    let target = transfers.channels.get(transfer_id).cloned();
    let Some(channel) = target else {
        // The start reply has not been handled yet.
        transfers
            .early
            .entry(transfer_id.to_owned())
            .or_default()
            .push(event);
        return;
    };
    let _ = channel.send(event.clone());
    if is_final(&event) {
        transfers.channels.remove(transfer_id);
    }
}

fn daemon_link(daemon: &DaemonState) -> Result<easysshd::link::Link, String> {
    daemon
        .link()
        .ok_or_else(|| "the connection to the session daemon was lost".to_owned())
}

/// `ls -l` style permissions from an SFTP entry.
fn mode_string(kind: RemoteFileKind, permissions: Option<u32>) -> String {
    let mut text = String::from(match kind {
        RemoteFileKind::Directory => 'd',
        RemoteFileKind::File => '-',
        RemoteFileKind::Symlink => 'l',
        RemoteFileKind::Other => '?',
    });
    let bits = permissions.unwrap_or(0);
    for (index, symbol) in "rwxrwxrwx".chars().enumerate() {
        let set = bits & (1 << (8 - index)) != 0;
        // Setuid, setgid and sticky replace the execute column, as in `ls -l`.
        let special = match index {
            2 => Some(('s', 'S', 0o4000)),
            5 => Some(('s', 'S', 0o2000)),
            8 => Some(('t', 'T', 0o1000)),
            _ => None,
        };
        text.push(match special {
            Some((on, off, bit)) if bits & bit != 0 => {
                if set {
                    on
                } else {
                    off
                }
            }
            _ if set => symbol,
            _ => '-',
        });
    }
    text
}

/// A daemon `SftpEntry` as the file pane lists it.
fn remote_file(entry: &serde_json::Value) -> RemoteFile {
    let kind = match entry["kind"].as_str() {
        Some("dir") => RemoteFileKind::Directory,
        Some("file") => RemoteFileKind::File,
        Some("symlink") => RemoteFileKind::Symlink,
        _ => RemoteFileKind::Other,
    };
    let modified = entry["modified"]
        .as_i64()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|time| {
            let local = time.with_timezone(&chrono::Local);
            if (chrono::Utc::now() - time).num_days().abs() < 180 {
                local.format("%b %e %H:%M").to_string()
            } else {
                local.format("%b %e  %Y").to_string()
            }
        })
        .unwrap_or_default();
    RemoteFile {
        name: entry["name"].as_str().unwrap_or_default().to_owned(),
        kind,
        size: entry["size"].as_u64().unwrap_or(0),
        modified,
        permissions: mode_string(kind, entry["permissions"].as_u64().map(|p| p as u32)),
        owner: entry["uid"].as_u64().map(|id| id.to_string()),
        group: entry["gid"].as_u64().map(|id| id.to_string()),
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

fn lock_error<T>(_: T) -> String {
    "file state is unavailable".to_owned()
}

async fn blocking<T, F>(task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| error.to_string())?
}

async fn on_session<T, F>(files: &Files, id: &str, task: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut SftpSession) -> Result<T, OpenSshError> + Send + 'static,
{
    let session = files
        .sessions
        .lock()
        .map_err(lock_error)?
        .get(id)
        .cloned()
        .ok_or("the file session has ended")?;
    blocking(move || {
        let mut session = session.lock().map_err(lock_error)?;
        task(&mut session).map_err(|error| error.to_string())
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedFiles {
    id: String,
    home: String,
}

#[tauri::command]
pub async fn sftp_open(
    app: AppHandle,
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    host_id: String,
) -> Result<OpenedFiles, String> {
    // Hosts on the built-in engine browse through the daemon, on the
    // connection the terminal already has.
    if let Some(link) = daemon::russh_link(&daemon, &host_id).await {
        match link
            .request("sftp.open", json!({ "host_id": host_id }))
            .await
        {
            Ok(answer) => {
                let id = answer["sftp_id"].as_str().unwrap_or_default().to_owned();
                let home = answer["home"].as_str().unwrap_or("/").to_owned();
                files
                    .remote_sessions
                    .lock()
                    .map_err(lock_error)?
                    .insert(id.clone());
                return Ok(OpenedFiles { id, home });
            }
            Err(error) if daemon::use_openssh(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    let connection = hosts::connection(&host_id)?;
    let env = askpass::env_for(&app, &connection.name, Some(&connection.id))?;
    let session = blocking(move || {
        SftpSession::connect(&OpenSsh, &connection, &env).map_err(|error| error.to_string())
    })
    .await?;
    let id = next_id();
    let home = session.home().to_owned();
    files
        .sessions
        .lock()
        .map_err(lock_error)?
        .insert(id.clone(), Arc::new(Mutex::new(session)));
    Ok(OpenedFiles { id, home })
}

#[tauri::command]
pub async fn sftp_list(
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    id: String,
    path: String,
) -> Result<Vec<RemoteFile>, String> {
    if files.is_remote(&id) {
        let answer = daemon_link(&daemon)?
            .request("sftp.list", json!({ "sftp_id": id, "path": path }))
            .await
            .map_err(|error| error.message)?;
        return Ok(answer["entries"]
            .as_array()
            .map(|entries| entries.iter().map(remote_file).collect())
            .unwrap_or_default());
    }
    on_session(&files, &id, move |session| session.list(&path)).await
}

#[tauri::command]
pub async fn sftp_mkdir(
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    id: String,
    path: String,
) -> Result<(), String> {
    if files.is_remote(&id) {
        return daemon_link(&daemon)?
            .request("sftp.mkdir", json!({ "sftp_id": id, "path": path }))
            .await
            .map(|_| ())
            .map_err(|error| error.message);
    }
    on_session(&files, &id, move |session| session.mkdir(&path)).await
}

#[tauri::command]
pub async fn sftp_remove(
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    id: String,
    path: String,
    directory: bool,
) -> Result<(), String> {
    if files.is_remote(&id) {
        return daemon_link(&daemon)?
            .request(
                "sftp.remove",
                json!({ "sftp_id": id, "path": path, "directory": directory }),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.message);
    }
    on_session(&files, &id, move |session| session.remove(&path, directory)).await
}

#[tauri::command]
pub async fn sftp_rename(
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    id: String,
    from: String,
    to: String,
) -> Result<(), String> {
    if files.is_remote(&id) {
        return daemon_link(&daemon)?
            .request(
                "sftp.rename",
                json!({ "sftp_id": id, "from": from, "to": to }),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.message);
    }
    on_session(&files, &id, move |session| session.rename(&from, &to)).await
}

#[tauri::command]
pub async fn sftp_chmod(
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    id: String,
    path: String,
    mode: u32,
    recursive: bool,
) -> Result<(), String> {
    if files.is_remote(&id) {
        return daemon_link(&daemon)?
            .request(
                "sftp.chmod",
                json!({ "sftp_id": id, "path": path, "mode": mode, "recursive": recursive }),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.message);
    }
    on_session(&files, &id, move |session| {
        session.chmod(&path, mode, recursive)
    })
    .await
}

#[tauri::command]
pub fn sftp_close(
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    id: String,
) -> Result<(), String> {
    if files
        .remote_sessions
        .lock()
        .map_err(lock_error)?
        .remove(&id)
    {
        if let Some(link) = daemon.link() {
            let _ = link.post("sftp.close", json!({ "sftp_id": id }));
        }
        return Ok(());
    }
    files.sessions.lock().map_err(lock_error)?.remove(&id);
    Ok(())
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum TransferEvent {
    /// A queued transfer's turn has come.
    Started,
    Progress {
        bytes: u64,
        total: Option<u64>,
    },
    /// How the transfer went on from an earlier try: `resumed`, or
    /// `source_changed` / `part_missing` (it began again from zero).
    Note {
        note: String,
        #[serde(rename = "resumedFrom")]
        resumed_from: Option<u64>,
    },
    /// The SHA-256 check of the transfer, before its end.
    Verify {
        status: String,
        files: u64,
        path: Option<String>,
        #[serde(rename = "localSha256")]
        local_sha256: Option<String>,
        #[serde(rename = "remoteSha256")]
        remote_sha256: Option<String>,
        detail: Option<String>,
    },
    Done,
    Failed {
        message: String,
    },
    Cancelled,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedTransfer {
    id: String,
    local: String,
    remote: String,
    /// Waits for others of the same host to finish first.
    queued: bool,
    /// A failed or cancelled try can be continued (the built-in engine) and
    /// its file can be checked with SHA-256.
    resumable: bool,
}

/// Uploads `local` into the remote directory `remote`, or downloads the
/// remote path `remote` into the user's Downloads folder (or, for a file
/// opened for editing, into `local` in the editor cache).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn transfer_start(
    app: AppHandle,
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    host_id: String,
    upload: bool,
    remote: String,
    local: Option<String>,
    resume: Option<bool>,
    verify: Option<bool>,
    events: Channel<TransferEvent>,
) -> Result<StartedTransfer, String> {
    let resume = resume.unwrap_or(false);
    let (local, remote) = if upload {
        let local = PathBuf::from(local.ok_or("no local file was given")?);
        let name = local
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("the local file name is not valid UTF-8")?;
        let remote = sftp::join_remote(&remote, name);
        (local, remote)
    } else {
        let name = remote
            .rsplit('/')
            .find(|part| !part.is_empty())
            .ok_or("the remote path has no file name")?;
        // A file opened for editing goes to its own copy; anything else to Downloads.
        let target = match local {
            Some(path) => {
                let path = PathBuf::from(path);
                // A resumed download goes back to the file an earlier try
                // chose in Downloads.
                let resumed_here = resume && path.parent() == dirs::download_dir().as_deref();
                if !resumed_here && !crate::edit::owns(&app, &path) {
                    return Err("the download target is not allowed".to_owned());
                }
                path
            }
            None => unique_download_path(&safe_file_name(name))?,
        };
        (target, remote)
    };
    if let Some(link) = daemon::russh_link(&daemon, &host_id).await {
        // Joins the host's transfer queue in the daemon: a few transfers run
        // at once on shared file sessions, the rest wait their turn.
        match link
            .request(
                "transfer.enqueue",
                json!({
                    "host_id": host_id,
                    "direction": if upload { "upload" } else { "download" },
                    "local_path": local.to_string_lossy(),
                    "remote_path": remote,
                    "recursive": true,
                    "resume": resume,
                    "verify": verify.unwrap_or(false),
                }),
            )
            .await
        {
            Ok(started) => {
                let id = started["transfer_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let queued = started["queued"].as_bool().unwrap_or(false);
                let early = {
                    let mut transfers = files.remote_transfers.lock().map_err(lock_error)?;
                    transfers.channels.insert(id.clone(), events.clone());
                    transfers.early.remove(&id).unwrap_or_default()
                };
                for event in early {
                    remote_event(&app, &id, event);
                }
                return Ok(StartedTransfer {
                    id,
                    local: local.to_string_lossy().into_owned(),
                    remote,
                    queued,
                    resumable: true,
                });
            }
            Err(error) if daemon::use_openssh(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    let connection = hosts::connection(&host_id)?;
    let env = askpass::env_for(&app, &connection.name, Some(&connection.id))?;
    let gate = files.gate(&host_id);
    let id = next_id();
    let ticket = gate.join();
    // The first transfers start at once, the rest wait their turn: every
    // `sftp` process is a session on the host (OpenSSH's `MaxSessions`).
    let queued = !gate.try_enter(ticket);
    let mut first = None;
    if !queued {
        match sftp::spawn_transfer(&OpenSsh, &connection, upload, &local, &remote, &env) {
            Ok(child) => first = Some(child),
            Err(error) => {
                gate.leave();
                return Err(error.to_string());
            }
        }
    }
    let transfer = Arc::new(RunningTransfer {
        child: Mutex::new(first),
        cancelled: AtomicBool::new(false),
        gate: gate.clone(),
    });
    files
        .transfers
        .lock()
        .map_err(lock_error)?
        .insert(id.clone(), transfer.clone());

    let local_path = local.clone();
    let remote_path = remote.clone();
    std::thread::spawn(move || {
        if queued {
            if !gate.wait(ticket, &transfer.cancelled) {
                // Cancelled in the queue: nothing was started.
                let _ = events.send(TransferEvent::Cancelled);
                return;
            }
            let _ = events.send(TransferEvent::Started);
            match sftp::spawn_transfer(
                &OpenSsh,
                &connection,
                upload,
                &local_path,
                &remote_path,
                &env,
            ) {
                Ok(child) => {
                    if let Ok(mut slot) = transfer.child.lock() {
                        *slot = Some(child);
                    }
                    // A cancel that came in between had no process to kill.
                    if transfer.cancelled.load(Ordering::Relaxed) {
                        if let Ok(mut slot) = transfer.child.lock() {
                            if let Some(child) = slot.as_mut() {
                                let _ = child.kill();
                            }
                        }
                    }
                }
                Err(error) => {
                    gate.leave();
                    let _ = events.send(TransferEvent::Failed {
                        message: error.to_string(),
                    });
                    return;
                }
            }
        }
        let errors = transfer
            .child
            .lock()
            .ok()
            .and_then(|mut slot| slot.as_mut().and_then(|child| child.stderr.take()))
            .map(|mut stderr| {
                std::thread::spawn(move || {
                    let mut text = String::new();
                    let _ = stderr.read_to_string(&mut text);
                    text
                })
            });
        let event = watch(&transfer, &local_path, upload, &events);
        gate.leave();
        let event = match event {
            Ok(()) => TransferEvent::Done,
            Err(_) if transfer.cancelled.load(Ordering::Relaxed) => {
                if !upload {
                    remove_partial(&local_path);
                }
                TransferEvent::Cancelled
            }
            Err(message) => {
                let detail = errors
                    .and_then(|reader| reader.join().ok())
                    .unwrap_or_default();
                let detail = detail.trim();
                TransferEvent::Failed {
                    message: if detail.is_empty() {
                        message
                    } else {
                        detail.to_owned()
                    },
                }
            }
        };
        let _ = events.send(event);
    });

    Ok(StartedTransfer {
        id,
        local: local.to_string_lossy().into_owned(),
        remote,
        queued,
        // The `sftp` program writes straight to the final name and has no
        // record of what it started from, so a try cannot be continued.
        resumable: false,
    })
}

/// The final remote path of a transfer: an upload names the file in the
/// directory `remote`, a download is `remote` itself.
fn final_remote(upload: bool, remote: &str, local: Option<&str>) -> Result<String, String> {
    if !upload {
        return Ok(remote.to_owned());
    }
    let name = Path::new(local.ok_or("no local file was given")?)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("the local file name is not valid UTF-8")?
        .to_owned();
    Ok(sftp::join_remote(remote, &name))
}

/// Do partial files of an earlier try of this transfer exist? Reports
/// `{found, files, bytes, size, localPath}`; nothing is found for a host on
/// the OpenSSH engine.
#[tauri::command]
pub async fn transfer_partial(
    daemon: State<'_, DaemonState>,
    host_id: String,
    upload: bool,
    remote: String,
    local: Option<String>,
) -> Result<serde_json::Value, String> {
    let Some(link) = daemon::russh_link(&daemon, &host_id).await else {
        return Ok(json!({ "found": false }));
    };
    let remote = final_remote(upload, &remote, local.as_deref())?;
    let answer = link
        .request(
            "transfer.partial",
            json!({
                "host_id": host_id,
                "direction": if upload { "upload" } else { "download" },
                "local_path": if upload { local } else { None },
                "remote_path": remote,
            }),
        )
        .await
        .map_err(|error| error.message)?;
    Ok(json!({
        "found": answer["found"].as_bool().unwrap_or(false),
        "files": answer["files"].as_u64().unwrap_or(0),
        "bytes": answer["bytes"].as_u64().unwrap_or(0),
        "size": answer["size"].as_u64().unwrap_or(0),
        "localPath": answer["local_path"],
    }))
}

/// Removes the partial files of this transfer, here and on the server.
#[tauri::command]
pub async fn transfer_discard(
    daemon: State<'_, DaemonState>,
    host_id: String,
    upload: bool,
    remote: String,
    local: Option<String>,
) -> Result<(), String> {
    let Some(link) = daemon::russh_link(&daemon, &host_id).await else {
        return Ok(());
    };
    let remote = final_remote(upload, &remote, local.as_deref())?;
    link.request(
        "transfer.discard",
        json!({
            "host_id": host_id,
            "direction": if upload { "upload" } else { "download" },
            "local_path": if upload { local } else { None },
            "remote_path": remote,
        }),
    )
    .await
    .map_err(|error| error.message)?;
    Ok(())
}

/// Compares SHA-256 of a local file and the remote file `remote` (final
/// paths). Reports `{status, files, path?, local_sha256?, remote_sha256?,
/// detail?}`; `status` is `match`, `mismatch` or `unavailable`.
#[tauri::command]
pub async fn transfer_verify(
    daemon: State<'_, DaemonState>,
    host_id: String,
    local: String,
    remote: String,
) -> Result<serde_json::Value, String> {
    if Path::new(&local).is_dir() {
        return Err("only a single file can be checked".to_owned());
    }
    let Some(link) = daemon::russh_link(&daemon, &host_id).await else {
        return Ok(json!({
            "status": "unavailable",
            "files": 0,
            "detail": "the host uses the OpenSSH engine",
        }));
    };
    link.request(
        "transfer.verify",
        json!({ "host_id": host_id, "local_path": local, "remote_path": remote }),
    )
    .await
    .map_err(|error| error.message)
}

/// Waits for the transfer, reporting the size of a download as it grows.
fn watch(
    transfer: &RunningTransfer,
    local: &Path,
    upload: bool,
    events: &Channel<TransferEvent>,
) -> Result<(), String> {
    let mut reported = None;
    loop {
        let status = match transfer.child.lock().map_err(lock_error)?.as_mut() {
            Some(child) => child.try_wait().map_err(|error| error.to_string())?,
            None => return Err("the transfer has no process".to_owned()),
        };
        if let Some(status) = status {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("sftp exited with {status}"))
            };
        }
        if !upload {
            if let Ok(metadata) = std::fs::metadata(local) {
                let bytes = metadata.len();
                if metadata.is_file() && reported != Some(bytes) {
                    reported = Some(bytes);
                    let _ = events.send(TransferEvent::Progress { bytes, total: None });
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[tauri::command]
pub fn transfer_cancel(
    files: State<'_, Files>,
    daemon: State<'_, DaemonState>,
    id: String,
) -> Result<(), String> {
    let remote = files
        .remote_transfers
        .lock()
        .map_err(lock_error)?
        .channels
        .contains_key(&id);
    if remote {
        if let Some(link) = daemon.link() {
            let _ = link.post("transfer.cancel", json!({ "transfer_id": id }));
        }
        return Ok(());
    }
    let transfer = files.transfers.lock().map_err(lock_error)?.remove(&id);
    if let Some(transfer) = transfer {
        transfer.cancelled.store(true, Ordering::Relaxed);
        if let Some(child) = transfer.child.lock().map_err(lock_error)?.as_mut() {
            let _ = child.kill();
        }
        // A transfer in the queue notices at once.
        transfer.gate.wake();
    }
    Ok(())
}

/// Removes what a cancelled download was writing. A file is removed; of a
/// folder only the file written last, the one cut off, so the files that
/// finished before the cancel stay.
fn remove_partial(path: &Path) {
    if path.is_dir() {
        if let Some(file) = newest_file(path) {
            let _ = std::fs::remove_file(file);
        }
    } else {
        let _ = std::fs::remove_file(path);
    }
}

/// The most recently modified regular file under `dir`, links not followed.
fn newest_file(dir: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut pending = vec![dir.to_owned()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                pending.push(entry.path());
            } else if metadata.is_file() {
                let modified = metadata.modified().unwrap_or(std::time::UNIX_EPOCH);
                if newest.as_ref().is_none_or(|(time, _)| modified >= *time) {
                    newest = Some((modified, entry.path()));
                }
            }
        }
    }
    newest.map(|(_, path)| path)
}

/// Remote names come from the server, so they must not be able to point
/// outside the Downloads folder.
fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    if matches!(cleaned.trim(), "" | "." | "..") {
        "download".to_owned()
    } else {
        cleaned
    }
}

fn unique_download_path(name: &str) -> Result<PathBuf, String> {
    let dir = dirs::download_dir()
        .or_else(dirs::home_dir)
        .ok_or("no Downloads folder was found")?;
    let candidate = dir.join(name);
    if !candidate.exists() {
        return Ok(candidate);
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let extension = path.extension().and_then(|extension| extension.to_str());
    for number in 1..1000 {
        let file = match extension {
            Some(extension) => format!("{stem} ({number}).{extension}"),
            None => format!("{stem} ({number})"),
        };
        let candidate = dir.join(file);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err("too many files with the same name".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        mode_string, newest_file, remote_file, remove_partial, safe_file_name, RemoteFileKind,
    };

    #[test]
    fn daemon_entries_look_like_ls_listings() {
        assert_eq!(
            mode_string(RemoteFileKind::Directory, Some(0o755)),
            "drwxr-xr-x"
        );
        assert_eq!(mode_string(RemoteFileKind::File, Some(0o640)), "-rw-r-----");
        assert_eq!(mode_string(RemoteFileKind::File, None), "----------");
        let file = remote_file(&serde_json::json!({
            "name": "a.txt", "kind": "file", "size": 12,
            "modified": 1_700_000_000, "permissions": 420
        }));
        assert_eq!(file.name, "a.txt");
        assert_eq!(file.kind, RemoteFileKind::File);
        assert_eq!(file.size, 12);
        assert_eq!(file.permissions, "-rw-r--r--");
        assert_eq!(
            mode_string(RemoteFileKind::File, Some(0o4755)),
            "-rwsr-xr-x"
        );
        assert_eq!(
            mode_string(RemoteFileKind::Directory, Some(0o1777)),
            "drwxrwxrwt"
        );
        let owned = remote_file(&serde_json::json!({
            "name": "b", "kind": "file", "size": 1, "modified": null,
            "permissions": 420, "uid": 1000, "gid": 100
        }));
        assert_eq!(owned.owner.as_deref(), Some("1000"));
        assert_eq!(owned.group.as_deref(), Some("100"));
        assert!(!file.modified.is_empty());
    }

    #[test]
    fn remote_names_cannot_escape_the_downloads_folder() {
        assert_eq!(safe_file_name("..\\..\\evil.exe"), ".._.._evil.exe");
        assert_eq!(safe_file_name(".."), "download");
        assert_eq!(safe_file_name("C:evil"), "C_evil");
        assert_eq!(safe_file_name("report 2026.pdf"), "report 2026.pdf");
    }

    #[test]
    fn a_cancelled_folder_download_keeps_the_files_that_finished() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("logs");
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::write(root.join("old/a.log"), "done").unwrap();
        std::fs::write(root.join("b.log"), "done").unwrap();
        let cut = root.join("old/c.log");
        std::fs::write(&cut, "cut o").unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&cut)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert_eq!(newest_file(&root).as_deref(), Some(cut.as_path()));
        remove_partial(&root);
        assert!(!cut.exists());
        assert!(root.join("old/a.log").exists());
        assert!(root.join("b.log").exists());
        // A single file is removed.
        remove_partial(&root.join("b.log"));
        assert!(!root.join("b.log").exists());
    }
}
