//! Local files for `rz`/`sz` in the terminal.
//!
//! The web view runs the ZMODEM protocol, but cannot touch the disk: it asks
//! for a file to be opened, then reads (sending) or writes (receiving) it in
//! chunks by an id. A received file is written under a temporary name next to
//! its final place and renamed when the transfer completes, so an aborted
//! transfer never leaves a half file under the real name.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::UNIX_EPOCH;

/// What the sender needs to offer a file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceInfo {
    pub id: u64,
    pub name: String,
    pub size: u64,
    /// Seconds since the epoch; 0 when the file system does not say.
    pub modified: u64,
}

enum Open {
    Source(File),
    Sink {
        file: File,
        part: PathBuf,
        target: PathBuf,
    },
}

#[derive(Default)]
pub struct TerminalFiles {
    next: AtomicU64,
    open: Mutex<HashMap<u64, Open>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl TerminalFiles {
    fn insert(&self, open: Open) -> u64 {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        lock(&self.open).insert(id, open);
        id
    }

    /// Opens a regular file for sending.
    pub fn open_source(&self, path: &Path) -> Result<SourceInfo, String> {
        let file = File::open(path).map_err(|error| error.to_string())?;
        let meta = file.metadata().map_err(|error| error.to_string())?;
        if !meta.is_file() {
            return Err("not a regular file".to_owned());
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file")
            .to_owned();
        let modified = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |elapsed| elapsed.as_secs());
        let id = self.insert(Open::Source(file));
        Ok(SourceInfo {
            id,
            name,
            size: meta.len(),
            modified,
        })
    }

    /// The next up to `max` bytes; empty at the end of the file.
    pub fn read(&self, id: u64, max: usize) -> Result<Vec<u8>, String> {
        let mut open = lock(&self.open);
        let Some(Open::Source(file)) = open.get_mut(&id) else {
            return Err("unknown file".to_owned());
        };
        let mut buffer = vec![0u8; max];
        let mut filled = 0;
        while filled < max {
            match file.read(&mut buffer[filled..]) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        buffer.truncate(filled);
        Ok(buffer)
    }

    /// Starts receiving a file that will end up at `target`. Returns the id
    /// and the final path.
    pub fn open_sink(&self, target: &Path) -> Result<(u64, PathBuf), String> {
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("no file name")?;
        let part = target.with_file_name(format!(".{name}.part"));
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&part)
            .map_err(|error| error.to_string())?;
        let id = self.insert(Open::Sink {
            file,
            part,
            target: target.to_owned(),
        });
        Ok((id, target.to_owned()))
    }

    pub fn write(&self, id: u64, bytes: &[u8]) -> Result<(), String> {
        let mut open = lock(&self.open);
        let Some(Open::Sink { file, .. }) = open.get_mut(&id) else {
            return Err("unknown file".to_owned());
        };
        file.write_all(bytes).map_err(|error| error.to_string())
    }

    /// Ends a file. A sink is kept (renamed to its final name) when `keep`,
    /// removed otherwise; a source is just closed.
    pub fn close(&self, id: u64, keep: bool) -> Result<(), String> {
        let Some(open) = lock(&self.open).remove(&id) else {
            return Ok(());
        };
        if let Open::Sink {
            mut file,
            part,
            target,
        } = open
        {
            if keep {
                file.flush().map_err(|error| error.to_string())?;
                drop(file);
                std::fs::rename(&part, &target).map_err(|error| error.to_string())?;
            } else {
                drop(file);
                let _ = std::fs::remove_file(&part);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::TerminalFiles;

    #[test]
    fn a_file_is_read_in_chunks_and_a_received_one_is_renamed_when_done() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("in.bin");
        let data: Vec<u8> = (0..=255u8).cycle().take(10_000).collect();
        std::fs::write(&source, &data).unwrap();

        let files = TerminalFiles::default();
        let info = files.open_source(&source).unwrap();
        assert_eq!((info.name.as_str(), info.size), ("in.bin", 10_000));
        let (sink, target) = files.open_sink(&dir.path().join("out.bin")).unwrap();
        let mut total = 0;
        loop {
            let chunk = files.read(info.id, 4096).unwrap();
            if chunk.is_empty() {
                break;
            }
            total += chunk.len();
            files.write(sink, &chunk).unwrap();
        }
        assert_eq!(total, 10_000);
        assert!(!target.exists(), "not under the real name until it is done");
        files.close(sink, true).unwrap();
        files.close(info.id, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), data);
        assert!(files.read(info.id, 1).is_err());
    }

    #[test]
    fn an_aborted_receive_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let files = TerminalFiles::default();
        let (sink, target) = files.open_sink(&dir.path().join("x.txt")).unwrap();
        files.write(sink, b"half").unwrap();
        files.close(sink, false).unwrap();
        assert!(!target.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_directory_cannot_be_sent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TerminalFiles::default().open_source(dir.path()).is_err());
    }
}
