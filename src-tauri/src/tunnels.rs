//! Port-forwarding tunnels: saved in the config, run as `ssh -N`, and
//! reported to the frontend through the `tunnel-status` event.
//!
//! Hosts on the built-in engine run their tunnels in the daemon, which keeps
//! them up across dropped connections. An `ssh -N` that exits after it was
//! up (the connection dropped) is started again with growing delays, the
//! same [`ReconnectPolicy`] the built-in engine uses.

use crate::askpass;
use crate::daemon::{self, DaemonState};
use crate::hosts::{self, load_config, open_store};
use easyssh_core::tunnel::spawn_tunnel;
use easyssh_core::{OpenSsh, Tunnel, TunnelKind};
use easyssh_engine::supervisor::ReconnectPolicy;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

/// How long a remote forward must stay up before it counts as running; it
/// cannot be probed from this computer.
const REMOTE_SETTLE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum TunnelStatus {
    Stopped,
    Starting,
    Running {
        /// Why the last connection through the forward could not be passed
        /// on (the tunnel itself keeps running).
        #[serde(rename = "lastError", skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
        #[serde(rename = "errorCount", skip_serializing_if = "is_zero")]
        error_count: u64,
    },
    /// The connection dropped; a new one is being made.
    Reconnecting {
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Failed {
        message: String,
    },
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

impl TunnelStatus {
    fn running() -> TunnelStatus {
        TunnelStatus::Running {
            last_error: None,
            error_count: 0,
        }
    }

    /// Whether the tunnel is up or on its way (the switch shows it on).
    fn is_active(&self) -> bool {
        matches!(
            self,
            TunnelStatus::Starting
                | TunnelStatus::Running { .. }
                | TunnelStatus::Reconnecting { .. }
        )
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusEvent {
    id: String,
    status: TunnelStatus,
}

struct RunningTunnel {
    child: Mutex<Child>,
    stopping: AtomicBool,
}

#[derive(Default)]
pub struct Tunnels {
    running: Mutex<HashMap<String, Arc<RunningTunnel>>>,
    /// Tunnels that run in the daemon (built-in engine).
    remote: Mutex<HashSet<String>>,
    statuses: Mutex<HashMap<String, TunnelStatus>>,
}

impl Tunnels {
    /// Stops every tunnel; called when the app exits.
    pub fn stop_all(&self) {
        if let Ok(mut running) = self.running.lock() {
            for (_, tunnel) in running.drain() {
                tunnel.stopping.store(true, Ordering::Relaxed);
                if let Ok(mut child) = tunnel.child.lock() {
                    let _ = child.kill();
                }
            }
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelView {
    id: String,
    name: String,
    host_id: String,
    kind: TunnelKind,
    bind_address: Option<String>,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
    status: TunnelStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelInput {
    id: Option<String>,
    name: String,
    host_id: String,
    kind: TunnelKind,
    bind_address: Option<String>,
    bind_port: u16,
    target_host: Option<String>,
    target_port: Option<u16>,
}

fn lock_error<T>(_: T) -> String {
    "tunnel state is unavailable".to_owned()
}

pub(crate) fn emit(app: &AppHandle, id: &str, status: TunnelStatus) {
    if let Ok(mut statuses) = app.state::<Tunnels>().statuses.lock() {
        statuses.insert(id.to_owned(), status.clone());
    }
    let _ = app.emit(
        "tunnel-status",
        StatusEvent {
            id: id.to_owned(),
            status,
        },
    );
}

/// A status the daemon reported for one of its tunnels.
pub fn remote_status(app: &AppHandle, status: easysshd::link::TunnelStatus) {
    let tunnels = app.state::<Tunnels>();
    let next = daemon_status(&status);
    if let Ok(mut remote) = tunnels.remote.lock() {
        if next.is_active() {
            remote.insert(status.id.clone());
        } else {
            remote.remove(&status.id);
        }
    }
    emit(app, &status.id, next);
}

fn daemon_status(status: &easysshd::link::TunnelStatus) -> TunnelStatus {
    match status.state.as_str() {
        "starting" => TunnelStatus::Starting,
        "running" => TunnelStatus::Running {
            last_error: status
                .last_channel_error
                .clone()
                .filter(|_| status.channel_errors > 0),
            error_count: status.channel_errors,
        },
        "reconnecting" => TunnelStatus::Reconnecting {
            message: status.error.clone(),
        },
        "stopped" => TunnelStatus::Stopped,
        _ => TunnelStatus::Failed {
            message: status
                .error
                .clone()
                .unwrap_or_else(|| "the tunnel stopped".to_owned()),
        },
    }
}

/// After the link to the daemon came back: the daemon may have restarted
/// and lost its tunnels, so ask it which run and mark the rest stopped.
pub fn resync_remote(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let Some(link) = app.state::<DaemonState>().link() else {
            return;
        };
        let rows: Vec<easysshd::link::TunnelStatus> = link
            .request("tunnel.list", serde_json::json!({}))
            .await
            .ok()
            .and_then(|answer| answer["tunnels"].as_array().cloned())
            .unwrap_or_default()
            .iter()
            .map(easysshd::link::TunnelStatus::from_payload)
            .collect();
        let known: Vec<String> = app
            .state::<Tunnels>()
            .remote
            .lock()
            .map(|remote| remote.iter().cloned().collect())
            .unwrap_or_default();
        for id in stale_tunnels(&known, &rows) {
            if let Ok(mut remote) = app.state::<Tunnels>().remote.lock() {
                remote.remove(&id);
            }
            emit(&app, &id, TunnelStatus::Stopped);
        }
        for row in rows {
            remote_status(&app, row);
        }
    });
}

/// The tunnels the app believes run in the daemon that the daemon does not
/// run (any more).
fn stale_tunnels(known: &[String], rows: &[easysshd::link::TunnelStatus]) -> Vec<String> {
    known
        .iter()
        .filter(|id| {
            !rows
                .iter()
                .any(|row| &row.id == *id && daemon_status(row).is_active())
        })
        .cloned()
        .collect()
}

/// Stops the daemon's tunnels when the app exits (they belong to the app).
pub fn stop_remote(app: &AppHandle) {
    let ids: Vec<String> = app
        .state::<Tunnels>()
        .remote
        .lock()
        .map(|remote| remote.iter().cloned().collect())
        .unwrap_or_default();
    let Some(link) = app.state::<DaemonState>().link() else {
        return;
    };
    for id in ids {
        let _ = tauri::async_runtime::block_on(tokio::time::timeout(
            Duration::from_secs(2),
            link.request("tunnel.stop", serde_json::json!({ "id": id })),
        ));
    }
}

#[tauri::command]
pub fn tunnels_list(tunnels: State<'_, Tunnels>) -> Result<Vec<TunnelView>, String> {
    let statuses = tunnels.statuses.lock().map_err(lock_error)?;
    Ok(load_config()?
        .tunnels
        .into_iter()
        .map(|tunnel| TunnelView {
            status: statuses
                .get(&tunnel.id)
                .cloned()
                .unwrap_or(TunnelStatus::Stopped),
            id: tunnel.id,
            name: tunnel.name,
            host_id: tunnel.host_id,
            kind: tunnel.kind,
            bind_address: tunnel.bind_address,
            bind_port: tunnel.bind_port,
            target_host: tunnel.target_host,
            target_port: tunnel.target_port,
        })
        .collect())
}

fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Creates or updates a tunnel and returns its id.
#[tauri::command]
pub fn tunnel_save(input: TunnelInput) -> Result<String, String> {
    let name = input.name.trim().to_owned();
    if name.is_empty() || name.chars().any(char::is_control) {
        return Err("the name is required".to_owned());
    }
    let dynamic = input.kind == TunnelKind::Dynamic;
    let tunnel = Tunnel {
        id: input
            .id
            .unwrap_or_else(|| format!("tunnel-{}", chrono::Utc::now().timestamp_millis())),
        name,
        host_id: input.host_id,
        kind: input.kind,
        bind_address: trimmed(input.bind_address),
        bind_port: input.bind_port,
        target_host: if dynamic {
            None
        } else {
            trimmed(input.target_host)
        },
        target_port: if dynamic { None } else { input.target_port },
    };
    tunnel.forward_spec().map_err(|error| error.to_string())?;
    hosts::connection(&tunnel.host_id)?;
    open_store()?
        .save_tunnel(&tunnel)
        .map_err(|error| error.to_string())?;
    Ok(tunnel.id)
}

#[tauri::command]
pub async fn tunnel_delete(app: AppHandle, id: String) -> Result<(), String> {
    tunnel_stop(app, id.clone()).await?;
    open_store()?
        .remove_tunnel(&id)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn tunnel_start(app: AppHandle, id: String) -> Result<(), String> {
    let tunnels = app.state::<Tunnels>();
    if tunnels
        .running
        .lock()
        .map_err(lock_error)?
        .contains_key(&id)
    {
        return Ok(());
    }
    let tunnel = load_config()?
        .tunnels
        .into_iter()
        .find(|tunnel| tunnel.id == id)
        .ok_or("unknown tunnel")?;
    if tunnels.remote.lock().map_err(lock_error)?.contains(&id) {
        return Ok(());
    }
    // Hosts on the built-in engine run the tunnel in the daemon, on the
    // connection the terminal already has.
    let daemon_state = app.state::<DaemonState>();
    if let Some(link) = daemon::russh_link(&daemon_state, &tunnel.host_id).await {
        match link
            .request("tunnel.start", serde_json::json!({ "tunnel": tunnel }))
            .await
        {
            Ok(_) => {
                tunnels.remote.lock().map_err(lock_error)?.insert(id);
                return Ok(());
            }
            Err(error) if error.code == "already_running" => return Ok(()),
            Err(error) if daemon::use_openssh(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    let (child, errors) = spawn(&app, &tunnel)?;
    let running = Arc::new(RunningTunnel {
        child: Mutex::new(child),
        stopping: AtomicBool::new(false),
    });
    tunnels
        .running
        .lock()
        .map_err(lock_error)?
        .insert(id.clone(), running.clone());
    emit(&app, &id, TunnelStatus::Starting);
    std::thread::spawn(move || watch(&app, &id, &tunnel, &running, errors));
    Ok(())
}

/// Starts `ssh -N` for `tunnel`; its standard error is collected in the
/// returned string.
fn spawn(app: &AppHandle, tunnel: &Tunnel) -> Result<(Child, Arc<Mutex<String>>), String> {
    let connection = hosts::connection(&tunnel.host_id)?;
    let env = askpass::env_for(app, &connection.name, Some(&connection.id))?;
    let mut child =
        spawn_tunnel(&OpenSsh, &connection, tunnel, &env).map_err(|error| error.to_string())?;
    let stderr = child.stderr.take();
    let errors = Arc::new(Mutex::new(String::new()));
    let collected = errors.clone();
    std::thread::spawn(move || {
        if let Some(mut stderr) = stderr {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            if let Ok(mut errors) = collected.lock() {
                *errors = text;
            }
        }
    });
    Ok((child, errors))
}

/// Sleeps for `delay` unless the tunnel is stopped meanwhile (`false`).
fn wait_unless_stopped(delay: Duration, running: &RunningTunnel) -> bool {
    let until = Instant::now() + delay;
    while Instant::now() < until {
        if running.stopping.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !running.stopping.load(Ordering::Relaxed)
}

/// Follows a tunnel until it is stopped or given up, starting `ssh` again
/// when the connection drops.
fn watch(
    app: &AppHandle,
    id: &str,
    tunnel: &Tunnel,
    running: &RunningTunnel,
    mut errors: Arc<Mutex<String>>,
) {
    let policy = ReconnectPolicy::default();
    let mut attempt = 0u32;
    let status = loop {
        let (exit, was_up) = follow(app, id, tunnel, running);
        // Give the standard-error reader a moment to finish.
        std::thread::sleep(Duration::from_millis(100));
        if running.stopping.load(Ordering::Relaxed) {
            break TunnelStatus::Stopped;
        }
        let detail = errors
            .lock()
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        let mut message = match exit {
            _ if !detail.is_empty() => detail,
            Some(status) => format!("ssh exited with {status}"),
            None => "ssh stopped".to_owned(),
        };
        if was_up {
            attempt = 0;
        } else if attempt == 0 {
            // It never came up: a wrong password or a port in use, which
            // starting it again will not fix.
            break TunnelStatus::Failed { message };
        }
        let respawned = loop {
            if attempt >= policy.max_attempts {
                break false;
            }
            attempt += 1;
            emit(
                app,
                id,
                TunnelStatus::Reconnecting {
                    message: Some(message.clone()),
                },
            );
            if !wait_unless_stopped(policy.delay(attempt), running) {
                break false;
            }
            match spawn(app, tunnel) {
                Ok((child, collected)) => {
                    if let Ok(mut slot) = running.child.lock() {
                        *slot = child;
                        // Stopped while it was starting: end the new one too.
                        if running.stopping.load(Ordering::Relaxed) {
                            let _ = slot.kill();
                        }
                    }
                    errors = collected;
                    break true;
                }
                Err(error) => message = error,
            }
        };
        if running.stopping.load(Ordering::Relaxed) {
            break TunnelStatus::Stopped;
        }
        if !respawned {
            break TunnelStatus::Failed { message };
        }
    };
    if let Ok(mut running) = app.state::<Tunnels>().running.lock() {
        running.remove(id);
    }
    emit(app, id, status);
}

/// Follows one `ssh` process until it exits, reporting when it is up.
/// Returns how it exited and whether it was up.
fn follow(
    app: &AppHandle,
    id: &str,
    tunnel: &Tunnel,
    running: &RunningTunnel,
) -> (Option<std::process::ExitStatus>, bool) {
    let started = Instant::now();
    let probe = probe_address(tunnel);
    let mut up = false;
    let exit = loop {
        let status = running.child.lock().map(|mut child| child.try_wait());
        match status {
            Ok(Ok(Some(status))) => break Some(status),
            Ok(Ok(None)) => {}
            _ => break None,
        }
        if !up {
            up = match probe {
                Some(address) => {
                    TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
                }
                None => started.elapsed() >= REMOTE_SETTLE,
            };
            if up {
                emit(app, id, TunnelStatus::running());
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    (exit, up)
}

/// The local address to probe for local and SOCKS forwards.
fn probe_address(tunnel: &Tunnel) -> Option<SocketAddr> {
    if tunnel.kind == TunnelKind::Remote {
        return None;
    }
    let host = match tunnel.bind_address.as_deref() {
        None | Some("") | Some("*") | Some("0.0.0.0") | Some("localhost") => "127.0.0.1",
        Some("::") => "::1",
        Some(host) => host,
    };
    (host, tunnel.bind_port).to_socket_addrs().ok()?.next()
}

#[tauri::command]
pub async fn tunnel_stop(app: AppHandle, id: String) -> Result<(), String> {
    let is_remote = app
        .state::<Tunnels>()
        .remote
        .lock()
        .map_err(lock_error)?
        .contains(&id);
    if is_remote {
        if let Some(link) = app.state::<DaemonState>().link() {
            return link
                .request("tunnel.stop", serde_json::json!({ "id": id }))
                .await
                .map(|_| ())
                .map_err(|error| error.message);
        }
    }
    let running = app
        .state::<Tunnels>()
        .running
        .lock()
        .map_err(lock_error)?
        .get(&id)
        .cloned();
    match running {
        Some(running) => {
            running.stopping.store(true, Ordering::Relaxed);
            let _ = running.child.lock().map_err(lock_error)?.kill();
        }
        None => emit(&app, &id, TunnelStatus::Stopped),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_loopback_for_wildcard_binds() {
        let mut tunnel = Tunnel {
            id: "t".into(),
            name: "t".into(),
            host_id: "h".into(),
            kind: TunnelKind::Dynamic,
            bind_address: Some("0.0.0.0".into()),
            bind_port: 1080,
            target_host: None,
            target_port: None,
        };
        assert_eq!(probe_address(&tunnel), Some(([127, 0, 0, 1], 1080).into()));
        tunnel.kind = TunnelKind::Remote;
        assert_eq!(probe_address(&tunnel), None);
    }

    fn row(id: &str, state: &str) -> easysshd::link::TunnelStatus {
        easysshd::link::TunnelStatus::from_payload(&serde_json::json!({
            "id": id,
            "state": state,
        }))
    }

    #[test]
    fn tunnels_the_daemon_no_longer_runs_are_stale() {
        let known = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        let rows = [
            row("a", "running"),
            row("b", "failed"),
            row("d", "reconnecting"),
        ];
        assert_eq!(stale_tunnels(&known, &rows), ["b", "c"]);
        assert_eq!(stale_tunnels(&known, &[]).len(), 3);
    }

    #[test]
    fn statuses_reach_the_ui_in_its_shape() {
        let json = |status: &TunnelStatus| serde_json::to_value(status).unwrap();
        assert_eq!(
            json(&TunnelStatus::running()),
            serde_json::json!({"state": "running"})
        );
        let warned = daemon_status(&easysshd::link::TunnelStatus::from_payload(
            &serde_json::json!({
                "id": "t",
                "state": "running",
                "channel_errors": 2,
                "last_channel_error": "127.0.0.1:1: connect failed",
            }),
        ));
        assert_eq!(
            json(&warned),
            serde_json::json!({
                "state": "running",
                "lastError": "127.0.0.1:1: connect failed",
                "errorCount": 2,
            })
        );
        let lost = daemon_status(&easysshd::link::TunnelStatus::from_payload(
            &serde_json::json!({"id": "t", "state": "reconnecting", "error": "lost"}),
        ));
        assert_eq!(
            json(&lost),
            serde_json::json!({"state": "reconnecting", "message": "lost"})
        );
        assert!(lost.is_active());
        assert!(!TunnelStatus::Stopped.is_active());
    }
}
