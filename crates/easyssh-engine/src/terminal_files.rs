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

use crate::local_files::reserve_unique_path;

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
        /// An empty file this receive created at `target` to hold the name;
        /// removed again when the receive is dropped.
        reserved: bool,
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

    /// Starts receiving a file that will end up at `target`, replacing
    /// whatever is there when it completes (the person chose that path).
    /// Returns the id and the final path.
    pub fn open_sink(&self, target: &Path) -> Result<(u64, PathBuf), String> {
        self.open_sink_at(target, false)
    }

    /// Starts receiving a file called `name` in `dir`. The final name is
    /// reserved right away (`name`, or `name (1)`, ... when taken), so two
    /// receives of the same name end up in two files and neither replaces the
    /// other. Returns the id and the final path.
    pub fn open_sink_unique(&self, dir: &Path, name: &str) -> Result<(u64, PathBuf), String> {
        let target = reserve_unique_path(dir, name)?;
        self.open_sink_at(&target, true).inspect_err(|_| {
            let _ = std::fs::remove_file(&target);
        })
    }

    fn open_sink_at(&self, target: &Path, reserved: bool) -> Result<(u64, PathBuf), String> {
        let name = target
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("no file name")?;
        // Every receive creates its own temporary file, exclusively: the
        // suffix is random and `create_new` fails if it is somehow taken.
        let mut attempts = 0;
        let (file, part) = loop {
            let suffix = uuid::Uuid::new_v4().simple().to_string();
            let part = target.with_file_name(format!(".{name}.{}.part", &suffix[..12]));
            match OpenOptions::new().write(true).create_new(true).open(&part) {
                Ok(file) => break (file, part),
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists && attempts < 16 =>
                {
                    attempts += 1;
                }
                Err(error) => return Err(error.to_string()),
            }
        };
        let id = self.insert(Open::Sink {
            file,
            part,
            target: target.to_owned(),
            reserved,
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
            reserved,
        } = open
        {
            let result = if keep {
                file.flush()
                    .map_err(|error| error.to_string())
                    .and_then(|()| {
                        drop(file);
                        // Replaces the empty reservation, or the file the
                        // person chose to overwrite. The temporary file is
                        // this receive's own, so nothing else is touched.
                        std::fs::rename(&part, &target).map_err(|error| error.to_string())
                    })
            } else {
                drop(file);
                Ok(())
            };
            if !keep || result.is_err() {
                let _ = std::fs::remove_file(&part);
                if reserved {
                    let _ = std::fs::remove_file(&target);
                }
            }
            return result;
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

    fn leftovers(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// Two receives of one name, A and B, each carrying different bytes and
    /// writing in turns; `end_a`/`end_b` say how each one finishes
    /// (true = keep). Returns the folder.
    fn two_receives(first_ends: &[(char, bool)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let files = TerminalFiles::default();
        let (a, path_a) = files.open_sink_unique(dir.path(), "same.txt").unwrap();
        let (b, path_b) = files.open_sink_unique(dir.path(), "same.txt").unwrap();
        assert_ne!(path_a, path_b, "each receive gets its own final name");
        files.write(a, b"AAAA-").unwrap();
        files.write(b, b"BBBB-").unwrap();
        files.write(a, b"aaaa").unwrap();
        files.write(b, b"bbbb").unwrap();
        for (which, keep) in first_ends {
            files
                .close(if *which == 'a' { a } else { b }, *keep)
                .unwrap();
        }
        let kept_a = first_ends.iter().any(|(w, k)| *w == 'a' && *k);
        let kept_b = first_ends.iter().any(|(w, k)| *w == 'b' && *k);
        assert_eq!(path_a.exists(), kept_a);
        assert_eq!(path_b.exists(), kept_b);
        if kept_a {
            assert_eq!(std::fs::read(&path_a).unwrap(), b"AAAA-aaaa");
        }
        if kept_b {
            assert_eq!(std::fs::read(&path_b).unwrap(), b"BBBB-bbbb");
        }
        dir
    }

    #[test]
    fn two_receives_of_one_name_never_share_files_whatever_the_order() {
        // A finishes first, then B.
        let dir = two_receives(&[('a', true), ('b', true)]);
        assert_eq!(leftovers(dir.path()), ["same (1).txt", "same.txt"]);
        // B finishes first, then A.
        let dir = two_receives(&[('b', true), ('a', true)]);
        assert_eq!(leftovers(dir.path()), ["same (1).txt", "same.txt"]);
        // A is cancelled while B completes, either way round.
        let dir = two_receives(&[('a', false), ('b', true)]);
        assert_eq!(leftovers(dir.path()), ["same (1).txt"]);
        let dir = two_receives(&[('b', true), ('a', false)]);
        assert_eq!(leftovers(dir.path()), ["same (1).txt"]);
        let dir = two_receives(&[('b', false), ('a', true)]);
        assert_eq!(leftovers(dir.path()), ["same.txt"]);
        // Both cancelled: nothing at all, no .part either.
        let dir = two_receives(&[('a', false), ('b', false)]);
        assert!(leftovers(dir.path()).is_empty());
        let dir = two_receives(&[('b', false), ('a', false)]);
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn a_receive_never_replaces_a_file_that_was_already_there() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("same.txt"), b"mine").unwrap();
        let files = TerminalFiles::default();
        let (id, path) = files.open_sink_unique(dir.path(), "same.txt").unwrap();
        assert_eq!(path, dir.path().join("same (1).txt"));
        files.write(id, b"new").unwrap();
        files.close(id, true).unwrap();
        assert_eq!(std::fs::read(dir.path().join("same.txt")).unwrap(), b"mine");
        assert_eq!(std::fs::read(path).unwrap(), b"new");
    }

    #[test]
    fn receives_on_threads_each_keep_their_own_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let files = TerminalFiles::default();
        let paths: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8u8)
                .map(|n| {
                    let files = &files;
                    let dir = dir.path();
                    scope.spawn(move || {
                        let (id, path) = files.open_sink_unique(dir, "race.bin").unwrap();
                        for _ in 0..50 {
                            files.write(id, &[n; 100]).unwrap();
                        }
                        files.close(id, true).unwrap();
                        (n, path)
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for (n, path) in paths {
            assert_eq!(std::fs::read(path).unwrap(), vec![n; 5000]);
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 8);
    }

    #[test]
    fn two_receives_to_one_chosen_path_do_not_mix_and_cancel_keeps_the_other() {
        // The person picked the path, so the last to finish replaces it, but
        // each file is whole and a cancel only removes its own temporary file.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("chosen.txt");
        std::fs::write(&target, b"old").unwrap();
        let files = TerminalFiles::default();
        let (a, _) = files.open_sink(&target).unwrap();
        let (b, _) = files.open_sink(&target).unwrap();
        files.write(a, b"AAAA").unwrap();
        files.write(b, b"BB").unwrap();
        files.close(a, false).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"old");
        files.write(b, b"BB").unwrap();
        files.close(b, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"BBBB");
        assert_eq!(leftovers(dir.path()), ["chosen.txt"]);
    }

    #[test]
    fn a_directory_cannot_be_sent() {
        let dir = tempfile::tempdir().unwrap();
        assert!(TerminalFiles::default().open_source(dir.path()).is_err());
    }
}
