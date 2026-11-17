//! Importing hosts from other SSH clients. The parsing, the "already exists"
//! matching and the merge live in `easyssh_store::import`; this file is the
//! thin layer that picks the route: through the daemon when it is connected
//! (it owns the database and announces `hosts.changed`), straight to the
//! database otherwise. Both routes run the same functions.
//!
//! Nothing here reads the user's SSH files on its own: a file is read because
//! the user picked it, and `~/.ssh/config` only on the command of the
//! "Import from ~/.ssh/config" button.

use crate::daemon::DaemonState;
use crate::hosts::{daemon_down, open_store};
use easyssh_store::import::{
    import_apply, import_preview, read_import_file, ApplyRequest, PreviewRequest,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use tauri::State;

/// The user's OpenSSH client configuration and the directory its `Include`
/// lines resolve in, under `home`.
fn ssh_config_in(home: &Path) -> Result<(PathBuf, PathBuf), String> {
    let dir = home.join(".ssh");
    let file = dir.join("config");
    if file.is_file() {
        Ok((file, dir))
    } else {
        Err("~/.ssh/config was not found".to_owned())
    }
}

async fn preview(
    daemon: &DaemonState,
    path: &Path,
    base_dir: Option<&Path>,
    format: Option<String>,
) -> Result<Value, String> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some(link) = daemon.link() {
        let mut payload = json!({ "path": path, "file_name": file_name });
        if let Some(dir) = base_dir {
            payload["base_dir"] = json!(dir);
        }
        if let Some(format) = &format {
            payload["format"] = json!(format);
        }
        match link.request("hosts.import_preview", payload).await {
            Ok(value) => return Ok(value),
            Err(error) if daemon_down(&error) || error.code == "unknown_command" => {}
            Err(error) => return Err(error.message),
        }
    }
    let bytes = read_import_file(path).map_err(|error| error.to_string())?;
    let preview = import_preview(
        &open_store()?,
        &PreviewRequest {
            bytes: &bytes,
            file_name: &file_name,
            format: format.as_deref(),
            base_dir,
        },
    )
    .map_err(|error| error.to_string())?;
    serde_json::to_value(preview).map_err(|error| error.to_string())
}

/// Parses a file the user picked and marks the hosts that are stored already.
/// Writes nothing.
#[tauri::command]
pub async fn import_preview_file(
    daemon: State<'_, DaemonState>,
    path: String,
    format: Option<String>,
) -> Result<Value, String> {
    preview(&daemon, Path::new(&path), None, format).await
}

/// Reads `~/.ssh/config` (and its `Include`s, inside `~/.ssh`) because the
/// user asked for it, and previews the hosts in it.
#[tauri::command]
pub async fn import_preview_ssh_config(
    daemon: State<'_, DaemonState>,
    format: Option<String>,
) -> Result<Value, String> {
    let home = dirs::home_dir().ok_or("the home directory is unknown")?;
    let (file, dir) = ssh_config_in(&home)?;
    preview(&daemon, &file, Some(&dir), format).await
}

/// Merges the selected hosts. Adds and updates only, never removes.
#[tauri::command]
pub async fn import_apply_hosts(
    daemon: State<'_, DaemonState>,
    request: ApplyRequest,
) -> Result<Value, String> {
    if let Some(link) = daemon.link() {
        let payload = serde_json::to_value(&request).map_err(|error| error.to_string())?;
        match link.request("hosts.import_apply", payload).await {
            Ok(value) => return Ok(value),
            Err(error) if daemon_down(&error) || error.code == "unknown_command" => {}
            Err(error) => return Err(error.message),
        }
    }
    let outcome = import_apply(&open_store()?, &request).map_err(|error| error.to_string())?;
    serde_json::to_value(outcome).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_is_looked_up_under_the_given_home_only() {
        let home = tempfile::tempdir().unwrap();
        assert!(ssh_config_in(home.path()).is_err());
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        std::fs::write(home.path().join(".ssh/config"), "Host a\n").unwrap();
        let (file, dir) = ssh_config_in(home.path()).unwrap();
        assert_eq!(file, home.path().join(".ssh/config"));
        assert_eq!(dir, home.path().join(".ssh"));
    }
}
