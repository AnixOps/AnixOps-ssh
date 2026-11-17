//! Known hosts: the host keys kept in the app database. The list and the
//! removal go through the daemon when it is connected (it owns the database)
//! and straight to the database otherwise. The import reads the user's
//! `~/.ssh/known_hosts` on the command of the settings screen; nothing reads
//! it on its own.

use crate::daemon::DaemonState;
use crate::hosts::{daemon_down, open_store};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::PathBuf;
use tauri::State;

/// A stored entry as the settings screen shows it. The key itself stays in
/// the database; only its fingerprint is shown.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all(serialize = "camelCase", deserialize = "snake_case"))]
pub struct KnownHostView {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub hashed: bool,
    pub marker: Option<String>,
    pub key_type: String,
    pub fingerprint: String,
    pub comment: Option<String>,
    pub added_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SkippedLineView {
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct KnownHostsImportView {
    pub imported: usize,
    pub duplicates: usize,
    pub skipped: Vec<SkippedLineView>,
}

fn user_known_hosts() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".ssh").join("known_hosts"))
        .filter(|path| path.is_file())
        .ok_or_else(|| "~/.ssh/known_hosts was not found".to_owned())
}

#[tauri::command]
pub async fn known_hosts_list(
    daemon: State<'_, DaemonState>,
) -> Result<Vec<KnownHostView>, String> {
    if let Some(link) = daemon.link() {
        match link.request("known_hosts.list", json!({})).await {
            Ok(value) => {
                return serde_json::from_value(value["entries"].clone())
                    .map_err(|error| error.to_string())
            }
            Err(error) if daemon_down(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    open_store()?
        .list_known_hosts()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|record| {
            serde_json::from_value(record.item.summary_json()).map_err(|error| error.to_string())
        })
        .collect()
}

/// Imports `~/.ssh/known_hosts`. Entries already stored are counted, not
/// duplicated; lines that cannot be read are reported.
#[tauri::command]
pub async fn known_hosts_import(
    daemon: State<'_, DaemonState>,
) -> Result<KnownHostsImportView, String> {
    let path = user_known_hosts()?;
    if let Some(link) = daemon.link() {
        match link
            .request("known_hosts.import", json!({ "path": path }))
            .await
        {
            Ok(value) => {
                return serde_json::from_value(value).map_err(|error| error.to_string());
            }
            Err(error) if daemon_down(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    let report = open_store()?
        .import_known_hosts_file(&path)
        .map_err(|error| error.to_string())?;
    Ok(KnownHostsImportView {
        imported: report.imported,
        duplicates: report.duplicates,
        skipped: report
            .skipped
            .into_iter()
            .map(|line| SkippedLineView {
                line: line.line,
                reason: line.reason,
            })
            .collect(),
    })
}

#[tauri::command]
pub async fn known_hosts_remove(daemon: State<'_, DaemonState>, id: String) -> Result<(), String> {
    if let Some(link) = daemon.link() {
        match link
            .request("known_hosts.remove", json!({ "id": id }))
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) if daemon_down(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    open_store()?
        .soft_delete_known_host(&id)
        .map(|_| ())
        .map_err(|error| error.to_string())
}
