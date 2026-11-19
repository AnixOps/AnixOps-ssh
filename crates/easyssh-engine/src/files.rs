//! The file pane and transfers on the built-in engine: `sftp.*` and
//! `transfer.*`. An SFTP session is a channel of the host's shared
//! connection; every transfer runs as its own task so it can be cancelled.

use crate::events::Events;
use crate::local_files::safe_file_name;
use crate::Lease;
use easyssh_ssh::{DirEntry, EntryKind, Error as SshError, FileStat, Sftp};
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
    /// Where this transfer reports to.
    events: Events,
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

/// `ls -l` style permissions: `drwxr-xr-x`.
pub fn mode_string(kind: EntryKind, permissions: Option<u32>) -> String {
    let mut text = String::from(match kind {
        EntryKind::Dir => 'd',
        EntryKind::File => '-',
        EntryKind::Symlink => 'l',
        EntryKind::Other => '?',
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

/// A listing row as the file browsers show it (the UI's `RemoteFile`):
/// `kind` is `directory`, `file`, `symlink` or `other`, `modified` reads like
/// `ls -l` (`Oct 14 09:12`, or `Oct 14  2025` when it is older than half a
/// year), in the local time zone.
pub fn remote_file_json(entry: &DirEntry) -> Value {
    let kind = match entry.stat.kind {
        EntryKind::File => "file",
        EntryKind::Dir => "directory",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    };
    let modified = entry
        .stat
        .modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .and_then(|age| chrono::DateTime::from_timestamp(age.as_secs() as i64, 0))
        .map(|time| {
            let local = time.with_timezone(&chrono::Local);
            if (chrono::Utc::now() - time).num_days().abs() < 180 {
                local.format("%b %e %H:%M").to_string()
            } else {
                local.format("%b %e  %Y").to_string()
            }
        })
        .unwrap_or_default();
    json!({
        "name": entry.name,
        "kind": kind,
        "size": entry.stat.size,
        "modified": modified,
        "permissions": mode_string(entry.stat.kind, entry.stat.permissions),
    })
}

/// Names from the server must not point outside the directory they are
/// listed in. Characters that are only invalid on this side (`:` on
/// Windows, say) are fine here; a download maps them with
/// [`safe_file_name`].
fn safe_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
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
        broadcast: Events,
    ) -> String {
        let id = self.id("transfer");
        let events = broadcast.clone();
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
            let skipped = progress.skipped;
            let (status, error) = match result {
                Ok(()) => ("completed", None),
                Err(e) => {
                    if let Some(partial) = &transfer.partial {
                        remove_partial(partial);
                    }
                    ("failed", Some(e.to_string()))
                }
            };
            let mut done = json!({"transfer_id": task_id, "status": status, "error": error});
            if skipped > 0 {
                done["skipped"] = json!(skipped);
            }
            broadcast.emit("transfer.done", done);
        });
        lock(&self.transfers).insert(
            id.clone(),
            Transfer {
                abort: task.abort_handle(),
                events,
                partial: (!upload).then_some(local),
            },
        );
        let _ = go.send(());
        id
    }

    /// `transfer.cancel`. Unknown or finished transfers are ignored. The
    /// end is reported to the sink the transfer was started with.
    pub fn cancel_transfer(&self, id: &str) {
        let Some(transfer) = lock(&self.transfers).remove(id) else {
            return;
        };
        transfer.abort.abort();
        if let Some(partial) = &transfer.partial {
            remove_partial(partial);
        }
        transfer.events.emit(
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
    broadcast: Events,
    bytes: u64,
    reported: u64,
    last: Instant,
    /// Links in a folder transfer that point nowhere, or back up into a
    /// folder that holds them.
    skipped: u64,
}

impl Progress {
    fn new(id: String, broadcast: Events) -> Self {
        Progress {
            id,
            broadcast,
            bytes: 0,
            reported: 0,
            last: Instant::now(),
            skipped: 0,
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
            self.broadcast.emit(
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
    drop(file);
    // Like `put -p`: the copy keeps the permissions and the modification
    // time. A server that refuses leaves an upload that is still complete.
    if let Ok(metadata) = source.metadata().await {
        let _ = sftp
            .set_stat(remote, local_mode(&metadata), metadata.modified().ok())
            .await;
    }
    Ok(())
}

/// The permission bits of a local file; Windows has none to give.
fn local_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// Gives a downloaded file the permission bits and modification time of
/// the remote one, like `get -p`.
fn preserve_local(file: &std::fs::File, stat: &FileStat) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(mode) = stat.permissions {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(mode & 0o777))?;
    }
    if let Some(modified) = stat.modified {
        file.set_modified(modified)?;
    }
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
    // Each folder carries the real paths of the folders above it, so a link
    // back up one of them is not followed round in circles.
    let root = tokio::fs::canonicalize(local_path)
        .await
        .map_err(|e| local(e, local_path))?;
    let mut pending = vec![(local_path.to_owned(), remote.to_owned(), vec![root])];
    while let Some((dir, remote_dir, above)) = pending.pop() {
        // An existing directory is fine.
        if let Err(e) = sftp.mkdir(&remote_dir).await {
            if !sftp
                .stat(&remote_dir)
                .await
                .is_ok_and(|s| s.kind == EntryKind::Dir)
            {
                return Err(e);
            }
        }
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .map_err(|e| local(e, &dir))?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| local(e, &dir))? {
            let name = entry.file_name().to_string_lossy().into_owned();
            let target = join_remote(&remote_dir, &name);
            let path = entry.path();
            let mut kind = entry.file_type().await.map_err(|e| local(e, &dir))?;
            if kind.is_symlink() {
                // A link is sent as what it points to.
                match tokio::fs::metadata(&path).await {
                    Ok(metadata) => kind = metadata.file_type(),
                    Err(_) => {
                        progress.skipped += 1;
                        continue;
                    }
                }
            }
            if kind.is_dir() {
                match tokio::fs::canonicalize(&path).await {
                    Ok(real) if !above.contains(&real) => {
                        pending.push((path, target, [&above[..], &[real]].concat()))
                    }
                    _ => progress.skipped += 1,
                }
            } else if kind.is_file() {
                upload_file(sftp, &path, &target, progress).await?;
            }
            // Special files (sockets, devices) are not sent.
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
    let stat = sftp.stat(remote).await?;
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
    let sink = sink.into_std().await;
    preserve_local(&sink, &stat).map_err(|e| local(e, local_path))?;
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
    // Each folder carries the real paths of the folders above it, so a link
    // back up one of them is not followed round in circles.
    let root = sftp.realpath(remote).await?;
    let mut pending = vec![(remote.to_owned(), local_path.to_owned(), vec![root])];
    while let Some((remote_dir, dir, above)) = pending.pop() {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| local(e, &dir))?;
        for entry in sftp.list(&remote_dir).await? {
            if !safe_name(&entry.name) {
                continue;
            }
            let source = join_remote(&remote_dir, &entry.name);
            let target = dir.join(safe_file_name(&entry.name));
            let kind = match entry.stat.kind {
                // A link is fetched as what it points to.
                EntryKind::Symlink => match sftp.stat(&source).await {
                    Ok(stat) => stat.kind,
                    Err(_) => {
                        progress.skipped += 1;
                        continue;
                    }
                },
                kind => kind,
            };
            match kind {
                EntryKind::Dir => match sftp.realpath(&source).await {
                    Ok(real) if !above.contains(&real) => {
                        pending.push((source, target, [&above[..], &[real]].concat()))
                    }
                    _ => progress.skipped += 1,
                },
                EntryKind::File => download_file(sftp, &source, &target, progress).await?,
                _ => {}
            }
        }
    }
    Ok(())
}

impl SftpEntry {
    /// The listing as the mobile file browser shows it: directories first,
    /// then by name, without the names that cannot be safely joined.
    pub async fn list_view(&self, path: &str) -> Result<Vec<Value>, SshError> {
        let mut entries = self.sftp.list(path).await?;
        entries.retain(|entry| safe_name(&entry.name));
        entries.sort_by(|a, b| {
            let dir = |e: &DirEntry| e.stat.kind == EntryKind::Dir;
            dir(b)
                .cmp(&dir(a))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(entries.iter().map(remote_file_json).collect())
    }

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
    use std::time::SystemTime;

    #[test]
    fn remote_names_cannot_escape() {
        assert!(safe_name("report.pdf"));
        // Fine on the server; a download maps them for the local side.
        for odd in ["a:b.log", "C:evil", "back\\slash", "what?"] {
            assert!(safe_name(odd), "{odd:?}");
        }
        assert_eq!(safe_file_name("a:b.log"), "a_b.log");
        assert_eq!(safe_file_name("..\\x"), ".._x");
        for bad in ["", ".", "..", "a/b", "../x", "a\0b"] {
            assert!(!safe_name(bad), "{bad:?}");
        }
    }

    #[test]
    fn downloads_take_the_remote_mode_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.sh");
        let file = std::fs::File::create(&path).unwrap();
        let modified = UNIX_EPOCH + Duration::from_secs(1_577_934_245);
        let stat = FileStat {
            kind: EntryKind::File,
            size: 0,
            modified: Some(modified),
            permissions: Some(0o4755),
            uid: None,
            gid: None,
        };
        preserve_local(&file, &stat).unwrap();
        let metadata = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.modified().unwrap(), modified);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Only the permission bits, never set-user-ID.
            assert_eq!(metadata.permissions().mode() & 0o7777, 0o755);
            assert_eq!(local_mode(&metadata), Some(0o755));
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
    fn listing_rows_look_like_ls_output() {
        assert_eq!(mode_string(EntryKind::Dir, Some(0o755)), "drwxr-xr-x");
        assert_eq!(mode_string(EntryKind::File, Some(0o640)), "-rw-r-----");
        assert_eq!(mode_string(EntryKind::File, None), "----------");
        assert_eq!(mode_string(EntryKind::Symlink, Some(0o777)), "lrwxrwxrwx");
        let row = remote_file_json(&DirEntry {
            name: "a.txt".into(),
            stat: easyssh_ssh::FileStat {
                kind: EntryKind::File,
                size: 12,
                modified: Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
                permissions: Some(0o644),
                uid: None,
                gid: None,
            },
        });
        assert_eq!(row["name"], "a.txt");
        assert_eq!(row["kind"], "file");
        assert_eq!(row["size"], 12);
        assert_eq!(row["permissions"], "-rw-r--r--");
        // Older than half a year: month, day and year.
        assert!(row["modified"].as_str().unwrap().ends_with(" 2023"));
        let dir = remote_file_json(&DirEntry {
            name: "d".into(),
            stat: easyssh_ssh::FileStat {
                kind: EntryKind::Dir,
                size: 0,
                modified: None,
                permissions: None,
                uid: None,
                gid: None,
            },
        });
        assert_eq!(dir["kind"], "directory");
        assert_eq!(dir["modified"], "");
    }

    #[test]
    fn progress_is_throttled_but_flushed() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let mut progress = Progress::new(
            "t".into(),
            Arc::new(move |kind: &str, payload: Value| {
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
