//! The per-user runtime directory: endpoint name, instance lock, secret file.

use crate::error::{Error, Result};
use crate::platform;
use crate::session::{random_bytes, Secret, SECRET_LEN};
use fs4::fs_std::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Paths {
    pub dir: PathBuf,
    /// Unix domain socket path (unused on Windows, where the pipe name is derived).
    pub socket: PathBuf,
    pub lock: PathBuf,
    pub secret: PathBuf,
}

impl Paths {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let p = Paths {
            socket: dir.join("easysshd.sock"),
            lock: dir.join("easysshd.lock"),
            secret: dir.join("easysshd.secret"),
            dir,
        };
        platform::check_endpoint_len(&p.socket)?;
        Ok(p)
    }
}

/// Default runtime directory for the current user and platform.
pub fn default_runtime_dir() -> PathBuf {
    platform::default_runtime_dir()
}

/// Holds the single-instance lock; the lock is released when dropped.
#[derive(Debug)]
pub struct InstanceLock {
    file: File,
}

impl InstanceLock {
    pub fn acquire(paths: &Paths) -> Result<Self> {
        let mut opts = platform::private_open_options();
        opts.read(true).write(true).create(true).truncate(false);
        let file = opts.open(&paths.lock)?;
        match file.try_lock_exclusive() {
            Ok(()) => {}
            Err(e) if e.kind() == fs4::lock_contended_error().kind() => {
                return Err(Error::AlreadyRunning(paths.dir.clone()))
            }
            Err(e) => return Err(e.into()),
        }
        let mut f = &file;
        f.set_len(0)?;
        write!(f, "{}", std::process::id())?;
        Ok(InstanceLock { file })
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Generate a fresh secret and write it owner-only, replacing any stale file.
/// The caller must hold the instance lock.
pub fn write_new_secret(paths: &Paths) -> Result<Secret> {
    let secret: Secret = random_bytes();
    match std::fs::remove_file(&paths.secret) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let mut opts = platform::private_open_options();
    opts.write(true).create_new(true);
    let mut f = opts.open(&paths.secret)?;
    f.write_all(&secret)?;
    f.sync_all()?;
    Ok(secret)
}

/// Read the secret the daemon published, refusing files that other users could
/// have written or read.
pub fn read_secret(paths: &Paths) -> Result<Secret> {
    let mut f = OpenOptions::new().read(true).open(&paths.secret)?;
    platform::verify_private_file(&paths.secret, &f)?;
    let mut buf = Vec::with_capacity(SECRET_LEN);
    // Read one byte past the expected size so an oversized file is detected.
    (&mut f).take(SECRET_LEN as u64 + 1).read_to_end(&mut buf)?;
    buf.try_into().map_err(|_| Error::InsecurePath {
        path: paths.secret.clone(),
        reason: format!("secret file must be exactly {SECRET_LEN} bytes"),
    })
}

/// Create the runtime directory if needed and verify it is private.
pub fn prepare_dir(paths: &Paths) -> Result<()> {
    platform::prepare_dir(&paths.dir)
}

/// Remove the files a running server owns.
pub fn cleanup(paths: &Paths) {
    for p in [&paths.socket, &paths.secret, &paths.lock] {
        let _ = std::fs::remove_file(p);
    }
}
