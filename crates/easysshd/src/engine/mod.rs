//! The built-in SSH engine inside the daemon.
//!
//! A saved host runs on [`easyssh_ssh`] (russh) or on the system OpenSSH,
//! decided by [`choice::choose_engine`] from the global `ssh_engine` setting,
//! the host's own `engine` override and what the host needs. Hosts on russh
//! share one connection per key ([`registry::Registry`]) between their
//! terminal, the file pane and tunnels, so one password prompt covers all
//! three.

pub mod choice;
pub mod files;
pub mod interact;
pub mod known_hosts;
pub mod plan;
pub mod registry;
pub mod tunnels;

use crate::askpass::Hub;
use easyssh_ssh::{
    AgentSource, ConnectParams, Error as SshError, Session, ShellHandle, ShellOptions,
};
use interact::{Broadcast, DaemonInteraction, HostKeyPrompts};
use known_hosts::LayeredKnownHosts;
use plan::{HostSpec, RusshPlan};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// A connect failure that several waiting callers can share.
pub type SharedError = Arc<SshError>;
pub type Lease = registry::Lease<Session, SharedError>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Where the host keys live.
#[derive(Debug, Clone)]
pub struct KnownHostsPaths {
    /// The app-owned file new keys are written to.
    pub app: PathBuf,
    /// The user's OpenSSH file, read only. `None` to ignore it.
    pub user: Option<PathBuf>,
}

/// The protocol error code for a connect failure, so callers can tell a
/// fallback (`unsupported`) from the user saying no (`cancelled`).
pub fn error_code(error: &SshError) -> &'static str {
    match error {
        SshError::Unsupported(_) => "unsupported",
        SshError::Cancelled(_) => "cancelled",
        SshError::AuthFailed { .. } => "auth_failed",
        SshError::HostKeyRejected { .. } => "hostkey_rejected",
        SshError::HostKeyMismatch { .. } => "hostkey_changed",
        SshError::HostKeyRevoked { .. } => "hostkey_revoked",
        SshError::Network(_) | SshError::Timeout(_) | SshError::Disconnected => "network",
        SshError::NotFound(_) => "not_found",
        _ => "ssh_error",
    }
}

pub struct Engine {
    pub(crate) registry: registry::Registry<Session, SharedError>,
    hub: Arc<Hub>,
    hostkeys: Arc<HostKeyPrompts>,
    broadcast: Broadcast,
    paths: Mutex<KnownHostsPaths>,
    pub(crate) files: files::Files,
    pub(crate) tunnels: tunnels::Tunnels,
}

impl Engine {
    pub fn new(
        hub: Arc<Hub>,
        broadcast: Broadcast,
        paths: KnownHostsPaths,
        linger: Duration,
    ) -> Arc<Engine> {
        Arc::new(Engine {
            registry: registry::Registry::new(linger),
            hostkeys: HostKeyPrompts::new(broadcast.clone()),
            hub,
            broadcast,
            paths: Mutex::new(paths),
            files: files::Files::default(),
            tunnels: tunnels::Tunnels::default(),
        })
    }

    pub fn set_known_hosts(&self, paths: KnownHostsPaths) {
        *lock(&self.paths) = paths;
    }

    pub fn set_linger(&self, linger: Duration) {
        self.registry.set_linger(linger);
    }

    pub fn hostkeys(&self) -> &Arc<HostKeyPrompts> {
        &self.hostkeys
    }

    pub(crate) fn broadcast(&self, kind: &str, payload: Value) -> usize {
        (self.broadcast)(kind, payload)
    }

    fn params(&self, spec: &HostSpec, agent: Option<&PathBuf>) -> ConnectParams {
        let paths = lock(&self.paths).clone();
        let known: Arc<dyn easyssh_ssh::KnownHosts> =
            Arc::new(LayeredKnownHosts::new(paths.app, paths.user));
        let mut params =
            ConnectParams::new(spec.host.clone(), spec.port, spec.user.clone()).known_hosts(known);
        params.agent = match (spec.agent, agent) {
            (false, _) => AgentSource::Disabled,
            (true, Some(path)) => AgentSource::Path(path.clone()),
            (true, None) => AgentSource::Environment,
        };
        params.identity_files = spec.identity_files.clone();
        params.keyboard_interactive = spec.keyboard_interactive;
        params.password = spec.password;
        params.password_attempts = spec.password_attempts;
        if let Some(timeout) = spec.connect_timeout {
            params.connect_timeout = timeout;
        }
        if spec.keepalive.is_some() {
            params.keepalive = spec.keepalive;
        }
        params
    }

    async fn connect(
        &self,
        plan: &RusshPlan,
        agent: Option<&PathBuf>,
    ) -> Result<Session, SharedError> {
        let hops: Vec<ConnectParams> = plan.hops.iter().map(|h| self.params(h, agent)).collect();
        let target = self.params(&plan.target, agent);
        let interaction = DaemonInteraction {
            hub: self.hub.clone(),
            hostkeys: self.hostkeys.clone(),
            label: plan.target.label.clone(),
        };
        Session::connect_via(&hops, target, &interaction)
            .await
            .map_err(Arc::new)
    }

    /// A lease on the shared connection for `plan`, connecting (and asking
    /// the user whatever it takes) when there is none.
    pub async fn lease(
        &self,
        plan: &RusshPlan,
        agent: Option<PathBuf>,
    ) -> Result<Lease, SharedError> {
        self.registry
            .acquire(plan.key.clone(), || self.connect(plan, agent.as_ref()))
            .await
    }

    /// A shell on the host's shared connection. The lease keeps the
    /// connection up for as long as the shell lives.
    pub async fn open_shell(
        &self,
        plan: &RusshPlan,
        agent: Option<PathBuf>,
        cols: u16,
        rows: u16,
    ) -> Result<(ShellHandle, Lease), SharedError> {
        let lease = self.lease(plan, agent).await?;
        let shell = lease
            .shell(ShellOptions {
                term: "xterm-256color".into(),
                cols: u32::from(cols.max(1)),
                rows: u32::from(rows.max(1)),
            })
            .await
            .map_err(Arc::new)?;
        Ok((shell, lease))
    }

    /// `engine.connections`.
    pub async fn connections(&self) -> Value {
        let rows = self.registry.connections().await;
        json!({
            "opened": self.registry.opened(),
            "live": rows.len(),
            "connections": rows.iter().map(|row| json!({
                "host_id": row.key.host_id,
                "target": row.key.target,
                "jump": row.key.jump,
                "leases": row.leases,
            })).collect::<Vec<_>>(),
        })
    }

    /// Stops tunnels and transfers and closes every connection.
    pub async fn shutdown(&self) {
        self.tunnels.stop_all().await;
        self.files.stop_all();
        self.registry.close_all().await;
    }
}
