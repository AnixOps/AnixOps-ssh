//! Unix: permission bits and ownership are the access control.

use crate::error::{Error, Result};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

// sun_path is 108 bytes on Linux and 104 on macOS/BSD, including the NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
const SUN_PATH_MAX: usize = 108;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const SUN_PATH_MAX: usize = 104;

pub fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

pub fn default_runtime_dir() -> PathBuf {
    if let Some(x) = std::env::var_os("XDG_RUNTIME_DIR") {
        let x = PathBuf::from(x);
        if x.is_absolute() {
            return x.join("easyssh");
        }
    }
    // macOS: TMPDIR is already per-user. Elsewhere this is a fallback; the uid
    // in the name keeps users apart and the ownership check catches squatters.
    std::env::temp_dir().join(format!("easyssh-{}", euid()))
}

pub fn check_endpoint_len(socket: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let len = socket.as_os_str().as_bytes().len();
    if len >= SUN_PATH_MAX {
        return Err(Error::SocketPathTooLong {
            path: socket.to_path_buf(),
            len,
            max: SUN_PATH_MAX - 1,
        });
    }
    Ok(())
}

pub fn private_open_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.mode(0o600);
    o
}

fn insecure(path: &Path, reason: impl Into<String>) -> Error {
    Error::InsecurePath {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// Create the directory (mode 0700) if missing, then require that it is a real
/// directory owned by us with no group/other permission bits.
pub fn prepare_dir(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    // symlink_metadata: do not follow a planted symlink.
    let md = std::fs::symlink_metadata(dir)?;
    if !md.is_dir() {
        return Err(insecure(dir, "not a directory (or a symlink)"));
    }
    if md.uid() != euid() {
        return Err(insecure(
            dir,
            format!("owned by uid {}, expected {}", md.uid(), euid()),
        ));
    }
    if md.mode() & 0o077 != 0 {
        return Err(insecure(
            dir,
            format!("mode is {:o}, expected 700", md.mode() & 0o7777),
        ));
    }
    Ok(())
}

pub fn verify_private_file(path: &Path, f: &File) -> Result<()> {
    let md = f.metadata()?;
    if !md.is_file() {
        return Err(insecure(path, "not a regular file"));
    }
    if md.uid() != euid() {
        return Err(insecure(path, "owned by a different user"));
    }
    if md.mode() & 0o077 != 0 {
        return Err(insecure(
            path,
            format!("mode is {:o}, expected 600", md.mode() & 0o7777),
        ));
    }
    Ok(())
}
