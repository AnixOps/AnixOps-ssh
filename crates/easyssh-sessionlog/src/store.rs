//! The log directory: `<data dir>/logs/<host>/<time>-<id>.esl`.
//!
//! The host folder and the file name are the only plain-text metadata; a
//! log's content is only in the encrypted file.

use crate::format::{Header, LogError, HEADER_LEN};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const EXTENSION: &str = "esl";
const MAX_HOST_DIR: usize = 64;

/// One log in a listing. `id` is `<host dir>/<file name>`, relative to the
/// log directory, and is what the other calls take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub id: String,
    pub host: String,
    pub started_ms: u64,
    /// File modification time: when the log was last written. For a finished
    /// log that is its end, so `modified_ms - started_ms` is the duration.
    pub modified_ms: u64,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CleanupReport {
    pub deleted: usize,
    pub kept: usize,
}

/// A log directory.
#[derive(Debug, Clone)]
pub struct LogStore {
    root: PathBuf,
}

/// A host name as a folder name: letters, digits and `-_.` stay, everything
/// else becomes `_`; never empty, never starting with a dot.
pub fn host_dir_name(host: &str) -> String {
    let mut name: String = host
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .take(MAX_HOST_DIR)
        .collect();
    if name.starts_with('.') {
        name.replace_range(..1, "_");
    }
    if name.is_empty() {
        name.push_str("local");
    }
    name
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl LogStore {
    /// The store for `<data dir>`: logs are in its `logs` folder.
    pub fn in_data_dir(data_dir: &Path) -> LogStore {
        LogStore {
            root: data_dir.join("logs"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Creates the file for a new log of `host`, readable by its owner only.
    /// Returns its path.
    pub fn create(&self, host: &str, started_ms: u64) -> Result<(PathBuf, File), LogError> {
        let dir = self.root.join(host_dir_name(host));
        create_private_dir(&self.root)?;
        create_private_dir(&dir)?;
        let stamp = chrono::DateTime::from_timestamp_millis(started_ms as i64)
            .unwrap_or_default()
            .format("%Y%m%dT%H%M%SZ");
        for attempt in 0u32..1000 {
            let name = if attempt == 0 {
                format!("{stamp}.{EXTENSION}")
            } else {
                format!("{stamp}-{attempt}.{EXTENSION}")
            };
            let path = dir.join(name);
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => return Ok((path, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        Err(LogError::Io("could not find a free log file name".into()))
    }

    /// A log's path from its id, refusing anything that is not exactly
    /// `<host dir>/<name>.esl` inside the log directory.
    pub fn resolve(&self, id: &str) -> Result<PathBuf, LogError> {
        let relative = Path::new(id);
        let parts: Vec<_> = relative.components().collect();
        let ok = parts.len() == 2
            && parts.iter().all(|c| matches!(c, Component::Normal(_)))
            && relative.extension().is_some_and(|e| e == EXTENSION)
            && !id.contains('\\');
        if !ok {
            return Err(LogError::Io(format!("not a session log id: {id}")));
        }
        Ok(self.root.join(relative))
    }

    /// Every log, newest first. Files that are not logs are skipped.
    pub fn list(&self) -> Vec<LogEntry> {
        let mut entries = Vec::new();
        let Ok(hosts) = fs::read_dir(&self.root) else {
            return entries;
        };
        for host in hosts.flatten() {
            let Ok(kind) = host.file_type() else { continue };
            if !kind.is_dir() {
                continue;
            }
            let host_name = host.file_name().to_string_lossy().into_owned();
            let Ok(files) = fs::read_dir(host.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_none_or(|e| e != EXTENSION) {
                    continue;
                }
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                let Some(header) = read_header(&path) else {
                    continue;
                };
                let modified_ms = meta.modified().map_or(0, millis);
                entries.push(LogEntry {
                    id: format!("{host_name}/{}", file.file_name().to_string_lossy()),
                    host: host_name.clone(),
                    started_ms: header.started_ms,
                    modified_ms: modified_ms.max(header.started_ms),
                    size: meta.len(),
                });
            }
        }
        entries.sort_by(|a, b| b.started_ms.cmp(&a.started_ms).then(a.id.cmp(&b.id)));
        entries
    }

    /// Reads a whole log file (at most `max` bytes).
    pub fn read(&self, id: &str, max: u64) -> Result<Vec<u8>, LogError> {
        let path = self.resolve(id)?;
        let file = File::open(&path)?;
        if file.metadata()?.len() > max {
            return Err(LogError::Io(format!(
                "the session log is larger than {} MiB",
                max / (1024 * 1024)
            )));
        }
        let mut bytes = Vec::new();
        file.take(max).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn delete(&self, id: &str) -> Result<(), LogError> {
        let path = self.resolve(id)?;
        fs::remove_file(&path)?;
        if let Some(dir) = path.parent() {
            // Only succeeds when the host folder is empty.
            let _ = fs::remove_dir(dir);
        }
        Ok(())
    }

    /// Deletes logs not written to for `retention_days` days as of `now`
    /// (0 keeps everything) and removes host folders left empty.
    pub fn cleanup(&self, retention_days: u32, now: SystemTime) -> CleanupReport {
        let mut report = CleanupReport::default();
        let Ok(hosts) = fs::read_dir(&self.root) else {
            return report;
        };
        let limit = Duration::from_secs(u64::from(retention_days) * 24 * 3600);
        for host in hosts.flatten() {
            if !host.file_type().is_ok_and(|k| k.is_dir()) {
                continue;
            }
            let Ok(files) = fs::read_dir(host.path()) else {
                continue;
            };
            for file in files.flatten() {
                let path = file.path();
                if path.extension().is_none_or(|e| e != EXTENSION) {
                    continue;
                }
                let age = file
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|m| now.duration_since(m).ok())
                    .unwrap_or_default();
                if retention_days > 0 && age > limit && fs::remove_file(&path).is_ok() {
                    report.deleted += 1;
                } else {
                    report.kept += 1;
                }
            }
            let _ = fs::remove_dir(host.path());
        }
        report
    }
}

fn read_header(path: &Path) -> Option<Header> {
    let mut buf = [0u8; HEADER_LEN];
    File::open(path).ok()?.read_exact(&mut buf).ok()?;
    Header::parse(&buf).ok()
}

fn create_private_dir(path: &Path) -> Result<(), LogError> {
    if path.is_dir() {
        return Ok(());
    }
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{LogKey, LogWriter};
    use zeroize::Zeroizing;

    fn key(_: &[u8]) -> Result<LogKey, String> {
        Ok(Zeroizing::new([3; 32]))
    }

    fn make(store: &LogStore, host: &str, at: u64) -> PathBuf {
        let (path, file) = store.create(host, at).unwrap();
        let mut w = LogWriter::new(file, at, &key).unwrap();
        w.write(b"x").unwrap();
        w.finish(at + 1).unwrap();
        path
    }

    #[test]
    fn host_names_are_safe_folder_names() {
        assert_eq!(host_dir_name("prod-db 01"), "prod-db_01");
        assert_eq!(host_dir_name("../etc"), "_._etc");
        assert_eq!(host_dir_name(".hidden"), "_hidden");
        assert_eq!(host_dir_name(""), "local");
        assert_eq!(host_dir_name("a/b\\c"), "a_b_c");
        assert_eq!(host_dir_name(&"x".repeat(200)).len(), MAX_HOST_DIR);
    }

    #[test]
    fn create_list_read_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::in_data_dir(dir.path());
        assert!(store.list().is_empty());
        let a = make(&store, "web 1", 1_700_000_000_000);
        let b = make(&store, "web 1", 1_700_000_100_000);
        make(&store, "db", 1_700_000_050_000);
        assert!(a.starts_with(dir.path().join("logs").join("web_1")));
        assert_ne!(a, b);
        let list = store.list();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].started_ms, 1_700_000_100_000);
        assert_eq!(list[1].host, "db");
        assert!(list.iter().all(|e| e.size > HEADER_LEN as u64));
        let id = list[1].id.clone();
        assert!(store.read(&id, 1 << 20).unwrap().starts_with(b"ESL1"));
        assert!(store.read(&id, 10).is_err());
        store.delete(&id).unwrap();
        assert_eq!(store.list().len(), 2);
        assert!(!dir.path().join("logs/db").exists(), "empty folder removed");
    }

    #[test]
    fn same_second_logs_do_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::in_data_dir(dir.path());
        let a = make(&store, "h", 1_700_000_000_000);
        let b = make(&store, "h", 1_700_000_000_500);
        assert_ne!(a, b);
    }

    #[test]
    fn ids_cannot_leave_the_log_directory() {
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::in_data_dir(dir.path());
        for bad in [
            "../x.esl",
            "a/../../x.esl",
            "/etc/passwd.esl",
            "a/b/c.esl",
            "a.esl",
            "a/b.txt",
            "a\\b.esl",
            "",
            "./b.esl",
        ] {
            assert!(store.resolve(bad).is_err(), "{bad:?}");
        }
        assert!(store.resolve("host/20260101T000000Z.esl").is_ok());
    }

    #[test]
    fn retention_deletes_only_old_logs() {
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::in_data_dir(dir.path());
        let old = make(&store, "old", 1_000);
        let fresh = make(&store, "fresh", 2_000);
        let now = SystemTime::now();
        let aged = now - Duration::from_secs(40 * 24 * 3600);
        File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(aged)
            .unwrap();
        let report = store.cleanup(30, now);
        assert_eq!(
            report,
            CleanupReport {
                deleted: 1,
                kept: 1
            }
        );
        assert!(!old.exists());
        assert!(fresh.exists());
        assert!(!dir.path().join("logs/old").exists());
        // 0 keeps everything, however old.
        File::options()
            .write(true)
            .open(&fresh)
            .unwrap()
            .set_modified(aged)
            .unwrap();
        assert_eq!(store.cleanup(0, now).deleted, 0);
        assert!(fresh.exists());
        assert_eq!(store.cleanup(30, now).deleted, 1);
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = LogStore::in_data_dir(dir.path());
        let path = make(&store, "h", 5);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let dir_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700);
    }
}
