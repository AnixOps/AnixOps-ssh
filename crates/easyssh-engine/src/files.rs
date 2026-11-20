//! The file pane and transfers on the built-in engine: `sftp.*` and
//! `transfer.*`. An SFTP session is a channel of the host's shared
//! connection; every transfer runs as its own task so it can be cancelled.

use crate::events::Events;
use crate::Lease;
use easyssh_ssh::{DirEntry, EntryKind, Error as SshError, Sftp};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::runtime::Handle;
use tokio::task::JoinHandle;

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

/// The file a transfer is writing right now, under a temporary name next to
/// where it ends up. A cancelled or failed transfer removes it; everything
/// finished before it stays.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Partial {
    Local(PathBuf),
    Remote(String),
}

type Current = Arc<Mutex<Option<Partial>>>;

struct Transfer {
    task: JoinHandle<()>,
    runtime: Handle,
    /// Where this transfer reports to.
    events: Events,
    entry: Arc<SftpEntry>,
    current: Current,
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
    let mut json = json!({
        "name": entry.name,
        "kind": kind,
        "size": entry.stat.size,
        "modified": entry
            .stat
            .modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        "permissions": entry.stat.permissions,
    });
    if let Some(uid) = entry.stat.uid {
        json["uid"] = json!(uid);
    }
    if let Some(gid) = entry.stat.gid {
        json["gid"] = json!(gid);
    }
    json
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
            transfer.task.abort();
            // Remote leftovers would need the connection that is going away.
            if let Some(Partial::Local(path)) = lock(&transfer.current).take() {
                let _ = std::fs::remove_file(path);
            }
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
        let current = Current::default();
        let task_entry = entry.clone();
        let task_current = current.clone();
        let task = tokio::spawn(async move {
            let _ = gate.await;
            let mut job = Job {
                sftp: &task_entry.sftp,
                progress: Progress::new(task_id.clone(), broadcast.clone()),
                current: task_current.clone(),
            };
            // Size the work first so the pane can show a percentage and the
            // time left, for a folder as well as a file.
            let total = if upload {
                local_total(&local).await
            } else {
                remote_total(&task_entry.sftp, &remote).await
            };
            job.progress.announce(total);
            let result = if upload {
                upload_path(&mut job, &local, &remote, recursive).await
            } else {
                download_path(&mut job, &remote, &local, recursive).await
            };
            job.progress.flush();
            // Whoever removes the entry reports the end: this task, or a
            // cancel that got there first.
            if lock(&transfers).remove(&task_id).is_none() {
                return;
            }
            let (status, error) = match result {
                Ok(()) => ("completed", None),
                Err(e) => {
                    remove_partial(&task_entry.sftp, &task_current).await;
                    ("failed", Some(e.to_string()))
                }
            };
            broadcast.emit(
                "transfer.done",
                json!({"transfer_id": task_id, "status": status, "error": error}),
            );
        });
        lock(&self.transfers).insert(
            id.clone(),
            Transfer {
                task,
                runtime: Handle::current(),
                events,
                entry,
                current,
            },
        );
        let _ = go.send(());
        id
    }

    /// `transfer.cancel`. Unknown or finished transfers are ignored. The
    /// end is reported to the sink the transfer was started with, once the
    /// file it was writing is removed.
    pub fn cancel_transfer(&self, id: &str) {
        let Some(transfer) = lock(&self.transfers).remove(id) else {
            return;
        };
        let Transfer {
            task,
            runtime,
            events,
            entry,
            current,
        } = transfer;
        task.abort();
        let id = id.to_owned();
        runtime.spawn(async move {
            // Once the task has stopped it cannot create the file again.
            let _ = task.await;
            remove_partial(&entry.sftp, &current).await;
            events.emit(
                "transfer.done",
                json!({"transfer_id": id, "status": "cancelled", "error": null}),
            );
        });
    }
}

async fn remove_partial(sftp: &Sftp, current: &Current) {
    let partial = lock(current).take();
    match partial {
        Some(Partial::Local(path)) => {
            let _ = tokio::fs::remove_file(path).await;
        }
        Some(Partial::Remote(path)) => {
            let _ = sftp.remove_file(&path).await;
        }
        None => {}
    }
}

/// `dir/.name.easyssh-part`: where a file is written before it replaces
/// `dir/name`, so a transfer that stops halfway never leaves a cut-off file
/// under the real name.
fn remote_part(path: &str) -> String {
    let cut = path.rfind('/').map_or(0, |cut| cut + 1);
    format!("{}.{}.easyssh-part", &path[..cut], &path[cut..])
}

fn local_part(path: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(".easyssh-part");
    path.with_file_name(name)
}

/// What one transfer works with.
struct Job<'a> {
    sftp: &'a Sftp,
    progress: Progress,
    /// The file being written now, for a cancel or failure to remove.
    current: Current,
}

impl Job<'_> {
    fn writing(&self, partial: Option<Partial>) {
        *lock(&self.current) = partial;
    }
}

/// Reports bytes moved, at most every [`PROGRESS_EVERY`].
struct Progress {
    id: String,
    broadcast: Events,
    total: Option<u64>,
    bytes: u64,
    reported: u64,
    last: Instant,
}

impl Progress {
    fn new(id: String, broadcast: Events) -> Self {
        Progress {
            id,
            broadcast,
            total: None,
            bytes: 0,
            reported: 0,
            last: Instant::now(),
        }
    }

    /// Tells the client how much there is to move, before the first byte.
    fn announce(&mut self, total: Option<u64>) {
        self.total = total;
        if total.is_some() {
            self.emit();
        }
    }

    fn emit(&self) {
        let mut payload = json!({"transfer_id": self.id, "bytes": self.bytes});
        if let Some(total) = self.total {
            payload["total"] = json!(total);
        }
        self.broadcast.emit("transfer.progress", payload);
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
            self.emit();
        }
        self.last = Instant::now();
    }
}

/// Adds what was being done to which remote path to a failure, so the
/// message a person reads names both.
fn at<T>(result: Result<T, SshError>, what: &str, path: &str) -> Result<T, SshError> {
    result.map_err(|e| SshError::Sftp(format!("{what} {path}: {e}")))
}

/// Bytes in a local file or folder (links and special files are skipped, as
/// the upload skips them). `None` when it cannot be told.
async fn local_total(path: &Path) -> Option<u64> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut total = 0u64;
        let mut pending = vec![path];
        while let Some(path) = pending.pop() {
            let metadata = std::fs::metadata(&path).ok()?;
            if !metadata.is_dir() {
                total += metadata.len();
                continue;
            }
            for entry in std::fs::read_dir(&path).ok()? {
                let entry = entry.ok()?;
                let kind = entry.file_type().ok()?;
                if kind.is_dir() {
                    pending.push(entry.path());
                } else if kind.is_file() {
                    total += entry.metadata().ok()?.len();
                }
            }
        }
        Some(total)
    })
    .await
    .ok()
    .flatten()
}

/// Bytes in a remote file or folder, counting what a download fetches.
async fn remote_total(sftp: &Sftp, remote: &str) -> Option<u64> {
    let stat = sftp.stat(remote).await.ok()?;
    if stat.kind != EntryKind::Dir {
        return Some(stat.size);
    }
    let mut total = 0u64;
    let mut pending = vec![remote.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in sftp.list(&dir).await.ok()? {
            if !safe_name(&entry.name) {
                continue;
            }
            match entry.stat.kind {
                EntryKind::Dir => pending.push(join_remote(&dir, &entry.name)),
                EntryKind::File => total += entry.stat.size,
                _ => {}
            }
        }
    }
    Some(total)
}

fn local(error: std::io::Error, path: &Path) -> SshError {
    SshError::Sftp(format!("{}: {error}", path.display()))
}

async fn upload_file(job: &mut Job<'_>, local_path: &Path, remote: &str) -> Result<(), SshError> {
    let sftp = job.sftp;
    let mut source = tokio::fs::File::open(local_path)
        .await
        .map_err(|e| local(e, local_path))?;
    // Uploading onto a link replaces the file it points to, as writing
    // through it did. A broken link is replaced itself.
    let mut existing = sftp.lstat(remote).await.ok();
    let mut target = remote.to_owned();
    if existing
        .as_ref()
        .is_some_and(|s| s.kind == EntryKind::Symlink)
    {
        if let Ok(real) = sftp.realpath(remote).await {
            existing = sftp.lstat(&real).await.ok();
            target = real;
        }
    }
    if existing.as_ref().is_some_and(|s| s.kind == EntryKind::Dir) {
        return Err(SshError::Sftp(format!("{remote} is a folder")));
    }
    let part = remote_part(&target);
    job.writing(Some(Partial::Remote(part.clone())));
    let mut file = at(sftp.create(&part).await, "creating", &part)?;
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
        job.progress.add(read as u64);
    }
    file.shutdown()
        .await
        .map_err(|e| SshError::Sftp(format!("closing {remote}: {e}")))?;
    drop(file);
    if let Some(mode) = existing.as_ref().and_then(|s| s.permissions) {
        // The replaced file keeps its permissions, as it did when it was
        // overwritten in place.
        let _ = sftp.set_permissions(&part, mode).await;
    }
    // A plain SFTP rename does not replace an existing file on OpenSSH.
    if let Err(error) = sftp.rename(&part, &target).await {
        if existing.is_none() {
            return at(Err(error), "renaming to", &target);
        }
        at(sftp.remove_file(&target).await, "replacing", &target)?;
        at(sftp.rename(&part, &target).await, "renaming to", &target)?;
    }
    job.writing(None);
    Ok(())
}

async fn upload_path(
    job: &mut Job<'_>,
    local_path: &Path,
    remote: &str,
    recursive: bool,
) -> Result<(), SshError> {
    let sftp = job.sftp;
    let metadata = tokio::fs::metadata(local_path)
        .await
        .map_err(|e| local(e, local_path))?;
    if !metadata.is_dir() {
        return upload_file(job, local_path, remote).await;
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
            if !sftp
                .stat(&remote_dir)
                .await
                .is_ok_and(|s| s.kind == EntryKind::Dir)
            {
                return at(Err(e), "creating folder", &remote_dir);
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
                upload_file(job, &entry.path(), &target).await?;
            }
            // Symbolic links and special files are not followed.
        }
    }
    Ok(())
}

async fn download_file(job: &mut Job<'_>, remote: &str, local_path: &Path) -> Result<(), SshError> {
    let mut file = at(job.sftp.open_read(remote).await, "opening", remote)?;
    let part = local_part(local_path);
    job.writing(Some(Partial::Local(part.clone())));
    let mut sink = tokio::fs::File::create(&part)
        .await
        .map_err(|e| local(e, &part))?;
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
            .map_err(|e| local(e, &part))?;
        job.progress.add(read as u64);
    }
    sink.flush().await.map_err(|e| local(e, &part))?;
    drop(sink);
    tokio::fs::rename(&part, local_path)
        .await
        .map_err(|e| local(e, local_path))?;
    job.writing(None);
    Ok(())
}

async fn download_path(
    job: &mut Job<'_>,
    remote: &str,
    local_path: &Path,
    recursive: bool,
) -> Result<(), SshError> {
    let sftp = job.sftp;
    let stat = at(sftp.stat(remote).await, "reading", remote)?;
    if stat.kind != EntryKind::Dir {
        return download_file(job, remote, local_path).await;
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
        for entry in at(sftp.list(&remote_dir).await, "listing", &remote_dir)? {
            if !safe_name(&entry.name) {
                continue;
            }
            let source = join_remote(&remote_dir, &entry.name);
            let target = dir.join(&entry.name);
            match entry.stat.kind {
                EntryKind::Dir => pending.push((source, target)),
                EntryKind::File => download_file(job, &source, &target).await?,
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
            self.sftp.remove_dir_all(path).await
        } else {
            self.sftp.remove_file(path).await
        }
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<(), SshError> {
        self.sftp.rename(from, to).await
    }

    /// Sets the permission bits of `path`, and of everything in it when
    /// `recursive` is set.
    pub async fn chmod(&self, path: &str, mode: u32, recursive: bool) -> Result<(), SshError> {
        if recursive {
            self.sftp.set_permissions_all(path, mode).await
        } else {
            self.sftp.set_permissions(path, mode).await
        }
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
    fn unfinished_files_have_a_hidden_name_beside_the_real_one() {
        assert_eq!(remote_part("/srv/app.tar"), "/srv/.app.tar.easyssh-part");
        assert_eq!(remote_part("/app.tar"), "/.app.tar.easyssh-part");
        assert_eq!(remote_part("app.tar"), ".app.tar.easyssh-part");
        assert_eq!(
            local_part(Path::new("/tmp/x/report 1.pdf")),
            Path::new("/tmp/x/.report 1.pdf.easyssh-part")
        );
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
        assert_eq!(mode_string(EntryKind::File, Some(0o4755)), "-rwsr-xr-x");
        assert_eq!(mode_string(EntryKind::File, Some(0o4644)), "-rwSr--r--");
        assert_eq!(mode_string(EntryKind::Dir, Some(0o1777)), "drwxrwxrwt");
        assert_eq!(mode_string(EntryKind::Dir, Some(0o2750)), "drwxr-s---");
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
    #[test]
    fn progress_carries_the_total_once_known() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let mut progress = Progress::new(
            "t".into(),
            Arc::new(move |kind: &str, payload: Value| {
                lock(&seen).push((kind.to_owned(), payload));
                1
            }),
        );
        progress.announce(None);
        assert!(lock(&events).is_empty(), "nothing to say without a total");
        progress.announce(Some(500));
        progress.add(10);
        progress.flush();
        let events = lock(&events);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].1["bytes"], 0);
        assert_eq!(events[0].1["total"], 500);
        assert_eq!(events[1].1["bytes"], 10);
        assert_eq!(events[1].1["total"], 500);
    }

    #[tokio::test]
    async fn a_local_folder_is_sized_by_its_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a"), [0u8; 10]).unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub").join("b"), [0u8; 32]).unwrap();
        assert_eq!(local_total(dir.path()).await, Some(42));
        assert_eq!(local_total(&dir.path().join("a")).await, Some(10));
        assert_eq!(local_total(&dir.path().join("missing")).await, None);
    }

    #[test]
    fn failures_name_the_remote_path_and_the_reason() {
        let error = at::<()>(
            Err(SshError::Sftp("permission denied".into())),
            "creating",
            "/srv/x",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("/srv/x") && error.contains("permission denied"),
            "{error}"
        );
    }
}
