//! The file pane and transfers on the built-in engine: `sftp.*` and
//! `transfer.*`. An SFTP session is a channel of the host's shared
//! connection; every transfer runs as its own task so it can be cancelled.

use super::interact::Broadcast;
use super::Lease;
use easyssh_ssh::{DirEntry, EntryKind, Error as SshError, Sftp};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::AbortHandle;

const CHUNK: usize = 64 * 1024;
const PROGRESS_EVERY: Duration = Duration::from_millis(150);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// An open file session. Dropping it releases the connection lease.
pub struct SftpEntry {
    sftp: Sftp,
    _lease: Lease,
}

struct Transfer {
    abort: AbortHandle,
    /// What a cancelled or failed transfer leaves behind and should remove
    /// (downloads only).
    partial: Option<PathBuf>,
}

#[derive(Default)]
pub struct Files {
    sessions: Mutex<HashMap<String, Arc<SftpEntry>>>,
    transfers: Arc<Mutex<HashMap<String, Transfer>>>,
    next: AtomicU64,
}

/// The `SftpEntry` JSON of the protocol.
pub fn entry_json(entry: &DirEntry) -> Value {
    let kind = match entry.stat.kind {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    };
    json!({
        "name": entry.name,
        "kind": kind,
        "size": entry.stat.size,
        "modified": entry
            .stat
            .modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        "permissions": entry.stat.permissions,
    })
}

/// Names from the server must not point outside the directory they are
/// listed in.
fn safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains(['/', '\\', '\0'])
        && !name.contains(':')
}

fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

impl Files {
    fn id(&self, prefix: &str) -> String {
        format!(
            "{prefix}{}-{}",
            self.next.fetch_add(1, Ordering::Relaxed) + 1,
            uuid::Uuid::new_v4().simple()
        )
    }

    /// Registers an SFTP session on a leased connection.
    pub async fn open(&self, lease: Lease) -> Result<(String, String), SshError> {
        let sftp = lease.sftp().await?;
        let home = sftp.home().await?;
        let id = self.id("sftp");
        lock(&self.sessions).insert(
            id.clone(),
            Arc::new(SftpEntry {
                sftp,
                _lease: lease,
            }),
        );
        Ok((id, home))
    }

    pub fn get(&self, id: &str) -> Option<Arc<SftpEntry>> {
        lock(&self.sessions).get(id).cloned()
    }

    pub async fn close(&self, id: &str) {
        let entry = lock(&self.sessions).remove(id);
        if let Some(entry) = entry {
            let _ = entry.sftp.close().await;
        }
    }

    /// Cancels every transfer and forgets every session.
    pub fn stop_all(&self) {
        for (_, transfer) in lock(&self.transfers).drain() {
            transfer.abort.abort();
        }
        lock(&self.sessions).clear();
    }

    /// `transfer.start`. The transfer runs in the background; `transfer.progress`
    /// and `transfer.done` events report on it.
    pub fn start_transfer(
        &self,
        entry: Arc<SftpEntry>,
        upload: bool,
        local: PathBuf,
        remote: String,
        recursive: bool,
        broadcast: Broadcast,
    ) -> String {
        let id = self.id("transfer");
        let transfers = self.transfers.clone();
        let task_id = id.clone();
        let (go, gate) = tokio::sync::oneshot::channel::<()>();
        let task_local = local.clone();
        let task = tokio::spawn(async move {
            let _ = gate.await;
            let mut progress = Progress::new(task_id.clone(), broadcast.clone());
            let result = if upload {
                upload_path(&entry.sftp, &task_local, &remote, recursive, &mut progress).await
            } else {
                download_path(&entry.sftp, &remote, &task_local, recursive, &mut progress).await
            };
            progress.flush();
            // Whoever removes the entry reports the end: this task, or a
            // cancel that got there first.
            let Some(transfer) = lock(&transfers).remove(&task_id) else {
                return;
            };
            let (status, error) = match result {
                Ok(()) => ("completed", None),
                Err(e) => {
                    if let Some(partial) = &transfer.partial {
                        remove_partial(partial);
                    }
                    ("failed", Some(e.to_string()))
                }
            };
            broadcast(
                "transfer.done",
                json!({"transfer_id": task_id, "status": status, "error": error}),
            );
        });
        lock(&self.transfers).insert(
            id.clone(),
            Transfer {
                abort: task.abort_handle(),
                partial: (!upload).then_some(local),
            },
        );
        let _ = go.send(());
        id
    }

    /// `transfer.cancel`. Unknown or finished transfers are ignored.
    pub fn cancel_transfer(&self, id: &str, broadcast: &Broadcast) {
        let Some(transfer) = lock(&self.transfers).remove(id) else {
            return;
        };
        transfer.abort.abort();
        if let Some(partial) = &transfer.partial {
            remove_partial(partial);
        }
        broadcast(
            "transfer.done",
            json!({"transfer_id": id, "status": "cancelled", "error": null}),
        );
    }
}

fn remove_partial(path: &Path) {
    let _ = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
}

/// Reports bytes moved, at most every [`PROGRESS_EVERY`].
struct Progress {
    id: String,
    broadcast: Broadcast,
    bytes: u64,
    reported: u64,
    last: Instant,
}

impl Progress {
    fn new(id: String, broadcast: Broadcast) -> Self {
        Progress {
            id,
            broadcast,
            bytes: 0,
            reported: 0,
            last: Instant::now(),
        }
    }

    fn add(&mut self, bytes: u64) {
        self.bytes += bytes;
        if self.last.elapsed() >= PROGRESS_EVERY {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.bytes != self.reported {
            self.reported = self.bytes;
            (self.broadcast)(
                "transfer.progress",
                json!({"transfer_id": self.id, "bytes": self.bytes}),
            );
        }
        self.last = Instant::now();
    }
}

fn local(error: std::io::Error, path: &Path) -> SshError {
    SshError::Sftp(format!("{}: {error}", path.display()))
}

async fn upload_file(
    sftp: &Sftp,
    local_path: &Path,
    remote: &str,
    progress: &mut Progress,
) -> Result<(), SshError> {
    let mut source = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| local(e, local_path))?;
    let mut file = sftp.create(remote).await?;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = source
            .read(&mut buffer)
            .await
            .map_err(|e| local(e, local_path))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .await
            .map_err(|e| SshError::Sftp(format!("writing {remote}: {e}")))?;
        progress.add(read as u64);
    }
    file.shutdown()
        .await
        .map_err(|e| SshError::Sftp(format!("closing {remote}: {e}")))?;
    Ok(())
}

async fn upload_path(
    sftp: &Sftp,
    local_path: &Path,
    remote: &str,
    recursive: bool,
    progress: &mut Progress,
) -> Result<(), SshError> {
    let metadata = tokio::fs::metadata(local_path)
        .await
        .map_err(|e| local(e, local_path))?;
    if !metadata.is_dir() {
        return upload_file(sftp, local_path, remote, progress).await;
    }
    if !recursive {
        return Err(SshError::Sftp(format!(
            "{} is a directory; use a recursive transfer",
            local_path.display()
        )));
    }
    let mut pending = vec![(local_path.to_owned(), remote.to_owned())];
    while let Some((dir, remote_dir)) = pending.pop() {
        // An existing directory is fine.
        if let Err(e) = sftp.mkdir(&remote_dir).await {
            if !sftp.stat(&remote_dir).await.is_ok_and(|s| s.kind == EntryKind::Dir) {
                return Err(e);
            }
        }
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| local(e, &dir))?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| local(e, &dir))? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let target = join_remote(&remote_dir, &name);
            let kind = entry.file_type().await.map_err(|e| local(e, &dir))?;
            if kind.is_dir() {
                pending.push((entry.path(), target));
            } else if kind.is_file() {
                upload_file(sftp, &entry.path(), &target, progress).await?;
            }
            // Symbolic links and special files are not followed.
        }
    }
    Ok(())
}

async fn download_file(
    sftp: &Sftp,
    remote: &str,
    local_path: &Path,
    progress: &mut Progress,
) -> Result<(), SshError> {
    let mut file = sftp.open_read(remote).await?;
    let mut sink = tokio::fs::File::create(local_path)
        .await
        .map_err(|e| local(e, local_path))?;
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| SshError::Sftp(format!("reading {remote}: {e}")))?;
        if read == 0 {
            break;
        }
        sink.write_all(&buffer[..read])
            .await
            .map_err(|e| local(e, local_path))?;
        progress.add(read as u64);
    }
    sink.flush().await.map_err(|e| local(e, local_path))?;
    Ok(())
}

async fn download_path(
    sftp: &Sftp,
    remote: &str,
    local_path: &Path,
    recursive: bool,
    progress: &mut Progress,
) -> Result<(), SshError> {
    let stat = sftp.stat(remote).await?;
    if stat.kind != EntryKind::Dir {
        return download_file(sftp, remote, local_path, progress).await;
    }
    if !recursive {
        return Err(SshError::Sftp(format!(
            "{remote} is a directory; use a recursive transfer"
        )));
    }
    let mut pending = vec![(remote.to_owned(), local_path.to_owned())];
    while let Some((remote_dir, dir)) = pending.pop() {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| local(e, &dir))?;
        for entry in sftp.list(&remote_dir).await? {
            if !safe_name(&entry.name) {
                continue;
            }
            let source = join_remote(&remote_dir, &entry.name);
            let target = dir.join(&entry.name);
            match entry.stat.kind {
                EntryKind::Dir => pending.push((source, target)),
                EntryKind::File => download_file(sftp, &source, &target, progress).await?,
                _ => {}
            }
        }
    }
    Ok(())
}

impl SftpEntry {
    pub async fn list(&self, path: &str) -> Result<Value, SshError> {
        let entries = self.sftp.list(path).await?;
        Ok(json!({"entries": entries.iter().map(entry_json).collect::<Vec<_>>()}))
    }

    pub async fn mkdir(&self, path: &str) -> Result<(), SshError> {
        self.sftp.mkdir(path).await
    }

    pub async fn remove(&self, path: &str, directory: bool) -> Result<(), SshError> {
        if directory {
            self.sftp.remove_dir(path).await
        } else {
            self.sftp.remove_file(path).await
        }
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), SshError> {
        self.sftp.rename(from, to).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_ssh::FileStat;
    use std::time::SystemTime;

    #[test]
    fn remote_names_cannot_escape() {
        assert!(safe_name("report.pdf"));
        for bad in ["", ".", "..", "a/b", "..\\x", "C:evil", "a\0b"] {
            assert!(!safe_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn joins_remote_paths() {
        assert_eq!(join_remote("/a", "b"), "/a/b");
        assert_eq!(join_remote("/a/", "b"), "/a/b");
        assert_eq!(join_remote("/", "b"), "/b");
    }

    #[test]
    fn entries_follow_the_protocol_shape() {
        let entry = DirEntry {
            name: "x".into(),
            stat: FileStat {
                kind: EntryKind::Dir,
                size: 4096,
                modified: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                permissions: Some(0o755),
                uid: None,
                gid: None,
            },
        };
        let json = entry_json(&entry);
        assert_eq!(json["kind"], "dir");
        assert_eq!(json["modified"], 1_700_000_000u64);
        assert_eq!(json["permissions"], 0o755);
        let mut missing = entry.clone();
        missing.stat.modified = None::<SystemTime>;
        assert!(entry_json(&missing)["modified"].is_null());
        easyssh_proto::validate("SftpEntry", &entry_json(&entry)).unwrap();
    }

    #[test]
    fn progress_is_throttled_but_flushed() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let mut progress = Progress::new(
            "t".into(),
            Arc::new(move |kind, payload| {
                lock(&seen).push((kind.to_owned(), payload));
                1
            }),
        );
        progress.add(10);
        progress.add(10);
        assert!(lock(&events).is_empty(), "throttled");
        progress.flush();
        let events = lock(&events);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].1["bytes"], 20);
    }
}
