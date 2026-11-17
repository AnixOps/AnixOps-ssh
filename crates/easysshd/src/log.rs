//! The daemon's log file: `easysshd.log`, rotated at 1 MiB, three files kept
//! (`easysshd.log`, `easysshd.log.1`, `easysshd.log.2`).
//!
//! The log is for support: lifecycle events, errors, the kind of each command
//! and the ids of the sessions it touched. It never holds secrets, askpass
//! prompts or answers, terminal input or output, host names or payloads;
//! callers pass only identifiers and fixed words. Every message is flattened
//! to one line, so nothing a client sends can forge a log entry.

use crate::platform;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

pub const LOG_NAME: &str = "easysshd.log";
/// Size at which the current file is rotated.
pub const MAX_BYTES: u64 = 1024 * 1024;
/// Files kept, the current one included.
pub const KEEP: usize = 3;

struct Current {
    file: File,
    size: u64,
}

pub struct Logger {
    path: Option<PathBuf>,
    max_bytes: u64,
    keep: usize,
    current: Mutex<Option<Current>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Logger {
    /// A logger that discards everything.
    pub fn disabled() -> Logger {
        Logger {
            path: None,
            max_bytes: MAX_BYTES,
            keep: KEEP,
            current: Mutex::new(None),
        }
    }

    /// Log into `dir` with the standard limits. A directory that cannot be
    /// written leaves logging off; the daemon works without it.
    pub fn open(dir: &Path) -> Logger {
        Self::with_limits(dir, MAX_BYTES, KEEP)
    }

    pub fn with_limits(dir: &Path, max_bytes: u64, keep: usize) -> Logger {
        let logger = Logger {
            path: Some(dir.join(LOG_NAME)),
            max_bytes: max_bytes.max(1),
            keep: keep.max(1),
            current: Mutex::new(None),
        };
        let _ = std::fs::create_dir_all(dir);
        *lock(&logger.current) = logger.open_file();
        logger
    }

    fn open_file(&self) -> Option<Current> {
        let path = self.path.as_ref()?;
        let mut options = platform::private_open_options();
        options.append(true).create(true);
        let file = options.open(path).ok()?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Some(Current { file, size })
    }

    fn numbered(&self, index: usize) -> Option<PathBuf> {
        let path = self.path.as_ref()?;
        Some(if index == 0 {
            path.clone()
        } else {
            let mut name = path.clone().into_os_string();
            name.push(format!(".{index}"));
            PathBuf::from(name)
        })
    }

    fn rotate(&self, current: &mut Option<Current>) {
        *current = None; // close before renaming (Windows)
        let _ = self.numbered(self.keep - 1).map(std::fs::remove_file);
        for index in (0..self.keep - 1).rev() {
            if let (Some(from), Some(to)) = (self.numbered(index), self.numbered(index + 1)) {
                let _ = std::fs::rename(from, to);
            }
        }
        *current = self.open_file();
    }

    pub fn info(&self, message: &str) {
        self.write("INFO", message);
    }

    pub fn error(&self, message: &str) {
        self.write("ERROR", message);
    }

    fn write(&self, level: &str, message: &str) {
        if self.path.is_none() {
            return;
        }
        let flat: String = message
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect();
        let line = format!(
            "{} {level} {flat}\n",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        );
        let mut current = lock(&self.current);
        if current.is_none() {
            *current = self.open_file();
        }
        let too_big = current
            .as_ref()
            .is_some_and(|c| c.size > 0 && c.size + line.len() as u64 > self.max_bytes);
        if too_big {
            self.rotate(&mut current);
        }
        if let Some(c) = current.as_mut() {
            if c.file.write_all(line.as_bytes()).is_ok() {
                c.size += line.len() as u64;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_keeps_three_files() {
        let dir = tempfile::tempdir().unwrap();
        let log = Logger::with_limits(dir.path(), 200, 3);
        for i in 0..40 {
            log.info(&format!("line number {i:04} padding padding"));
        }
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert!(names.contains(&"easysshd.log".to_owned()), "{names:?}");
        assert!(names.contains(&"easysshd.log.1".to_owned()), "{names:?}");
        assert!(names.contains(&"easysshd.log.2".to_owned()), "{names:?}");
        assert!(!names.contains(&"easysshd.log.3".to_owned()), "{names:?}");
        for name in names {
            let len = std::fs::metadata(dir.path().join(name)).unwrap().len();
            assert!(len <= 200, "file larger than the limit: {len}");
        }
        // The newest line is in the current file.
        let current = std::fs::read_to_string(dir.path().join("easysshd.log")).unwrap();
        assert!(current.contains("line number 0039"));
    }

    #[test]
    fn messages_stay_on_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let log = Logger::open(dir.path());
        log.info("a\nINFO forged\r\nentry");
        let text = std::fs::read_to_string(dir.path().join("easysshd.log")).unwrap();
        assert_eq!(text.lines().count(), 1);
    }
}
