//! The server monitor for hosts on the OpenSSH engine.
//!
//! An OpenSSH terminal session is started as the master of a control socket
//! (`ssh -M -S <runtime>/cm-<id>`, Unix only). The monitor runs its script
//! through that socket (`ssh -S <socket> -o ControlMaster=no -o BatchMode=yes`),
//! so it rides the connection the terminal already has: no new login, no
//! prompt. When no socket exists the host counts as not connected. Windows
//! OpenSSH has no ControlMaster, so there the monitor keeps answering
//! `wrong_engine`.

use easyssh_core::{Connection, OpenSsh, SshInvocation};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// `sun_path` is 104 bytes on macOS (108 on Linux), and ssh binds a temporary
/// name 17 bytes longer than the socket before renaming it.
const MAX_SOCKET_PATH: usize = 104 - 1 - 17;

/// How long one command may take through the master.
const RUN_TIMEOUT: Duration = Duration::from_secs(15);

/// Why a command through the master did not run.
#[derive(Debug, PartialEq, Eq)]
pub enum Failed {
    /// There is no master socket (the terminal is closed, or the platform has
    /// no ControlMaster).
    NotConnected,
    /// `ssh` could not be started, timed out or exited with an error.
    Error(String),
}

/// Where the master of session `session_id` listens, when the platform
/// supports it and the path fits a socket name.
pub fn control_path(runtime_dir: &Path, session_id: &str) -> Option<PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    let short: String = session_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(10)
        .collect();
    if short.is_empty() {
        return None;
    }
    let path = runtime_dir.join(format!("cm-{short}"));
    (path.as_os_str().len() <= MAX_SOCKET_PATH).then_some(path)
}

/// Whether a master socket is there now.
pub fn socket_exists(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_socket())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Runs `command` over the master at `control` and returns its standard
/// output. A nonzero exit status is an error carrying what `ssh` printed.
pub async fn run(connection: &Connection, control: &Path, command: &str) -> Result<String, Failed> {
    if !socket_exists(control) {
        return Err(Failed::NotConnected);
    }
    let invocation = SshInvocation::for_control_command(&OpenSsh, connection, control, command)
        .map_err(|e| Failed::Error(e.to_string()))?;
    let mut process = tokio::process::Command::new(&invocation.executable);
    process
        .args(&invocation.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        process.creation_flags(CREATE_NO_WINDOW);
    }
    let output = tokio::time::timeout(RUN_TIMEOUT, process.output())
        .await
        .map_err(|_| Failed::Error("the server took too long to answer".into()))?
        .map_err(|e| Failed::Error(e.to_string()))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    let text = String::from_utf8_lossy(&output.stderr);
    let text = text.trim();
    // The master went away between the check and the run.
    if !socket_exists(control) {
        return Err(Failed::NotConnected);
    }
    Err(Failed::Error(if text.is_empty() {
        format!("ssh exited with {}", output.status)
    } else {
        text.to_owned()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_path_is_short_and_per_session() {
        let dir = Path::new("/run/user/1000/easyssh");
        let a = control_path(dir, "0123456789abcdef0123456789abcdef");
        if cfg!(unix) {
            assert_eq!(
                a,
                Some(PathBuf::from("/run/user/1000/easyssh/cm-0123456789"))
            );
            assert_ne!(a, control_path(dir, "fedcba9876543210"));
        } else {
            assert_eq!(a, None);
        }
    }

    #[test]
    fn control_path_gives_up_on_long_runtime_dirs() {
        let dir = PathBuf::from(format!("/{}", "d".repeat(120)));
        assert_eq!(control_path(&dir, "abcdef"), None);
        assert_eq!(control_path(Path::new("/run"), "--"), None);
    }

    #[tokio::test]
    async fn no_socket_means_not_connected() {
        let connection = Connection::alias("Prod", "production");
        let result = run(&connection, Path::new("/nonexistent/cm-x"), "uptime").await;
        assert_eq!(result, Err(Failed::NotConnected));
    }
}
