//! Sync, team vaults and host history: a relay to the daemon.
//!
//! The daemon owns the account, the sync loop and the team vaults, so there
//! is no in-process fallback: without a daemon connection these commands
//! fail with the code `unavailable` and the UI says sync needs the
//! background service. One generic command carries them, restricted to the
//! allowlist below so the webview cannot reach anything else through it.

use crate::daemon::DaemonState;
use serde::Serialize;
use serde_json::Value;
use tauri::State;

const ALLOWED: &[&str] = &[
    "sync.status",
    "sync.signup",
    "sync.signin",
    "sync.signout",
    "sync.now",
    "sync.devices.list",
    "sync.devices.revoke",
    "vaults.list",
    "vault.create_shared",
    "vault.members.list",
    "vault.members.lookup",
    "vault.members.add",
    "vault.members.remove",
    "vault.members.role",
    "hosts.move_vault",
    "hosts.vaults",
    "hosts.history",
    "hosts.history_restore",
];

/// The error the UI receives: a stable code and a message to show.
#[derive(Debug, Serialize)]
pub struct SyncError {
    code: String,
    message: String,
}

pub(crate) fn allowed(kind: &str) -> bool {
    ALLOWED.contains(&kind)
}

#[tauri::command]
pub async fn sync_call(
    daemon: State<'_, DaemonState>,
    kind: String,
    payload: Option<Value>,
) -> Result<Value, SyncError> {
    if !allowed(&kind) {
        return Err(SyncError {
            code: "invalid_request".into(),
            message: format!("command not available: {kind}"),
        });
    }
    let Some(link) = daemon.link() else {
        return Err(SyncError {
            code: "unavailable".into(),
            message: "the background service is not running".into(),
        });
    };
    link.request(&kind, payload.unwrap_or_else(|| serde_json::json!({})))
        .await
        .map_err(|error| SyncError {
            code: error.code,
            message: error.message,
        })
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn only_sync_commands_pass() {
        assert!(allowed("sync.signin"));
        assert!(allowed("hosts.history"));
        assert!(!allowed("sessions.open"));
        assert!(!allowed("vault.unlock"));
        assert!(!allowed("shutdown"));
    }
}
