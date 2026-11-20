//! Server monitor commands. The daemon reads the server on the connection
//! the terminal already has; hosts on OpenSSH have no shared connection to
//! read through, so they answer `wrong_engine`.

use crate::daemon::DaemonState;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

/// A failure the UI tells apart by `code` (`wrong_engine`, `not_connected`,
/// `unavailable`, ...).
#[derive(Serialize)]
pub struct MonitorError {
    code: String,
    message: String,
}

fn unavailable() -> MonitorError {
    MonitorError {
        code: "unavailable".into(),
        message: "the connection to the session daemon was lost".into(),
    }
}

async fn request(daemon: &DaemonState, kind: &str, payload: Value) -> Result<Value, MonitorError> {
    let link = daemon.link().ok_or_else(unavailable)?;
    link.request(kind, payload)
        .await
        .map_err(|error| MonitorError {
            code: error.code,
            message: error.message,
        })
}

/// `monitor.sample`: the raw sample script output and the latency.
#[tauri::command]
pub async fn monitor_sample(
    daemon: State<'_, DaemonState>,
    host_id: String,
    detail: bool,
) -> Result<Value, MonitorError> {
    request(
        &daemon,
        "monitor.sample",
        json!({ "host_id": host_id, "detail": detail }),
    )
    .await
}

/// `monitor.signal`: TERM, KILL, HUP or INT to one process.
#[tauri::command]
pub async fn monitor_signal(
    daemon: State<'_, DaemonState>,
    host_id: String,
    pid: u32,
    signal: String,
) -> Result<(), MonitorError> {
    request(
        &daemon,
        "monitor.signal",
        json!({ "host_id": host_id, "pid": pid, "signal": signal }),
    )
    .await
    .map(|_| ())
}
