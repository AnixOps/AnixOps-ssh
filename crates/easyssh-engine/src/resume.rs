//! What a stopped transfer leaves behind and what it takes to continue it.
//!
//! A transfer writes each file under a temporary name next to its target
//! (`.name.easyssh-part`, on the server for an upload and on this machine for
//! a download). When it fails or is cancelled the temporary file stays, and a
//! small record in the journal says what it was a part of: the source's size
//! and modification time and the temporary file's path. A transfer asked to
//! resume continues from the temporary file's length only when the source
//! still has the size and time recorded; otherwise it starts again from zero.
//!
//! The journal is a folder with one JSON file per partial file, named after a
//! hash of what it describes. Records that have not been touched for
//! [`STALE_AFTER`] are dropped (with the local temporary file) when the
//! journal is opened; a temporary file on a server cannot be reached then and
//! stays until it is discarded or written again.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Partial files nobody continued in this long are removed.
pub const STALE_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// An upload writes many requests ahead; if the connection breaks the server
/// may have the last of them but not an earlier one. A resumed upload starts
/// this far before the end of what is there and writes it again.
pub const REWIND: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub host: String,
    pub upload: bool,
    /// The local file this is a part of (the source of an upload, the target
    /// of a download).
    pub local: String,
    /// The remote file.
    pub remote: String,
    /// The temporary file: remote for an upload, local for a download.
    pub part: String,
    /// Size and modification time (seconds) of the source when it began.
    pub size: u64,
    pub mtime: Option<u64>,
    /// Bytes of the file moved when the record was last written.
    pub bytes: u64,
    /// The file or folder the whole transfer was asked for.
    pub top_local: String,
    pub top_remote: String,
    /// Seconds since the epoch of the last write.
    pub updated: u64,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|age| age.as_secs())
        .unwrap_or(0)
}

/// Whole seconds since the epoch, the precision SFTP keeps.
pub fn secs(time: Option<SystemTime>) -> Option<u64> {
    time.and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|age| age.as_secs())
}

impl Entry {
    /// Is the source still the one this partial file was made from?
    pub fn same_source(&self, size: u64, mtime: Option<u64>) -> bool {
        self.size == size && self.mtime.is_some() && self.mtime == mtime
    }
}

/// Where a resumed transfer continues, given the length of the temporary
/// file; `None` when it cannot be continued (it is longer than the source).
pub fn resume_offset(upload: bool, part_len: u64, size: u64) -> Option<u64> {
    if part_len > size {
        return None;
    }
    Some(if upload {
        part_len.saturating_sub(REWIND)
    } else {
        // Downloads are written in file order, so what is there is a prefix.
        part_len
    })
}

pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    /// Opens (creating) the journal folder and drops what went stale.
    pub fn open(dir: PathBuf) -> Journal {
        let _ = std::fs::create_dir_all(&dir);
        let journal = Journal { dir };
        journal.sweep(now_secs());
        journal
    }

    fn file(&self, host: &str, upload: bool, local: &str, remote: &str) -> PathBuf {
        let mut hash = Sha256::new();
        for part in [host, if upload { "up" } else { "down" }, local, remote] {
            hash.update(part.as_bytes());
            hash.update([0]);
        }
        let name: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{name}.json"))
    }

    pub fn load(&self, host: &str, upload: bool, local: &str, remote: &str) -> Option<Entry> {
        let text = std::fs::read(self.file(host, upload, local, remote)).ok()?;
        serde_json::from_slice(&text).ok()
    }

    /// Writes the record (best effort: a transfer never fails for it).
    pub fn save(&self, entry: &Entry) {
        let path = self.file(&entry.host, entry.upload, &entry.local, &entry.remote);
        if let Ok(text) = serde_json::to_vec(entry) {
            let _ = std::fs::write(path, text);
        }
    }

    pub fn remove(&self, host: &str, upload: bool, local: &str, remote: &str) {
        let _ = std::fs::remove_file(self.file(host, upload, local, remote));
    }

    pub fn all(&self) -> Vec<Entry> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| std::fs::read(entry.path()).ok())
            .filter_map(|text| serde_json::from_slice(&text).ok())
            .collect()
    }

    /// The partial files of one requested transfer: the records of `host`
    /// whose whole transfer was `remote` (and, when given, `local`).
    pub fn find(&self, host: &str, upload: bool, local: Option<&str>, remote: &str) -> Vec<Entry> {
        let mut found: Vec<Entry> = self
            .all()
            .into_iter()
            .filter(|e| {
                e.host == host
                    && e.upload == upload
                    && e.top_remote == remote
                    && local.is_none_or(|local| e.top_local == local)
            })
            .collect();
        found.sort_by(|a, b| a.remote.cmp(&b.remote));
        found
    }

    /// Forgets a record; a local temporary file goes with it.
    pub fn discard_local(&self, entry: &Entry) {
        self.remove(&entry.host, entry.upload, &entry.local, &entry.remote);
        if !entry.upload {
            let _ = std::fs::remove_file(&entry.part);
        }
    }

    /// Drops records untouched for [`STALE_AFTER`]. Returns how many.
    pub fn sweep(&self, now: u64) -> usize {
        let mut dropped = 0;
        for entry in self.all() {
            if now.saturating_sub(entry.updated) >= STALE_AFTER.as_secs() {
                self.discard_local(&entry);
                dropped += 1;
            }
        }
        dropped
    }
}

/// SHA-256 of a local file, lower-case hex.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(updated: u64, part: &Path) -> Entry {
        Entry {
            host: "h".into(),
            upload: false,
            local: "/l/a".into(),
            remote: "/r/a".into(),
            part: part.to_string_lossy().into_owned(),
            size: 10,
            mtime: Some(5),
            bytes: 3,
            top_local: "/l/a".into(),
            top_remote: "/r/a".into(),
            updated,
        }
    }

    #[test]
    fn a_changed_source_cannot_be_continued() {
        let e = entry(0, Path::new("/x"));
        assert!(e.same_source(10, Some(5)));
        assert!(!e.same_source(11, Some(5)));
        assert!(!e.same_source(10, Some(6)));
        assert!(!e.same_source(10, None));
    }

    #[test]
    fn uploads_rewind_and_downloads_do_not() {
        assert_eq!(resume_offset(true, 100, 1000), Some(0));
        assert_eq!(resume_offset(true, REWIND + 7, REWIND * 2), Some(7));
        assert_eq!(resume_offset(false, 700, 1000), Some(700));
        assert_eq!(resume_offset(false, 1001, 1000), None);
        assert_eq!(resume_offset(false, 0, 0), Some(0));
    }

    #[test]
    fn records_are_found_by_what_was_asked_and_stale_ones_go_with_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join(".a.easyssh-part");
        std::fs::write(&part, b"abc").unwrap();
        let journal = Journal::open(dir.path().join("journal"));
        let e = entry(1000, &part);
        journal.save(&e);
        assert_eq!(journal.load("h", false, "/l/a", "/r/a"), Some(e.clone()));
        assert_eq!(journal.find("h", false, None, "/r/a"), vec![e.clone()]);
        assert!(journal.find("h", true, None, "/r/a").is_empty());
        assert!(journal.find("other", false, None, "/r/a").is_empty());
        assert_eq!(journal.sweep(1000 + STALE_AFTER.as_secs() - 1), 0);
        assert!(part.exists());
        assert_eq!(journal.sweep(1000 + STALE_AFTER.as_secs()), 1);
        assert!(!part.exists());
        assert!(journal.all().is_empty());
    }

    #[test]
    fn hashes_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
