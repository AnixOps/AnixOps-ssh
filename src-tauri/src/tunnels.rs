//! Port-forwarding tunnels: saved in the config, run as `ssh -N`, and
//! reported to the frontend through the `tunnel-status` event.

use crate::askpass;
use crate::daemon::{self, DaemonState};
use crate::hosts::{self, load_config, open_store};
use easyssh_core::tunnel::{bare_host, is_socket_path, spawn_tunnel};
use easyssh_core::{OpenSsh, Tunnel, TunnelKind};
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
        /// The port that listens, when known; it differs from the saved one
        /// for a tunnel saved with port 0 (any free port).
        #[serde(skip_serializing_if = "Option::is_none")]
        port: Option<u16>,
    },
    Failed {
        message: String,
    },
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
    let next = match status.state.as_str() {
        "starting" => TunnelStatus::Starting,
        "running" => TunnelStatus::Running {
            port: status.bind_port,
        },
        "stopped" => TunnelStatus::Stopped,
        _ => TunnelStatus::Failed {
            message: status
                .error
                .unwrap_or_else(|| "the tunnel stopped".to_owned()),
        },
    };
    if let Ok(mut remote) = tunnels.remote.lock() {
        match next {
            TunnelStatus::Starting | TunnelStatus::Running { .. } => {
                remote.insert(status.id.clone());
            }
            _ => {
                remote.remove(&status.id);
            }
        }
    }
    emit(app, &status.id, next);
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

/// A field without surrounding space, and an IPv6 address without the
/// brackets it may have been typed in (`[::1]`); `None` when empty.
fn trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| bare_host(&value).to_owned())
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
    let connection = hosts::connection(&tunnel.host_id)?;
    let env = askpass::env_for(&app, &connection.name, Some(&connection.id))?;
    let mut child =
        spawn_tunnel(&OpenSsh, &connection, &tunnel, &env).map_err(|error| error.to_string())?;
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
    std::thread::spawn(move || watch(&app, &id, &tunnel, &running, &errors));
    Ok(())
}

/// Follows a tunnel until its process exits, reporting when it is up.
fn watch(
    app: &AppHandle,
    id: &str,
    tunnel: &Tunnel,
    running: &RunningTunnel,
    errors: &Mutex<String>,
) {
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
                emit(
                    app,
                    id,
                    TunnelStatus::Running {
                        port: (tunnel.bind_port != 0).then_some(tunnel.bind_port),
                    },
                );
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    };
    if let Ok(mut running) = app.state::<Tunnels>().running.lock() {
        running.remove(id);
    }
    // Give the standard-error reader a moment to finish.
    std::thread::sleep(Duration::from_millis(100));
    let status = if running.stopping.load(Ordering::Relaxed) {
        TunnelStatus::Stopped
    } else {
        let detail = errors
            .lock()
            .map(|text| text.trim().to_owned())
            .unwrap_or_default();
        let message = match exit {
            _ if !detail.is_empty() => detail,
            Some(status) => format!("ssh exited with {status}"),
            None => "ssh stopped".to_owned(),
        };
        TunnelStatus::Failed { message }
    };
    emit(app, id, status);
}

/// The local address to probe for local and SOCKS forwards. `None` for a
/// remote forward and for port 0, whose port `ssh` does not tell: those
/// count as running once `ssh` stayed up for a moment.
fn probe_address(tunnel: &Tunnel) -> Option<SocketAddr> {
    if tunnel.kind == TunnelKind::Remote || tunnel.bind_port == 0 {
        return None;
    }
    if tunnel.bind_address.as_deref().is_some_and(is_socket_path) {
        return None;
    }
    let host = match tunnel.bind_address.as_deref().map(bare_host) {
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
        tunnel.kind = TunnelKind::Local;
        tunnel.bind_address = Some("[::1]".into());
        assert_eq!(
            probe_address(&tunnel),
            Some((std::net::Ipv6Addr::LOCALHOST, 1080).into())
        );
        tunnel.bind_port = 0;
        assert_eq!(probe_address(&tunnel), None);
    }

    #[test]
    fn saved_addresses_lose_their_brackets() {
        assert_eq!(trimmed(Some(" [::1] ".into())).as_deref(), Some("::1"));
        assert_eq!(trimmed(Some("db".into())).as_deref(), Some("db"));
        assert_eq!(trimmed(Some("  ".into())), None);
        assert_eq!(trimmed(None), None);
    }

    #[test]
    fn a_running_status_names_its_port() {
        assert_eq!(
            serde_json::to_value(TunnelStatus::Running { port: Some(40123) }).unwrap(),
            serde_json::json!({"state": "running", "port": 40123})
        );
        assert_eq!(
            serde_json::to_value(TunnelStatus::Running { port: None }).unwrap(),
            serde_json::json!({"state": "running"})
        );
    }
}
