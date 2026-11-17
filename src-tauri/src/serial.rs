//! Serial ports for the host editor's picker and the new-tab menu.
//!
//! The daemon owns serial sessions, so listing goes to it; without a daemon
//! connection the app lists the ports itself (the picker still works, opening
//! a serial host needs the daemon).

use crate::daemon::DaemonState;
use crate::hosts::daemon_down;
use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SerialPortView {
    path: String,
    description: Option<String>,
}

fn view_of(value: &Value) -> Vec<SerialPortView> {
    value["ports"]
        .as_array()
        .map(|ports| {
            ports
                .iter()
                .filter_map(|port| {
                    Some(SerialPortView {
                        path: port["path"].as_str()?.to_owned(),
                        description: port["description"].as_str().map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tauri::command]
pub async fn serial_ports(daemon: State<'_, DaemonState>) -> Result<Vec<SerialPortView>, String> {
    if let Some(link) = daemon.link() {
        match link.request("serial.ports", json!({})).await {
            Ok(value) => return Ok(view_of(&value)),
            Err(error) if daemon_down(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    let ports = tauri::async_runtime::spawn_blocking(easysshd::serial::list_ports)
        .await
        .map_err(|error| error.to_string())??;
    Ok(ports
        .into_iter()
        .map(|port| SerialPortView {
            path: port.path,
            description: port.description,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_daemons_reply_becomes_picker_entries() {
        let reply = json!({"ports": [
            {"path": "/dev/ttyUSB0", "description": "FTDI (USB 0403:6001)"},
            {"path": "/dev/ttyS0", "description": null},
        ]});
        let ports = view_of(&reply);
        assert_eq!(ports.len(), 2);
        assert_eq!(ports[0].path, "/dev/ttyUSB0");
        assert_eq!(ports[1].description, None);
        assert!(view_of(&json!({})).is_empty());
    }
}
