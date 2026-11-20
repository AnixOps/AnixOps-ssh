//! Tunnels on the built-in engine: `tunnel.start|stop|list` and the
//! `tunnel.status` event. A tunnel is a forward on the host's shared
//! connection, so it costs no extra login.
//!
//! When the connection drops, the forward is stopped (its port is freed, so
//! nothing accepts connections that go nowhere), the tunnel reports
//! `reconnecting`, and it leases a new connection with growing delays
//! ([`ReconnectPolicy`]) and opens the forward again. It gives up with
//! `failed` after the policy's attempts, or at once when retrying cannot help
//! (wrong password, rejected host key).
//!
//! Connections the forward accepts but cannot pass on (the server refused
//! the channel, the target is down) do not stop it: they are counted and the
//! last reason travels in the status (`channel_errors`, `last_channel_error`).

use crate::events::Events;
use crate::supervisor::ReconnectPolicy;
use crate::Lease;
use easyssh_core::tunnel::bare_host;
use easyssh_core::{Tunnel, TunnelKind};
use easyssh_ssh::{Error as SshError, Forward, ForwardFailures};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::watch;
use tokio::task::JoinHandle;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Why a new lease could not be had; `fatal` when retrying cannot help.
#[derive(Debug, Clone)]
pub struct RelinkError {
    pub message: String,
    pub fatal: bool,
}

/// Leases the tunnel's host again after its connection dropped.
pub type Relink =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Lease, RelinkError>> + Send>> + Send + Sync>;

#[derive(Clone)]
struct Status {
    host_id: String,
    state: &'static str,
    bind_port: Option<u16>,
    error: Option<String>,
    channel_errors: u64,
    last_channel_error: Option<String>,
}

impl Status {
    fn new(host_id: &str, state: &'static str) -> Status {
        Status {
            host_id: host_id.to_owned(),
            state,
            bind_port: None,
            error: None,
            channel_errors: 0,
            last_channel_error: None,
        }
    }

    fn error(mut self, error: impl Into<String>) -> Status {
        self.error = Some(error.into());
        self
    }

    fn port(mut self, port: u16) -> Status {
        self.bind_port = Some(port);
        self
    }

    fn json(&self, id: &str) -> Value {
        let mut value = json!({
            "id": id,
            "state": self.state,
            "bind_port": self.bind_port,
            "error": self.error,
        });
        if self.channel_errors > 0 {
            value["channel_errors"] = json!(self.channel_errors);
            value["last_channel_error"] = json!(self.last_channel_error);
        }
        value
    }
}

struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

#[derive(Default)]
pub struct Tunnels {
    running: tokio::sync::Mutex<HashMap<String, Running>>,
    statuses: Arc<Mutex<HashMap<String, Status>>>,
    /// `None`: [`ReconnectPolicy::default`].
    policy: Mutex<Option<ReconnectPolicy>>,
}

fn announce(
    statuses: &Mutex<HashMap<String, Status>>,
    broadcast: &Events,
    id: &str,
    status: Status,
) {
    broadcast.emit("tunnel.status", status.json(id));
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

/// A `tunnel.status` payload (or a `tunnel.list` row) as the UI's
/// `TunnelStatus`: `{"state": "running"}`, `{"state": "reconnecting",
/// "message": ...}`, or `{"state": "failed", "message": "..."}`. A running
/// tunnel that could not pass connections on also carries `lastError` and
/// `errorCount`.
pub fn ui_status(payload: &Value) -> Value {
    let state = payload["state"].as_str().unwrap_or("failed");
    let error = payload["error"].as_str();
    match state {
        "running" => {
            let mut value = json!({ "state": "running" });
            // The port that listens: the UI shows it for a tunnel saved with
            // port 0 (any free port).
            if let Some(port) = payload["bind_port"].as_u64() {
                value["port"] = json!(port);
            }
            if let Some(count) = payload["channel_errors"].as_u64().filter(|n| *n > 0) {
                value["errorCount"] = json!(count);
                value["lastError"] = payload["last_channel_error"].clone();
            }
            value
        }
        "starting" | "stopped" => json!({ "state": state }),
        "reconnecting" => match error {
            Some(message) => json!({ "state": "reconnecting", "message": message }),
            None => json!({ "state": "reconnecting" }),
        },
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
        "status": ui_status(payload),
    })
}

/// Opens the forward of `tunnel` on `session`; `bind_port` replaces the
/// saved port (a reopened forward keeps the port it had).
async fn open_forward(
    tunnel: &Tunnel,
    session: &easyssh_ssh::Session,
    bind_port: u16,
) -> Result<(Forward, u16), SshError> {
    let target = || -> Result<(&str, u16), SshError> {
        let host = tunnel
            .target_host
            .as_deref()
            .map(bare_host)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| SshError::Forward("the target host is required".into()))?;
        let port = tunnel
            .target_port
            .filter(|p| *p > 0)
            .ok_or_else(|| SshError::Forward("the target port is required".into()))?;
        Ok((host, port))
    };
    let forward = match tunnel.kind {
        TunnelKind::Local => {
            let (host, port) = target()?;
            session
                .forward_local(bind_host(tunnel), bind_port, host, port)
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
            session.forward_remote(bind, bind_port, host, port).await
        }
        TunnelKind::Dynamic => session.forward_dynamic(bind_host(tunnel), bind_port).await,
    }?;
    let port = forward
        .local_addr()
        .map(|addr| addr.port())
        .or_else(|| forward.remote_port().and_then(|p| u16::try_from(p).ok()))
        .unwrap_or(bind_port);
    Ok((forward, port))
}

/// What the task that keeps one tunnel up needs.
struct Keeper {
    tunnel: Tunnel,
    statuses: Arc<Mutex<HashMap<String, Status>>>,
    broadcast: Events,
    relink: Relink,
    policy: ReconnectPolicy,
    stop: watch::Receiver<bool>,
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

impl Keeper {
    fn announce(&self, status: Status) {
        announce(&self.statuses, &self.broadcast, &self.tunnel.id, status);
    }

    fn status(&self, state: &'static str) -> Status {
        Status::new(&self.tunnel.host_id, state)
    }

    /// Follows the forward while its connection lives; reopens it on a new
    /// connection when the old one drops. Ends when stopped or given up.
    async fn run(mut self, mut forward: Forward, mut lease: Lease, mut port: u16) {
        loop {
            let session = lease.session().clone();
            let mut failures = forward.failures();
            let mut running = self.status("running").port(port);
            loop {
                tokio::select! {
                    _ = stopped(&mut self.stop) => {
                        let _ = forward.stop().await;
                        return;
                    }
                    changed = failures.changed() => {
                        if changed.is_err() {
                            continue;
                        }
                        let ForwardFailures { count, last } = failures.borrow_and_update().clone();
                        running.channel_errors = count;
                        running.last_channel_error = last;
                        self.announce(running.clone());
                    }
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        if session.is_closed() {
                            break;
                        }
                    }
                }
            }
            // Free the port first: a listener whose connections go nowhere
            // is worse than none.
            let _ = forward.stop().await;
            drop(lease);
            match self.reopen(port).await {
                Some((next_forward, next_lease, next_port)) => {
                    forward = next_forward;
                    lease = next_lease;
                    port = next_port;
                }
                None => return,
            }
        }
    }

    /// Leases a new connection and opens the forward on it, with backoff.
    /// `None` when stopped or given up (the status says which).
    async fn reopen(&mut self, port: u16) -> Option<(Forward, Lease, u16)> {
        let mut reason = "the connection to the server was lost".to_owned();
        // A remote forward on port 0 gets whatever port the server picks.
        let bind_port = match self.tunnel.kind {
            TunnelKind::Remote => self.tunnel.bind_port,
            _ => port,
        };
        for attempt in 1..=self.policy.max_attempts {
            self.announce(self.status("reconnecting").error(reason.clone()));
            tokio::select! {
                _ = stopped(&mut self.stop) => return None,
                _ = tokio::time::sleep(self.policy.delay(attempt)) => {}
            }
            let relink = (self.relink)();
            let lease = tokio::select! {
                _ = stopped(&mut self.stop) => return None,
                lease = relink => lease,
            };
            match lease {
                Ok(lease) => match open_forward(&self.tunnel, lease.session(), bind_port).await {
                    Ok((forward, port)) => {
                        self.announce(self.status("running").port(port));
                        return Some((forward, lease, port));
                    }
                    Err(error) => reason = error.to_string(),
                },
                Err(error) if error.fatal => {
                    self.announce(self.status("failed").error(error.message));
                    return None;
                }
                Err(error) => reason = error.message,
            }
            tracing::debug!(
                "tunnel {}: reconnect attempt {attempt} failed: {reason}",
                self.tunnel.id
            );
        }
        self.announce(self.status("failed").error(reason));
        None
    }
}

impl Tunnels {
    /// Overrides how hard a tunnel tries to get its connection back.
    pub fn set_policy(&self, policy: ReconnectPolicy) {
        *lock(&self.policy) = Some(policy);
    }

    /// The last status of one tunnel, as the UI shows it; `stopped` when it
    /// never ran.
    pub fn ui_status_of(&self, id: &str) -> Value {
        match lock(&self.statuses).get(id) {
            Some(status) => ui_status(&status.json(id)),
            None => json!({ "state": "stopped" }),
        }
    }

    pub fn list(&self) -> Value {
        let statuses = lock(&self.statuses);
        let mut tunnels: Vec<Value> = statuses
            .iter()
            .map(|(id, s)| {
                let mut row = s.json(id);
                row["host_id"] = json!(s.host_id);
                row
            })
            .collect();
        tunnels.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
        json!({"tunnels": tunnels})
    }

    /// Marks a tunnel as starting (before the connection is made, which may
    /// involve prompts). `false` when it already runs.
    pub async fn begin(&self, tunnel: &Tunnel, broadcast: &Events) -> bool {
        let mut running = self.running.lock().await;
        if let Some(old) = running.get(&tunnel.id) {
            if !old.task.is_finished() {
                return false;
            }
            // It gave up (its connection died for good): start afresh.
            running.remove(&tunnel.id);
        }
        announce(
            &self.statuses,
            broadcast,
            &tunnel.id,
            Status::new(&tunnel.host_id, "starting"),
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
            Status::new(&tunnel.host_id, "failed").error(message),
        );
    }

    /// Opens the forward on a leased connection and keeps it up: when the
    /// connection drops, `relink` leases a new one. Returns the port that is
    /// listening (local and dynamic forwards) or that the server listens on
    /// (remote).
    pub async fn start(
        &self,
        tunnel: &Tunnel,
        lease: Lease,
        relink: Relink,
        broadcast: Events,
    ) -> Result<u16, SshError> {
        let (forward, port) = open_forward(tunnel, lease.session(), tunnel.bind_port).await?;
        let (stop, stop_rx) = watch::channel(false);
        let keeper = Keeper {
            tunnel: tunnel.clone(),
            statuses: self.statuses.clone(),
            broadcast: broadcast.clone(),
            relink,
            policy: lock(&self.policy).unwrap_or_default(),
            stop: stop_rx,
        };
        // Announced before the keeper runs, so a failure it reports comes after.
        announce(
            &self.statuses,
            &broadcast,
            &tunnel.id,
            Status::new(&tunnel.host_id, "running").port(port),
        );
        let task = tokio::spawn(keeper.run(forward, lease, port));
        self.running
            .lock()
            .await
            .insert(tunnel.id.clone(), Running { stop, task });
        Ok(port)
    }

    /// `tunnel.stop`. Unknown tunnels are ignored.
    pub async fn stop(&self, id: &str, broadcast: &Events) {
        let running = self.running.lock().await.remove(id);
        if let Some(running) = running {
            finish(running).await;
        }
        let host_id = lock(&self.statuses).get(id).map(|s| s.host_id.clone());
        if let Some(host_id) = host_id {
            announce(
                &self.statuses,
                broadcast,
                id,
                Status::new(&host_id, "stopped"),
            );
        }
    }

    pub async fn stop_all(&self) {
        let all: Vec<Running> = self.running.lock().await.drain().map(|(_, r)| r).collect();
        for running in all {
            finish(running).await;
        }
    }
}

/// Tells a tunnel's keeper to stop and waits until its forward is gone.
async fn finish(running: Running) {
    let _ = running.stop.send(true);
    let mut task = running.task;
    if tokio::time::timeout(Duration::from_secs(5), &mut task)
        .await
        .is_err()
    {
        task.abort();
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
        assert_eq!(
            ui_status(&json!({"state": "running"})),
            json!({"state": "running"})
        );
        assert_eq!(
            ui_status(&json!({"state": "stopped"})),
            json!({"state": "stopped"})
        );
        assert_eq!(
            ui_status(&json!({"state": "failed", "error": "port busy"})),
            json!({"state": "failed", "message": "port busy"})
        );
        assert_eq!(
            ui_status(&json!({"state": "reconnecting", "error": "lost"})),
            json!({"state": "reconnecting", "message": "lost"})
        );
        assert_eq!(
            ui_status(&json!({
                "state": "running",
                "channel_errors": 2,
                "last_channel_error": "127.0.0.1:1: connect failed",
            })),
            json!({
                "state": "running",
                "errorCount": 2,
                "lastError": "127.0.0.1:1: connect failed",
            })
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

    #[test]
    fn statuses_match_the_protocol() {
        let mut status = Status::new("h", "running").port(8080);
        status.channel_errors = 3;
        status.last_channel_error = Some("db:5432: connect failed".into());
        easyssh_proto::validate("TunnelStatusEvent", &status.json("t")).unwrap();
        let reconnecting = Status::new("h", "reconnecting").error("the connection was lost");
        easyssh_proto::validate("TunnelStatusEvent", &reconnecting.json("t")).unwrap();
        let tunnels = Tunnels::default();
        lock(&tunnels.statuses).insert("t".into(), status);
        easyssh_proto::validate("TunnelListResult", &tunnels.list()).unwrap();
        assert_eq!(
            tunnels.ui_status_of("t"),
            json!({"state": "running", "port": 8080, "errorCount": 3, "lastError": "db:5432: connect failed"})
        );
    }
}
