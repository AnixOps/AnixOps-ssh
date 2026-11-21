//! Editing a remote file in a local editor: where the temporary copy lives
//! and a watcher that reports when the editor saved it.
//!
//! The copy sits in `<root>/<hash of host and remote path>/<file name>`, so
//! one remote file always maps to the same folder, two files with the same
//! name on different hosts never collide, and the upload that follows a save
//! (which names the remote file after the local one) puts the file back where
//! it came from.

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime};

/// Larger files are not opened for editing.
pub const MAX_EDIT_BYTES: u64 = 20 * 1024 * 1024;

/// A remote name made safe as a local file name.
pub fn safe_file_name(name: &str) -> String {
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

/// The folder holding the copy of `remote_path` on `host_id`.
pub fn edit_dir(root: &Path, host_id: &str, remote_path: &str) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(host_id.as_bytes());
    hash.update([0]);
    hash.update(remote_path.as_bytes());
    let digest = hash.finalize();
    let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    root.join(name)
}

/// The local copy of `remote_path` on `host_id`; `None` for a path without a file name.
pub fn local_file(root: &Path, host_id: &str, remote_path: &str) -> Option<PathBuf> {
    let name = remote_path.rsplit('/').find(|part| !part.is_empty())?;
    Some(edit_dir(root, host_id, remote_path).join(safe_file_name(name)))
}

/// True when `path` is below `root` without stepping out of it.
pub fn is_inside(root: &Path, path: &Path) -> bool {
    path.starts_with(root)
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
}

/// How much of a file is read to tell text from binary, as git does.
pub const SNIFF_BYTES: usize = 8000;

/// The start of a file is binary: it has a NUL byte, which text in any
/// encoding a text editor opens by default (UTF-8, Latin-1, GBK) never has.
pub fn looks_binary(head: &[u8]) -> bool {
    head.iter().take(SNIFF_BYTES).any(|&byte| byte == 0)
}

/// Reads the start of `path` and tells whether it is binary.
pub fn file_looks_binary(path: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    let mut head = Vec::with_capacity(SNIFF_BYTES);
    std::fs::File::open(path)?
        .take(SNIFF_BYTES as u64)
        .read_to_end(&mut head)?;
    Ok(looks_binary(&head))
}

/// Splits a configured editor command into the program and its arguments.
/// Spaces separate; double quotes keep a path with spaces together.
pub fn split_command(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quoted = false;
    for ch in command.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            ch if ch.is_whitespace() && !quoted => {
                if started {
                    parts.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            ch => {
                current.push(ch);
                started = true;
            }
        }
    }
    if started {
        parts.push(current);
    }
    parts
}

/// What is told about a file to notice that it changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fingerprint {
    len: u64,
    modified: Option<SystemTime>,
}

pub fn fingerprint(path: &Path) -> Option<Fingerprint> {
    let metadata = std::fs::metadata(path).ok()?;
    metadata.is_file().then(|| Fingerprint {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

/// Reports saves of one file. Dropping it stops the watching.
pub struct EditWatch {
    _watcher: RecommendedWatcher,
}

/// Calls `on_change` once the file at `file` was saved and nothing wrote to it
/// for `debounce`. Editors save by writing in place or by replacing the file
/// through a rename, so the folder is watched; only saves that change the
/// file's size or modification time count.
pub fn watch(
    file: &Path,
    debounce: Duration,
    on_change: impl Fn() + Send + 'static,
) -> notify::Result<EditWatch> {
    let dir = file
        .parent()
        .ok_or_else(|| notify::Error::generic("the file to watch has no folder"))?;
    let name = file.file_name().map(|name| name.to_owned());
    let (sender, receiver) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            if event
                .paths
                .iter()
                .any(|path| path.file_name().map(|n| n.to_owned()) == name)
            {
                let _ = sender.send(());
            }
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    let file = file.to_owned();
    let mut last = fingerprint(&file);
    std::thread::spawn(move || {
        // Ends when the watcher, and with it the sender, is dropped.
        while receiver.recv().is_ok() {
            loop {
                match receiver.recv_timeout(debounce) {
                    Ok(()) => continue,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            let now = fingerprint(&file);
            if now.is_some() && now != last {
                last = now;
                on_change();
            }
        }
    });
    Ok(EditWatch { _watcher: watcher })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Instant;

    #[test]
    fn binary_is_told_by_a_nul_byte_near_the_start() {
        assert!(looks_binary(b"\x7fELF\x02\x01\x01\0\0"));
        assert!(!looks_binary("server { listen 80; } # 中文注释".as_bytes()));
        assert!(!looks_binary(b""));
        let mut late = vec![b'a'; SNIFF_BYTES];
        late.push(0);
        assert!(!looks_binary(&late));
        let dir = std::env::temp_dir().join(format!("easyssh-sniff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("ls");
        std::fs::write(&file, b"\x7fELF\0\0\0").unwrap();
        assert!(file_looks_binary(&file).unwrap());
        std::fs::write(&file, b"#!/bin/sh\necho hi\n").unwrap();
        assert!(!file_looks_binary(&file).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_remote_file_has_one_folder_and_hosts_do_not_collide() {
        let root = Path::new("/cache/edit");
        let a = local_file(root, "h1", "/etc/nginx/nginx.conf").unwrap();
        assert_eq!(a, local_file(root, "h1", "/etc/nginx/nginx.conf").unwrap());
        assert_eq!(a.file_name().unwrap(), "nginx.conf");
        assert_ne!(a, local_file(root, "h2", "/etc/nginx/nginx.conf").unwrap());
        assert_ne!(a, local_file(root, "h1", "/opt/nginx.conf").unwrap());
        assert!(is_inside(root, &a));
    }

    #[test]
    fn names_from_the_server_stay_inside_the_folder() {
        let root = Path::new("/cache/edit");
        let file = local_file(root, "h", "/tmp/..\\..\\evil").unwrap();
        assert!(is_inside(root, &file));
        assert_eq!(safe_file_name(".."), "download");
        assert_eq!(local_file(root, "h", "/"), None);
        assert!(!is_inside(root, &root.join("a/../../b")));
        assert!(!is_inside(root, Path::new("/elsewhere/file")));
    }

    #[test]
    fn an_editor_command_splits_into_program_and_arguments() {
        assert_eq!(split_command("code -w"), ["code", "-w"]);
        assert_eq!(
            split_command(r#""C:\Program Files\Notepad++\notepad++.exe" -multiInst"#),
            [r"C:\Program Files\Notepad++\notepad++.exe", "-multiInst"]
        );
        assert!(split_command("   ").is_empty());
    }

    fn wait_for(count: &AtomicUsize, at_least: usize) -> bool {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if count.load(Ordering::SeqCst) >= at_least {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    #[test]
    fn a_save_is_reported_once_and_a_burst_counts_as_one() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.conf");
        std::fs::write(&file, "one").unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        // The quiet time is long enough that a busy CI runner pausing
        // between the two writes of the burst still counts them as one.
        let _watch = watch(&file, Duration::from_millis(1000), move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        // Another file in the folder is not this file.
        std::fs::write(dir.path().join("other"), "x").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(count.load(Ordering::SeqCst), 0);
        // A burst of writes, then a save that replaces the file by rename.
        std::fs::write(&file, "two!").unwrap();
        std::fs::write(&file, "three").unwrap();
        assert!(wait_for(&count, 1));
        std::thread::sleep(Duration::from_millis(1500));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let swap = dir.path().join(".a.conf.swp");
        std::fs::write(&swap, "four, longer").unwrap();
        std::fs::rename(&swap, &file).unwrap();
        assert!(wait_for(&count, 2));
    }

    #[test]
    fn dropping_the_watch_stops_the_reports() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("b.txt");
        std::fs::write(&file, "a").unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let watch = watch(&file, Duration::from_millis(100), move || {
            seen.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        drop(watch);
        std::fs::write(&file, "bbbb").unwrap();
        std::thread::sleep(Duration::from_millis(600));
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }
}
