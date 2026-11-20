//! Encrypted session log of one terminal session (see `easyssh-sessionlog`
//! for the format).
//!
//! A [`Recorder`] receives the same text the scrollback receives (after the
//! shell-integration script has been filtered out), seals it into chunks of
//! 64 KiB or 2 s, and writes the final chunk when the session ends. If the
//! daemon dies first, the file simply has no final chunk and is shown as
//! incomplete.

use easyssh_sessionlog::{KeyFn, LogStore, LogWriter};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often the timer looks for output that has waited long enough.
const TICK: Duration = Duration::from_millis(500);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

type ErrorHook = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Recorder {
    path: PathBuf,
    writer: Arc<Mutex<Option<LogWriter<File>>>>,
    on_error: ErrorHook,
}

impl Recorder {
    /// Creates `<logs>/<host>/<time>.esl` and writes its header. `derive`
    /// gives the key for the new log's salt (the vault's device key through
    /// HKDF). `on_error` hears about a write that failed, after which the
    /// session goes on unrecorded.
    pub fn start(
        store: &LogStore,
        host: &str,
        derive: KeyFn<'_>,
        on_error: ErrorHook,
    ) -> Result<Arc<Recorder>, String> {
        let started = now_ms();
        let (path, file) = store.create(host, started).map_err(|e| e.to_string())?;
        let writer = match LogWriter::new(file, started, derive) {
            Ok(writer) => writer,
            Err(error) => {
                // Do not leave a file without a header behind.
                let _ = std::fs::remove_file(&path);
                return Err(error.to_string());
            }
        };
        let writer = Arc::new(Mutex::new(Some(writer)));
        spawn_timer(Arc::downgrade(&writer), on_error.clone());
        Ok(Arc::new(Recorder {
            path,
            writer,
            on_error,
        }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Adds terminal output.
    pub fn write(&self, text: &str) {
        let mut guard = lock(&self.writer);
        if let Some(writer) = guard.as_mut() {
            if let Err(error) = writer.write(&easyssh_engine::text_to_bytes(text)) {
                *guard = None;
                (self.on_error)(&error.to_string());
            }
        }
    }

    /// Writes the final chunk. Safe to call more than once.
    pub fn finish(&self) {
        let writer = lock(&self.writer).take();
        if let Some(writer) = writer {
            if let Err(error) = writer.finish(now_ms()) {
                (self.on_error)(&error.to_string());
            }
        }
    }
}

fn spawn_timer(writer: Weak<Mutex<Option<LogWriter<File>>>>, on_error: ErrorHook) {
    std::thread::spawn(move || loop {
        std::thread::sleep(TICK);
        let Some(writer) = writer.upgrade() else {
            return;
        };
        let mut guard = lock(&writer);
        match guard.as_mut() {
            None => return,
            Some(w) => {
                if let Err(error) = w.flush_if_due(Instant::now()) {
                    *guard = None;
                    on_error(&error.to_string());
                    return;
                }
            }
        }
    });
}
