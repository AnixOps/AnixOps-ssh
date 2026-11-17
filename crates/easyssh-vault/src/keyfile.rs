//! A saved private key as a file, for the OpenSSH engine only.
//!
//! OpenSSH reads identities from files, so the daemon writes the saved key
//! to a `0600` file in its private runtime directory, passes `-i` for it,
//! and deletes it again when the `ssh` process ends (the [`KeyFile`] drop).
//! A crash can leave one behind, so [`sweep`] clears the directory on start.
//! The `russh` engine never needs this: it takes the key from a
//! [`SecretSource`](crate::SecretSource) in memory.

use easyssh_crypto::SecretText;
use std::io::Write;
use std::path::{Path, PathBuf};

const DIR_NAME: &str = "keys";
const PREFIX: &str = "key-";

/// A temporary key file; removed on drop.
#[derive(Debug)]
pub struct KeyFile {
    path: PathBuf,
}

/// The directory key files go in, under the daemon's runtime directory.
pub fn key_dir(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(DIR_NAME)
}

#[cfg(unix)]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    // The runtime directory lives under the user's profile, which Windows
    // already restricts to the user.
    std::fs::create_dir_all(dir)
}

#[cfg(unix)]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}

impl KeyFile {
    /// Writes `key` to a new `0600` file in `<runtime_dir>/keys`. Line
    /// endings are normalised to `\n` and a final newline is added, which
    /// OpenSSH insists on.
    pub fn write(runtime_dir: &Path, key: &SecretText) -> std::io::Result<Self> {
        let dir = key_dir(runtime_dir);
        private_dir(&dir)?;
        let path = dir.join(format!("{PREFIX}{}", uuid::Uuid::new_v4().simple()));
        let mut file = create_private(&path)?;
        let mut body = zeroize::Zeroizing::new(key.expose().replace("\r\n", "\n"));
        if !body.ends_with('\n') {
            body.push('\n');
        }
        // On failure, do not leave a half-written key behind.
        let written = file.write_all(body.as_bytes()).and_then(|()| file.flush());
        drop(file);
        if let Err(error) = written {
            let _ = std::fs::remove_file(&path);
            return Err(error);
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        // Overwrite first so the bytes do not linger in a freed block.
        if let Ok(meta) = std::fs::metadata(&self.path) {
            if let Ok(mut file) = std::fs::OpenOptions::new().write(true).open(&self.path) {
                let _ = file.write_all(&vec![0u8; meta.len() as usize]);
                let _ = file.sync_all();
            }
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Removes key files a crashed daemon left behind. Returns how many.
pub fn sweep(runtime_dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(key_dir(runtime_dir)) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(PREFIX))
        .filter(|entry| std::fs::remove_file(entry.path()).is_ok())
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_private_normalised_and_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretText::new("-----BEGIN KEY-----\r\nbody\r\n-----END KEY-----");
        let file = KeyFile::write(dir.path(), &key).unwrap();
        let path = file.path().to_owned();
        assert!(path.starts_with(key_dir(dir.path())));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "-----BEGIN KEY-----\nbody\n-----END KEY-----\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = std::fs::metadata(key_dir(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn every_file_has_its_own_name_and_sweep_clears_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretText::new("k\n");
        let a = KeyFile::write(dir.path(), &key).unwrap();
        let b = KeyFile::write(dir.path(), &key).unwrap();
        assert_ne!(a.path(), b.path());
        std::mem::forget(a);
        std::mem::forget(b);
        assert_eq!(sweep(dir.path()), 2);
        assert_eq!(sweep(dir.path()), 0);
        assert_eq!(sweep(&dir.path().join("missing")), 0);
    }
}
