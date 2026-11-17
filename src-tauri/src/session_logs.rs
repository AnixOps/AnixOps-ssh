//! Encrypted session logs: settings, the list, the viewer and plain-text
//! export. The daemon writes the logs (see `easysshd::recorder`); this side
//! only reads them, with the key derived from the same device key.
//!
//! Logs of sessions the app runs itself (while the daemon is unavailable)
//! are not recorded.

use crate::hosts::open_store;
use crate::vault::app_vault;
use easyssh_core::ConfigStore;
use easyssh_sessionlog::text::to_plain_text;
use easyssh_sessionlog::{decrypt, Decrypted, LogSettings, LogStore, KEY_INFO};
use serde::Serialize;
use std::path::Path;

/// The viewer reads at most this much of one log file.
const MAX_LOG_BYTES: u64 = 64 * 1024 * 1024;

fn log_store() -> Result<LogStore, String> {
    let v1 = ConfigStore::default_path().map_err(|error| error.to_string())?;
    let db = easyssh_store::db_path_for(v1.path());
    Ok(LogStore::in_data_dir(
        db.parent().unwrap_or_else(|| Path::new(".")),
    ))
}

fn load_settings() -> Result<(easyssh_store::Store, LogSettings), String> {
    let store = open_store()?;
    let settings = store
        .get_setting_as::<LogSettings>(LogSettings::KEY)
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    Ok((store, settings))
}

fn save_settings(store: &easyssh_store::Store, settings: &LogSettings) -> Result<(), String> {
    settings.validate()?;
    store
        .set_setting_as(LogSettings::KEY, settings)
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn session_logs_settings_get() -> Result<LogSettings, String> {
    Ok(load_settings()?.1)
}

/// The global default and the retention; per-host choices are kept.
#[tauri::command]
pub fn session_logs_settings_save(
    record_by_default: bool,
    retention_days: u32,
) -> Result<LogSettings, String> {
    let (store, mut settings) = load_settings()?;
    settings.record_by_default = record_by_default;
    settings.retention_days = retention_days;
    save_settings(&store, &settings)?;
    Ok(settings)
}

/// One host's choice: on, off, or `None` to follow the default.
#[tauri::command]
pub fn session_log_host_set(host_id: String, record: Option<bool>) -> Result<LogSettings, String> {
    let (store, mut settings) = load_settings()?;
    settings.set_host(&host_id, record);
    save_settings(&store, &settings)?;
    Ok(settings)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogView {
    id: String,
    host: String,
    started_ms: u64,
    /// From the start to the last write; exact for a finished log.
    duration_ms: u64,
    size: u64,
}

#[tauri::command]
pub fn session_logs_list() -> Result<Vec<LogView>, String> {
    Ok(log_store()?
        .list()
        .into_iter()
        .map(|entry| LogView {
            duration_ms: entry.modified_ms.saturating_sub(entry.started_ms),
            id: entry.id,
            host: entry.host,
            started_ms: entry.started_ms,
            size: entry.size,
        })
        .collect())
}

fn read_log(id: &str) -> Result<Decrypted, String> {
    let bytes = log_store()?
        .read(id, MAX_LOG_BYTES)
        .map_err(|error| error.to_string())?;
    let vault = app_vault();
    let derive = |salt: &[u8]| {
        vault
            .derive_key(KEY_INFO, salt)
            .map_err(|error| error.to_string())
    };
    decrypt(&bytes, &derive).map_err(|error| error.to_string())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedLog {
    /// The terminal output, escape sequences included, for a read-only xterm.
    text: String,
    /// False when the log has no final chunk (the session was cut off).
    complete: bool,
    started_ms: u64,
    ended_ms: Option<u64>,
}

/// Decrypts a log for the viewer. A modified or damaged log is an error.
#[tauri::command]
pub async fn session_log_open(id: String) -> Result<OpenedLog, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let log = read_log(&id)?;
        Ok(OpenedLog {
            text: String::from_utf8_lossy(&log.data).into_owned(),
            complete: log.complete,
            started_ms: log.header.started_ms,
            ended_ms: log.ended_ms,
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Writes the log as plain text to `path`. The window asks the user first.
#[tauri::command]
pub async fn session_log_export(id: String, path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let target = Path::new(&path);
        if !target.is_absolute() {
            return Err("choose a full file path".to_owned());
        }
        let log = read_log(&id)?;
        std::fs::write(target, to_plain_text(&log.data))
            .map_err(|error| format!("cannot write the file: {error}"))
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
pub fn session_log_delete(id: String) -> Result<(), String> {
    log_store()?.delete(&id).map_err(|error| error.to_string())
}
