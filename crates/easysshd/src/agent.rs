//! Validating the `SSH_AUTH_SOCK` a client sends with `session.open`.
//!
//! The daemon outlives the user's login, so its own environment can name an
//! agent that no longer exists. The client sends the value of its current
//! environment instead. Because the daemon then puts that path into a child's
//! environment, it only accepts an absolute path to a socket owned by the
//! daemon's own user; anything else is ignored.

use std::path::PathBuf;

/// `Ok(Some(path))` to apply, `Ok(None)` when there is nothing to apply (on
/// Windows), `Err(reason)` when the path is not acceptable.
pub fn validate_agent_socket(path: &str) -> Result<Option<PathBuf>, String> {
    #[cfg(unix)]
    {
        validate_for_uid(path, crate::platform::euid()).map(Some)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(None)
    }
}

/// The check behind [`validate_agent_socket`], for an explicit owner.
#[cfg(unix)]
pub fn validate_for_uid(path: &str, uid: u32) -> Result<PathBuf, String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    if path.is_empty() || path.contains('\0') {
        return Err("empty path or NUL byte".into());
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("not an absolute path".into());
    }
    // Follows symlinks: agents are often reached through one.
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot stat: {e}"))?;
    if !meta.file_type().is_socket() {
        return Err("not a socket".into());
    }
    if meta.uid() != uid {
        return Err("owned by another user".into());
    }
    Ok(path)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn accepts_own_socket_only() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("agent.sock");
        let _listener = UnixListener::bind(&sock).unwrap();
        let uid = crate::platform::euid();
        assert_eq!(validate_for_uid(sock.to_str().unwrap(), uid).unwrap(), sock);
        assert!(validate_for_uid(sock.to_str().unwrap(), uid + 1)
            .unwrap_err()
            .contains("another user"));
        assert!(validate_for_uid("agent.sock", uid)
            .unwrap_err()
            .contains("absolute"));
        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        assert!(validate_for_uid(file.to_str().unwrap(), uid)
            .unwrap_err()
            .contains("not a socket"));
        assert!(validate_for_uid("/nonexistent/agent", uid).is_err());
        assert!(validate_for_uid("", uid).is_err());
    }
}
