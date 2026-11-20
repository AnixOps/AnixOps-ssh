//! The file pane and transfers on the built-in engine: `sftp.*` and
//! `transfer.*`. An SFTP session is a channel of the host's shared
//! connection; every transfer runs as its own task so it can be cancelled.

use crate::events::Events;
use crate::local_files::safe_file_name;
use crate::Lease;
use easyssh_ssh::{DirEntry, EntryKind, Error as SshError, FileStat, Sftp};
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
            let skipped = job.progress.skipped;
            let (status, error) = match result {
                Ok(()) => ("completed", None),
                Err(e) => {
                    remove_partial(&task_entry.sftp, &task_current).await;
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
    let mut file = sftp.create(&part).await?;
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
    // A replaced file keeps its own permissions, as it did when it was
    // overwritten in place; a new one gets the local permissions, like
    // `put -p`. Both get the local modification time. A server that refuses
    // leaves an upload that is still complete.
    let metadata = source.metadata().await.ok();
    let mode = existing
        .as_ref()
        .and_then(|s| s.permissions)
        .or_else(|| metadata.as_ref().and_then(local_mode));
    let modified = metadata.and_then(|m| m.modified().ok());
    if mode.is_some() || modified.is_some() {
        let _ = sftp.set_stat(&part, mode, modified).await;
    }
    // A plain SFTP rename does not replace an existing file on OpenSSH.
    if let Err(error) = sftp.rename(&part, &target).await {
        if existing.is_none() {
            return Err(error);
        }
        sftp.remove_file(&target).await?;
        sftp.rename(&part, &target).await?;
    }
    job.writing(None);
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
                        job.progress.skipped += 1;
                        continue;
                    }
                }
            }
            if kind.is_dir() {
                match tokio::fs::canonicalize(&path).await {
                    Ok(real) if !above.contains(&real) => {
                        pending.push((path, target, [&above[..], &[real]].concat()))
                    }
                    _ => job.progress.skipped += 1,
                }
            } else if kind.is_file() {
                upload_file(job, &path, &target).await?;
            }
            // Special files (sockets, devices) are not sent.
        }
    }
    Ok(())
}

async fn download_file(job: &mut Job<'_>, remote: &str, local_path: &Path) -> Result<(), SshError> {
    let stat = job.sftp.stat(remote).await?;
    let mut file = job.sftp.open_read(remote).await?;
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
    let sink = sink.into_std().await;
    preserve_local(&sink, &stat).map_err(|e| local(e, &part))?;
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
    let stat = sftp.stat(remote).await?;
    if stat.kind != EntryKind::Dir {
        return download_file(job, remote, local_path).await;
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
                        job.progress.skipped += 1;
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
                    _ => job.progress.skipped += 1,
                },
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
