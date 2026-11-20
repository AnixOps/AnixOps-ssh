//! Tunnels on the built-in engine: `tunnel.start|stop|list` and the
//! `tunnel.status` event. A tunnel is a forward on the host's shared
//! connection, so it costs no extra login.

use crate::events::Events;
use crate::Lease;
use easyssh_core::tunnel::{bare_host, is_socket_path};
use easyssh_core::{Tunnel, TunnelKind};
use easyssh_ssh::{Dest, Error as SshError, Forward};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::task::JoinHandle;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[derive(Clone)]
struct Status {
    host_id: String,
    state: &'static str,
    bind_port: Option<u16>,
    error: Option<String>,
}

struct Running {
    forward: Option<Forward>,
    _lease: Lease,
    watcher: JoinHandle<()>,
}

#[derive(Default)]
pub struct Tunnels {
    running: tokio::sync::Mutex<HashMap<String, Running>>,
    statuses: Arc<Mutex<HashMap<String, Status>>>,
}

fn announce(
    statuses: &Mutex<HashMap<String, Status>>,
    broadcast: &Events,
    id: &str,
    status: Status,
) {
    broadcast.emit(
        "tunnel.status",
        json!({
            "id": id,
            "state": status.state,
            "bind_port": status.bind_port,
            "error": status.error,
        }),
    );
    lock(statuses).insert(id.to_owned(), status);
}

/// The address to listen on: OpenSSH's default is the loopback interface.
/// `localhost` and `*` are passed on as they are: the forward listens on
/// both IPv4 and IPv6 for them.
fn bind_host(tunnel: &Tunnel) -> &str {
    match tunnel.bind_address.as_deref().map(bare_host) {
        None | Some("") | Some("localhost") => "localhost",
        Some(other) => other,
    }
}

/// A running tunnel's status with the port it listens on, which the UI
/// shows for a tunnel saved with port 0 (any free port).
fn with_port(mut status: Value, port: Option<u64>) -> Value {
    if let (Some("running"), Some(port)) = (status["state"].as_str(), port) {
        status["port"] = json!(port);
    }
    status
}

/// A tunnel's state as the UI's `TunnelStatus`: `{"state": "running"}`, or
/// `{"state": "failed", "message": "..."}`.
pub fn ui_status(state: &str, error: Option<&str>) -> Value {
    match state {
        "starting" | "running" | "stopped" => json!({ "state": state }),
        _ => json!({
            "state": "failed",
            "message": error.unwrap_or("the tunnel stopped"),
        }),
    }
}

/// The `tunnel.status` event as the UI's `tunnel-status` payload.
pub fn ui_status_event(payload: &Value) -> Value {
    json!({
        "id": payload["id"],
        "status": with_port(
            ui_status(
                payload["state"].as_str().unwrap_or("failed"),
                payload["error"].as_str(),
            ),
            payload["bind_port"].as_u64(),
        ),
    })
}

impl Tunnels {
    /// The last status of one tunnel, as the UI shows it; `stopped` when it
    /// never ran.
    pub fn ui_status_of(&self, id: &str) -> Value {
        match lock(&self.statuses).get(id) {
            Some(status) => with_port(
                ui_status(status.state, status.error.as_deref()),
                status.bind_port.map(u64::from),
            ),
            None => ui_status("stopped", None),
        }
    }

    pub fn list(&self) -> Value {
        let statuses = lock(&self.statuses);
        let mut tunnels: Vec<Value> = statuses
            .iter()
            .map(|(id, s)| {
                json!({
                    "id": id,
                    "host_id": s.host_id,
                    "state": s.state,
                    "bind_port": s.bind_port,
                    "error": s.error,
                })
            })
            .collect();
        tunnels.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        json!({"tunnels": tunnels})
    }

    /// Marks a tunnel as starting (before the connection is made, which may
    /// involve prompts). `false` when it already runs.
    pub async fn begin(&self, tunnel: &Tunnel, broadcast: &Events) -> bool {
        let mut running = self.running.lock().await;
        if running.contains_key(&tunnel.id) {
            let failed = lock(&self.statuses)
                .get(&tunnel.id)
                .is_some_and(|s| s.state == "failed");
            if !failed {
                return false;
            }
            // Its connection died: drop what is left of it and start afresh.
            if let Some(old) = running.remove(&tunnel.id) {
                old.watcher.abort();
            }
        }
        announce(
            &self.statuses,
            broadcast,
            &tunnel.id,
            Status {
                host_id: tunnel.host_id.clone(),
                state: "starting",
                bind_port: None,
                error: None,
            },
        );
        true
    }

    /// Records a failure before the forward was up (no connection, port busy).
    pub fn failed(&self, tunnel: &Tunnel, error: &SshError, broadcast: &Events) {
        self.failed_with(tunnel, &error.to_string(), broadcast);
    }

    /// [`Tunnels::failed`] with the message already made.
    pub fn failed_with(&self, tunnel: &Tunnel, message: &str, broadcast: &Events) {
        announce(
            &self.statuses,
            broadcast,
            &tunnel.id,
            Status {
                host_id: tunnel.host_id.clone(),
                state: "failed",
                bind_port: None,
                error: Some(message.to_owned()),
            },
        );
    }

    /// Opens the forward on a leased connection and watches the connection.
    /// Returns the port that is listening (local and dynamic forwards) or
    /// that the server listens on (remote).
    pub async fn start(
        &self,
        tunnel: &Tunnel,
        lease: Lease,
        broadcast: Events,
    ) -> Result<u16, SshError> {
        let socket_of = |value: Option<&str>| {
            value
                .map(str::trim)
                .filter(|v| is_socket_path(v))
                .map(str::to_owned)
        };
        let bind_socket = socket_of(tunnel.bind_address.as_deref());
        let target_socket = socket_of(tunnel.target_host.as_deref());
        // The target as a host and port, unless it is a socket.
        let target = || -> Result<(&str, u16), SshError> {
            let host = tunnel
                .target_host
                .as_deref()
                .map(bare_host)
                .filter(|h| !h.is_empty())
                .ok_or_else(|| SshError::Forward("the target host is required".into()))?;
            if target_socket.is_some() {
                return Ok((host, 0));
            }
            let port = tunnel
                .target_port
                .filter(|p| *p > 0)
                .ok_or_else(|| SshError::Forward("the target port is required".into()))?;
            Ok((host, port))
        };
        let forward = match tunnel.kind {
            TunnelKind::Local => {
                let (host, port) = target()?;
                let dest = match &target_socket {
                    Some(path) => Dest::Socket(path.clone()),
                    None => Dest::Tcp {
                        host: host.to_owned(),
                        port,
                    },
                };
                lease
                    .forward_local_to(
                        bind_socket.as_deref(),
                        bind_host(tunnel),
                        tunnel.bind_port,
                        dest,
                    )
                    .await
            }
            TunnelKind::Remote => {
                let (host, port) = target()?;
                let bind = tunnel
                    .bind_address
                    .as_deref()
                    .map(bare_host)
                    .filter(|b| !b.is_empty())
                    .unwrap_or("localhost");
                lease
                    .forward_remote_to(
                        bind_socket.as_deref(),
                        bind,
                        tunnel.bind_port,
                        host,
                        port,
                        target_socket.as_deref(),
                    )
                    .await
            }
            TunnelKind::Dynamic => {
                if bind_socket.is_some() {
                    return Err(SshError::Forward(
                        "a SOCKS proxy cannot listen on a Unix socket".into(),
                    ));
                }
                lease
                    .forward_dynamic(bind_host(tunnel), tunnel.bind_port)
                    .await
            }
        }?;
        let port = forward
            .local_addr()
            .map(|addr| addr.port())
            .or_else(|| forward.remote_port().and_then(|p| u16::try_from(p).ok()))
            .unwrap_or(tunnel.bind_port);

        let id = tunnel.id.clone();
        let host_id = tunnel.host_id.clone();
        let session = lease.session().clone();
        let statuses = self.statuses.clone();
        let watching = broadcast.clone();
        let watcher = tokio::spawn(async move {
            // A tunnel lives and dies with its connection.
            while !session.is_closed() {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            announce(
                &statuses,
                &watching,
                &id,
                Status {
                    host_id,
                    state: "failed",
                    bind_port: None,
                    error: Some("the connection to the server was lost".into()),
                },
            );
        });
        self.running.lock().await.insert(
            tunnel.id.clone(),
            Running {
                forward: Some(forward),
                _lease: lease,
                watcher,
            },
        );
        announce(
            &self.statuses,
            &broadcast,
            &tunnel.id,
            Status {
                host_id: tunnel.host_id.clone(),
                state: "running",
                bind_port: Some(port),
                error: None,
            },
        );
        Ok(port)
    }

    /// `tunnel.stop`. Unknown tunnels are ignored.
    pub async fn stop(&self, id: &str, broadcast: &Events) {
        let running = self.running.lock().await.remove(id);
        let Some(mut running) = running else {
            // Never started, or it failed: just clear the failure.
            let host_id = lock(&self.statuses).get(id).map(|s| s.host_id.clone());
            if let Some(host_id) = host_id {
                announce(
                    &self.statuses,
                    broadcast,
                    id,
                    Status {
                        host_id,
                        state: "stopped",
                        bind_port: None,
                        error: None,
                    },
                );
            }
            return;
        };
        running.watcher.abort();
        if let Some(forward) = running.forward.take() {
            let _ = forward.stop().await;
        }
        let host_id = lock(&self.statuses)
            .get(id)
            .map(|s| s.host_id.clone())
            .unwrap_or_default();
        announce(
            &self.statuses,
            broadcast,
            id,
            Status {
                host_id,
                state: "stopped",
                bind_port: None,
                error: None,
            },
        );
    }

    pub async fn stop_all(&self) {
        let all: Vec<Running> = self.running.lock().await.drain().map(|(_, r)| r).collect();
        for mut running in all {
            running.watcher.abort();
            if let Some(forward) = running.forward.take() {
                let _ = forward.stop().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tunnel(bind: Option<&str>) -> Tunnel {
        Tunnel {
            id: "t".into(),
            name: "t".into(),
            host_id: "h".into(),
            kind: TunnelKind::Local,
            bind_address: bind.map(str::to_owned),
            bind_port: 8080,
            target_host: Some("x".into()),
            target_port: Some(80),
        }
    }

    #[test]
    fn the_default_bind_is_loopback() {
        assert_eq!(bind_host(&tunnel(None)), "localhost");
        assert_eq!(bind_host(&tunnel(Some(""))), "localhost");
        assert_eq!(bind_host(&tunnel(Some("localhost"))), "localhost");
        assert_eq!(bind_host(&tunnel(Some("*"))), "*");
        assert_eq!(bind_host(&tunnel(Some("10.0.0.2"))), "10.0.0.2");
        assert_eq!(bind_host(&tunnel(Some("[::1]"))), "::1");
    }

    #[test]
    fn states_become_the_uis_tunnel_status() {
        assert_eq!(ui_status("running", None), json!({"state": "running"}));
        assert_eq!(ui_status("stopped", None), json!({"state": "stopped"}));
        assert_eq!(
            ui_status("failed", Some("port busy")),
            json!({"state": "failed", "message": "port busy"})
        );
        assert_eq!(
            ui_status_event(&json!({"id": "t", "state": "failed", "error": "lost"})),
            json!({"id": "t", "status": {"state": "failed", "message": "lost"}})
        );
        assert_eq!(
            ui_status_event(&json!({"id": "t", "state": "running", "bind_port": 40123})),
            json!({"id": "t", "status": {"state": "running", "port": 40123}})
        );
        assert_eq!(
            Tunnels::default().ui_status_of("never-ran"),
            json!({"state": "stopped"})
        );
    }

    #[tokio::test]
    async fn a_tunnel_starts_once_and_reports_failures() {
        let tunnels = Tunnels::default();
        let events = Arc::new(Mutex::new(Vec::new()));
        let seen = events.clone();
        let broadcast: Events = Arc::new(move |kind: &str, payload: Value| {
            lock(&seen).push((kind.to_owned(), payload));
            1
        });
        let t = tunnel(None);
        assert!(tunnels.begin(&t, &broadcast).await);
        tunnels.failed(&t, &SshError::Forward("busy".into()), &broadcast);
        let listed = tunnels.list();
        assert_eq!(listed["tunnels"][0]["state"], "failed");
        let events = lock(&events);
        assert_eq!(events[0].1["state"], "starting");
        assert_eq!(events[1].1["state"], "failed");
        assert!(events[1].1["error"].as_str().unwrap().contains("busy"));
        easyssh_proto::validate("TunnelStatusEvent", &events[1].1).unwrap();
    }
}
