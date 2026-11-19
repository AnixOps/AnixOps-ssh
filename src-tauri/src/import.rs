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
use crate::vault::app_vault;
use easyssh_services::{settings as service_settings, Service, ServiceForm};
use easyssh_store::import::{
    import_apply, import_preview, read_import_file, ApplyRequest, PreviewRequest,
};
use serde::Deserialize;
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

/// What the "import from a service" form sends. `secret` is only set when the
/// user typed a new credential; the saved one is used otherwise.
#[derive(Debug, Deserialize)]
pub struct ServiceRequest {
    service: String,
    form: ServiceForm,
    #[serde(default)]
    secret: Option<String>,
    /// The user cleared the saved credential.
    #[serde(default)]
    clear_secret: bool,
}

fn service_of(id: &str) -> Result<Service, String> {
    Service::from_id(id).ok_or_else(|| format!("unknown service {id}"))
}

/// The form values saved by the last import from `service`, and whether a
/// credential is saved. The credential itself never leaves the vault.
#[tauri::command]
pub fn service_remembered(service: String) -> Result<service_settings::Remembered, String> {
    Ok(service_settings::remembered(
        &open_store()?,
        service_of(&service)?,
    ))
}

/// Applies the form's credential choice: clears the saved one when asked and
/// returns the credential to use now (the typed one, else the saved one).
fn credential(
    store: &easyssh_store::Store,
    service: Service,
    request: &ServiceRequest,
) -> Result<zeroize::Zeroizing<String>, String> {
    let vault = app_vault();
    if request.clear_secret {
        service_settings::set_secret(store, &vault, service, None).map_err(|e| e.to_string())?;
    }
    easyssh_services::credential(store, &vault, service, request.secret.as_deref())
        .map_err(|e| e.to_string())
}

/// After a request that worked: remembers the form and a newly typed
/// credential (sealed by the vault).
fn remember(
    store: &easyssh_store::Store,
    service: Service,
    request: &ServiceRequest,
    secret: &str,
) -> Result<(), String> {
    service_settings::save_form(store, service, &request.form).map_err(|e| e.to_string())?;
    if request
        .secret
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        service_settings::set_secret(store, &app_vault(), service, Some(secret))
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Tests the address and credential with one small request. Returns how many
/// hosts the service lists. A credential that worked is saved.
#[tauri::command]
pub async fn service_test(request: ServiceRequest) -> Result<usize, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let service = service_of(&request.service)?;
        let store = open_store()?;
        let secret = credential(&store, service, &request)?;
        let total = easyssh_services::test(service, &request.form, &secret)
            .map_err(|error| error.to_string())?;
        remember(&store, service, &request, &secret)?;
        Ok(total)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Reads the hosts of a service the user named and previews them like a file:
/// marks the stored ones and lists what the service no longer has. Writes
/// nothing but the remembered form and credential. The merge is
/// `import_apply_hosts`, the same as for a file.
#[tauri::command]
pub async fn service_preview(request: ServiceRequest) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let service = service_of(&request.service)?;
        let store = open_store()?;
        let secret = credential(&store, service, &request)?;
        let fetched = easyssh_services::fetch(service, &request.form, &secret)
            .map_err(|error| error.to_string())?;
        remember(&store, service, &request, &secret)?;
        let preview = fetched.preview(&store).map_err(|error| error.to_string())?;
        serde_json::to_value(preview).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
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
