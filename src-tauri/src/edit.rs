//! Editing a remote file in a local editor: the temporary copy in the app's
//! cache folder, opening it, and telling the window when it was saved. The
//! download and the upload back go through the transfer queue like any other.

use crate::settings;
use easyssh_core::edit::{self, EditWatch};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

/// How long a file must stay unchanged before a save is reported.
const DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Edits {
    watches: Mutex<HashMap<String, EditWatch>>,
}

fn root(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_cache_dir()
        .map(|dir| dir.join("edit"))
        .map_err(|error| error.to_string())
}

/// True when `path` is a file the edit feature owns, so a command from the
/// window cannot write or delete anywhere else.
pub fn owns(app: &AppHandle, path: &Path) -> bool {
    root(app).is_ok_and(|root| edit::is_inside(&root, path) && path != root)
}

/// Leftovers of an earlier run; called once at start-up.
pub fn clean_all(app: &AppHandle) {
    if let Ok(root) = root(app) {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// Where the copy of `remote` goes. Any earlier copy is removed, so the
/// download that follows starts clean.
#[tauri::command]
pub fn edit_prepare(app: AppHandle, host_id: String, remote: String) -> Result<String, String> {
    let root = root(&app)?;
    let local =
        edit::local_file(&root, &host_id, &remote).ok_or("the remote path has no file name")?;
    let dir = local.parent().ok_or("no folder for the local copy")?;
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    Ok(local.to_string_lossy().into_owned())
}

/// Opens the copy with the editor from the settings, or the system's
/// default application for the file.
#[tauri::command]
pub fn edit_open(app: AppHandle, local: String) -> Result<(), String> {
    let path = PathBuf::from(&local);
    if !owns(&app, &path) {
        return Err("not a file of the editor cache".to_owned());
    }
    let command = settings::editor_command();
    let parts = edit::split_command(&command);
    if let Some((program, args)) = parts.split_first() {
        std::process::Command::new(program)
            .args(args)
            .arg(&path)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("{program}: {error}"))
    } else {
        app.opener()
            .open_path(local, None::<&str>)
            .map_err(|error| error.to_string())
    }
}

/// Starts reporting saves of `local` as `edit-saved` events carrying `key`.
#[tauri::command]
pub fn edit_watch(
    app: AppHandle,
    edits: State<'_, Edits>,
    key: String,
    local: String,
) -> Result<(), String> {
    let path = PathBuf::from(&local);
    if !owns(&app, &path) {
        return Err("not a file of the editor cache".to_owned());
    }
    let event_key = key.clone();
    let emitter = app.clone();
    let watch = edit::watch(&path, DEBOUNCE, move || {
        let _ = emitter.emit("edit-saved", &event_key);
    })
    .map_err(|error| error.to_string())?;
    edits
        .watches
        .lock()
        .map_err(|_| "edit state is unavailable".to_owned())?
        .insert(key, watch);
    Ok(())
}

/// Stops watching and removes the copy.
#[tauri::command]
pub fn edit_stop(
    app: AppHandle,
    edits: State<'_, Edits>,
    key: String,
    local: String,
) -> Result<(), String> {
    edits
        .watches
        .lock()
        .map_err(|_| "edit state is unavailable".to_owned())?
        .remove(&key);
    let path = PathBuf::from(&local);
    if owns(&app, &path) {
        if let Some(dir) = path.parent() {
            if owns(&app, dir) {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }
    Ok(())
}
