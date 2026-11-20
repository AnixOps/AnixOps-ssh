//! File pane commands: browsing through a long-lived `sftp` per pane, and
//! transfers in their own `sftp` processes so each can be cancelled.

use crate::daemon::{self, DaemonState};
use crate::{askpass, hosts};
use easyssh_core::sftp::{self, RemoteFile, RemoteFileKind, SftpSession};
use easyssh_core::{OpenSsh, OpenSshError};
use serde::Serialize;
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{ipc::Channel, AppHandle, Manager, State};

struct RunningTransfer {
    child: Mutex<Child>,
    cancelled: AtomicBool,
}

/// Transfers the daemon runs (built-in engine): where their events go.
#[derive(Default)]
struct RemoteTransfers {
    channels: HashMap<String, (Channel<TransferEvent>, String)>,
    /// Events that arrived before the transfer was registered.
    early: HashMap<String, Vec<TransferEvent>>,
}

#[derive(Default)]
pub struct Files {
    sessions: Mutex<HashMap<String, Arc<Mutex<SftpSession>>>>,
    transfers: Mutex<HashMap<String, Arc<RunningTransfer>>>,
    /// File sessions that live in the daemon.
    remote_sessions: Mutex<HashSet<String>>,
    remote_transfers: Mutex<RemoteTransfers>,
}

impl Files {
    fn is_remote(&self, id: &str) -> bool {
        self.remote_sessions
            .lock()
            .map(|sessions| sessions.contains(id))
            .unwrap_or(false)
    }
}

fn is_final(event: &TransferEvent) -> bool {
    !matches!(event, TransferEvent::Progress { .. })
}

/// Delivers a daemon transfer event to the pane that started it.
pub fn remote_event(app: &AppHandle, transfer_id: &str, event: TransferEvent) {
    let files = app.state::<Files>();
    let Ok(mut transfers) = files.remote_transfers.lock() else {
        return;
    };
    let target = transfers
        .channels
        .get(transfer_id)
        .map(|(channel, sftp_id)| (channel.clone(), sftp_id.clone()));
    let Some((channel, sftp_id)) = target else {
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
        drop(transfers);
        close_remote(app, &sftp_id);
    }
}

/// The transfer's own file session is no longer needed.
fn close_remote(app: &AppHandle, sftp_id: &str) {
    let files = app.state::<Files>();
    if let Ok(mut sessions) = files.remote_sessions.lock() {
        sessions.remove(sftp_id);
    }
    if let Some(link) = app.state::<DaemonState>().link() {
        let _ = link.post("sftp.close", json!({ "sftp_id": sftp_id }));
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
        text.push(if bits & (1 << (8 - index)) != 0 {
            symbol
        } else {
            '-'
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

/// Changes the permission bits of `path`, and of everything in it with `recursive`.
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
#[serde(
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    tag = "kind"
)]
pub enum TransferEvent {
    Progress {
        bytes: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        total: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        files_done: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        files_total: Option<u64>,
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
}

/// Uploads `local` into the remote directory `remote`, or downloads the
/// remote path `remote` into the user's Downloads folder.
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
    events: Channel<TransferEvent>,
) -> Result<StartedTransfer, String> {
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
        (unique_download_path(&safe_file_name(name))?, remote)
    };
    if let Some(link) = daemon::russh_link(&daemon, &host_id).await {
        // A file session of its own on the shared connection, closed when
        // the transfer ends.
        match link
            .request("sftp.open", json!({ "host_id": host_id }))
            .await
        {
            Ok(opened) => {
                let sftp_id = opened["sftp_id"].as_str().unwrap_or_default().to_owned();
                let started = link
                    .request(
                        "transfer.start",
                        json!({
                            "sftp_id": sftp_id,
                            "direction": if upload { "upload" } else { "download" },
                            "local_path": local.to_string_lossy(),
                            "remote_path": remote,
                            "recursive": true,
                        }),
                    )
                    .await;
                let started = match started {
                    Ok(started) => started,
                    Err(error) => {
                        let _ = link.post("sftp.close", json!({ "sftp_id": sftp_id }));
                        return Err(error.message);
                    }
                };
                let id = started["transfer_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                let early = {
                    let mut transfers = files.remote_transfers.lock().map_err(lock_error)?;
                    transfers
                        .channels
                        .insert(id.clone(), (events.clone(), sftp_id.clone()));
                    transfers.early.remove(&id).unwrap_or_default()
                };
                for event in early {
                    remote_event(&app, &id, event);
                }
                return Ok(StartedTransfer {
                    id,
                    local: local.to_string_lossy().into_owned(),
                    remote,
                });
            }
            Err(error) if daemon::use_openssh(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    let connection = hosts::connection(&host_id)?;
    let env = askpass::env_for(&app, &connection.name, Some(&connection.id))?;
    let mut child = sftp::spawn_transfer(&OpenSsh, &connection, upload, &local, &remote, &env)
        .map_err(|error| error.to_string())?;
    let stderr = child.stderr.take();
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut stderr) = stderr {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    });
    let id = next_id();
    let transfer = Arc::new(RunningTransfer {
        child: Mutex::new(child),
        cancelled: AtomicBool::new(false),
    });
    files
        .transfers
        .lock()
        .map_err(lock_error)?
        .insert(id.clone(), transfer.clone());

    let local_path = local.clone();
    std::thread::spawn(move || {
        let event = watch(&transfer, &local_path, upload, &events);
        let event = match event {
            Ok(()) => TransferEvent::Done,
            Err(_) if transfer.cancelled.load(Ordering::Relaxed) => {
                if !upload {
                    remove_partial(&local_path);
                }
                TransferEvent::Cancelled
            }
            Err(message) => {
                let detail = errors.join().unwrap_or_default();
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
    })
}

/// Waits for the transfer, reporting the size of a download as it grows.
fn watch(
    transfer: &RunningTransfer,
    local: &Path,
    upload: bool,
    events: &Channel<TransferEvent>,
) -> Result<(), String> {
    let mut reported = None;
    if upload {
        // OpenSSH's `sftp` reports nothing while uploading, but the size of
        // what goes up is known, so the pane can show it.
        if let Some((total, files)) = local_totals(local) {
            let _ = events.send(TransferEvent::Progress {
                bytes: 0,
                total: Some(total),
                files_done: Some(0),
                files_total: Some(files),
            });
        }
    }
    loop {
        let status = transfer
            .child
            .lock()
            .map_err(lock_error)?
            .try_wait()
            .map_err(|error| error.to_string())?;
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
                    let _ = events.send(TransferEvent::Progress {
                        bytes,
                        total: None,
                        files_done: None,
                        files_total: None,
                    });
                }
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Bytes and files under a local path, links not followed.
fn local_totals(path: &Path) -> Option<(u64, u64)> {
    let mut totals = (0, 0);
    let mut pending = vec![path.to_owned()];
    while let Some(current) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&current).ok()?;
        if metadata.is_file() {
            totals.0 += metadata.len();
            totals.1 += 1;
        } else if metadata.is_dir() {
            for entry in std::fs::read_dir(&current).ok()?.flatten() {
                pending.push(entry.path());
            }
        }
    }
    Some(totals)
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
        let _ = transfer.child.lock().map_err(lock_error)?.kill();
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
        assert!(!file.modified.is_empty());
    }

    #[test]
    fn local_totals_count_files_and_bytes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/x"), "12345").unwrap();
        std::fs::write(dir.path().join("a/b/y"), "123").unwrap();
        assert_eq!(super::local_totals(dir.path()), Some((8, 2)));
        assert_eq!(super::local_totals(&dir.path().join("a/x")), Some((5, 1)));
        assert_eq!(super::local_totals(&dir.path().join("none")), None);
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
