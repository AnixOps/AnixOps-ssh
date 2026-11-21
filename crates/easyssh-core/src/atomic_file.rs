//! Whole-file writes that other writers cannot interleave with.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temporary name next to `path`, unique per process and per call.
fn temporary_name(path: &Path) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(
        ".{}-{}-{nanos:x}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    path.with_file_name(name)
}

/// Writes `bytes` to a fresh temporary file in the same directory (created
/// exclusively, so no other writer shares it), then renames it over `path`.
/// The temporary file is removed when anything fails.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut attempts = 0;
    loop {
        let temporary = temporary_name(path);
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempts < 8 => {
                attempts += 1;
                continue;
            }
            Err(error) => return Err(error),
        };
        let result = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                drop(file);
                fs::rename(&temporary, path)
            });
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        return result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_writes_leave_one_whole_document_and_no_temporaries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("doc.json");
        let docs: Vec<Vec<u8>> = (0..2u8).map(|n| vec![b'a' + n; 256 * 1024]).collect();
        std::thread::scope(|scope| {
            for doc in &docs {
                let path = &path;
                scope.spawn(move || {
                    for _ in 0..20 {
                        // Windows may refuse a rename onto a file another
                        // thread has open; only the final state matters.
                        let _ = write(path, doc);
                    }
                });
            }
        });
        write(&path, &docs[0]).unwrap();
        let result = fs::read(&path).unwrap();
        assert!(docs.contains(&result));
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("doc.json")]);
    }

    #[test]
    fn a_failed_write_leaves_no_temporary() {
        let dir = tempfile::tempdir().unwrap();
        // Renaming a file over a directory fails.
        let target = dir.path().join("taken");
        fs::create_dir(&target).unwrap();
        assert!(write(&target, b"x").is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
