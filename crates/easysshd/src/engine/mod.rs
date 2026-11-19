//! The built-in SSH engine inside the daemon.
//!
//! A saved host runs on [`easyssh_ssh`] (russh) or on the system OpenSSH,
//! decided by [`choice::choose_engine`] from the global `ssh_engine` setting,
//! the host's own `engine` override and what the host needs. Hosts on russh
//! share one connection per key ([`registry::Registry`]) between their
//! terminal, the file pane and tunnels, so one password prompt covers all
//! three.

pub mod choice;
pub mod interact;
/// Host keys in the store, the connection registry, the file sessions and the
/// tunnels live in `easyssh-engine`: the in-process engine of the mobile app
/// runs the same code.
pub use easyssh_engine::{files, known_hosts, registry, tunnels};
pub mod plan;

/// The most extra connections to one host for sessions the server refused
/// on the others (`MaxSessions`).
const MAX_SHARD: u32 = 3;

use crate::askpass::Hub;
use easyssh_engine::prompts::HostKeyPrompts;
use easyssh_ssh::{
    AgentSource, ConnectParams, Error as SshError, MemoryIdentity, Session, ShellHandle,
    ShellOptions,
};
use easyssh_store::Store;
use interact::{Broadcast, DaemonInteraction, SavedPassword};
use known_hosts::{migrate_app_file, LayeredKnownHosts, StoreKnownHosts};
use plan::{HostSpec, RusshPlan};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub use easyssh_engine::{Lease, SharedError};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Where the host keys live besides the store.
#[derive(Debug, Clone)]
pub struct KnownHostsPaths {
    /// The file the engine kept host keys in before they moved into the
    /// store. Entries in it are imported once and the file is renamed. It
    /// is also where keys go when the engine has no store (tests).
    pub app: PathBuf,
    /// The user's OpenSSH file, read only. `None` to ignore it.
    pub user: Option<PathBuf>,
}

pub use easyssh_engine::error_code;

fn vault_error(error: easyssh_vault::VaultError) -> SshError {
    match error {
        easyssh_vault::VaultError::Locked => {
            SshError::Locked("the vault is locked; unlock it to use the saved key".into())
        }
        other => SshError::Protocol(format!("cannot read the saved key: {other}")),
    }
}

/// The private key (and passphrase) saved for `host_id`, ready to be handed
/// to the engine from memory. Nothing saved: `None`. Saved but the vault is
/// locked: [`SshError::Locked`], which the daemon reports as `vault_locked`.
fn saved_identity(
    secrets: &dyn easyssh_vault::SecretSource,
    host_id: &str,
    label: &str,
) -> Result<Option<MemoryIdentity>, SshError> {
    if !secrets
        .secret_flags(host_id)
        .map_err(vault_error)?
        .private_key
    {
        return Ok(None);
    }
    let saved = secrets.host_secrets(host_id).map_err(vault_error)?;
    Ok(saved.private_key.map(|key| {
        MemoryIdentity::new(
            format!("saved key of {label}"),
            key.expose(),
            saved.passphrase.map(|p| p.expose().to_owned()),
        )
    }))
}

pub struct Engine {
    pub(crate) registry: registry::Registry<Session, SharedError>,
    hub: Arc<Hub>,
    hostkeys: Arc<HostKeyPrompts>,
    paths: Mutex<KnownHostsPaths>,
    /// Host keys live here (they sync with the vault).
    store: Mutex<Option<Arc<Mutex<Store>>>>,
    secrets: Mutex<Option<Arc<dyn easyssh_vault::SecretSource>>>,
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
            hostkeys: HostKeyPrompts::with_timeout(broadcast, crate::askpass::ANSWER_TIMEOUT),
            hub,
            paths: Mutex::new(paths),
            store: Mutex::new(None),
            secrets: Mutex::new(None),
            files: files::Files::default(),
            tunnels: tunnels::Tunnels::default(),
        })
    }

    /// Where saved passwords come from (the daemon's vault).
    pub fn set_secrets(&self, secrets: Arc<dyn easyssh_vault::SecretSource>) {
        *lock(&self.secrets) = Some(secrets);
    }

    /// Keeps host keys in the app database from now on, and imports what
    /// the old app-owned file holds (once). A file that cannot be imported
    /// stays where it is and is tried again at the next start; host keys
    /// are not lost either way. Returns how many entries were imported.
    pub fn set_store(&self, store: Arc<Mutex<Store>>) -> std::io::Result<usize> {
        *lock(&self.store) = Some(store);
        self.migrate_known_hosts()
    }

    pub fn set_known_hosts(&self, paths: KnownHostsPaths) {
        *lock(&self.paths) = paths;
        let _ = self.migrate_known_hosts();
    }

    fn migrate_known_hosts(&self) -> std::io::Result<usize> {
        let Some(store) = lock(&self.store).clone() else {
            return Ok(0);
        };
        let file = lock(&self.paths).app.clone();
        let store = lock(&store);
        migrate_app_file(&store, &file)
    }

    pub fn set_linger(&self, linger: Duration) {
        self.registry.set_linger(linger);
    }

    pub fn hostkeys(&self) -> &Arc<HostKeyPrompts> {
        &self.hostkeys
    }

    fn params(&self, spec: &HostSpec, agent: Option<&PathBuf>) -> ConnectParams {
        let paths = lock(&self.paths).clone();
        let known: Arc<dyn easyssh_ssh::KnownHosts> = match lock(&self.store).clone() {
            Some(store) => Arc::new(LayeredKnownHosts::with_primary(
                Arc::new(StoreKnownHosts::new(store)),
                paths.user,
            )),
            None => Arc::new(LayeredKnownHosts::new(paths.app, paths.user)),
        };
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
        if let Some(count) = spec.keepalive_max {
            params.keepalive_max = count;
        }
        params
    }

    /// The host's saved private key. `Locked` when one is saved but the
    /// vault is locked (the caller unlocks it and retries, as for OpenSSH).
    fn saved_identity(&self, plan: &RusshPlan) -> Result<Option<MemoryIdentity>, SshError> {
        let Some(secrets) = lock(&self.secrets).clone() else {
            return Ok(None);
        };
        saved_identity(&*secrets, &plan.key.host_id, &plan.target.label)
    }

    async fn connect(
        &self,
        plan: &RusshPlan,
        agent: Option<&PathBuf>,
    ) -> Result<Session, SharedError> {
        let hops: Vec<ConnectParams> = plan.hops.iter().map(|h| self.params(h, agent)).collect();
        let mut target = self.params(&plan.target, agent);
        // A private key (and its passphrase) saved for the host goes to the
        // engine from memory; it is never written to a file.
        if let Some(identity) = self.saved_identity(plan).map_err(Arc::new)? {
            target = target.memory_identity(identity);
        }
        let saved_password = lock(&self.secrets).clone().map(|secrets| SavedPassword {
            account: format!("{}@{}", plan.target.user, plan.target.host),
            secrets,
            host_id: plan.key.host_id.clone(),
            used: std::sync::atomic::AtomicBool::new(false),
        });
        let interaction = DaemonInteraction {
            hub: self.hub.clone(),
            hostkeys: self.hostkeys.clone(),
            label: plan.target.label.clone(),
            saved_password,
            touch: Mutex::default(),
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
        self.lease_shard(plan, agent.as_ref(), 0).await
    }

    /// A lease on the host's extra connection number `shard` (0 is the
    /// usual one).
    async fn lease_shard(
        &self,
        plan: &RusshPlan,
        agent: Option<&PathBuf>,
        shard: u32,
    ) -> Result<Lease, SharedError> {
        self.registry
            .acquire(plan.key.shard(shard), || self.connect(plan, agent))
            .await
    }

    /// A shell on the host's shared connection. The lease keeps the
    /// connection up for as long as the shell lives.
    ///
    /// When the server refuses the session (OpenSSH's `MaxSessions`, 10 by
    /// default), the shell opens on another connection to the same host.
    /// When the channel does not open in time, the connection is presumed
    /// dead: it is closed and the shell opens on a new one.
    pub async fn open_shell(
        &self,
        plan: &RusshPlan,
        agent: Option<PathBuf>,
        cols: u16,
        rows: u16,
    ) -> Result<(ShellHandle, Lease), SharedError> {
        let options = ShellOptions {
            term: "xterm-256color".into(),
            cols: u32::from(cols.max(1)),
            rows: u32::from(rows.max(1)),
        };
        let mut shard = 0;
        let mut retried_timeout = false;
        loop {
            let lease = self.lease_shard(plan, agent.as_ref(), shard).await?;
            match lease.shell(options.clone()).await {
                Ok(shell) => return Ok((shell, lease)),
                Err(SshError::ChannelRefused(_)) if shard < MAX_SHARD => shard += 1,
                Err(SshError::Timeout(_)) if !retried_timeout => {
                    retried_timeout = true;
                    drop(lease);
                    self.registry.evict(&plan.key.shard(shard)).await;
                }
                Err(error) => return Err(Arc::new(error)),
            }
        }
    }

    /// `engine.connections`.
    pub async fn connections(&self) -> Value {
        self.registry.report().await
    }

    /// Stops tunnels and transfers and closes every connection.
    pub async fn shutdown(&self) {
        self.tunnels.stop_all().await;
        self.files.stop_all();
        self.registry.close_all().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_crypto::SecretText;
    use easyssh_store::{SecretFlags, StoredSecrets};
    use easyssh_vault::{SecretSource, VaultError};

    struct Saved {
        key: Option<&'static str>,
        passphrase: Option<&'static str>,
        locked: bool,
    }

    impl SecretSource for Saved {
        fn host_secrets(&self, _: &str) -> Result<StoredSecrets, VaultError> {
            if self.locked {
                return Err(VaultError::Locked);
            }
            Ok(StoredSecrets {
                private_key: self.key.map(SecretText::new),
                passphrase: self.passphrase.map(SecretText::new),
                ..Default::default()
            })
        }

        fn secret_flags(&self, _: &str) -> Result<SecretFlags, VaultError> {
            Ok(SecretFlags {
                private_key: self.key.is_some(),
                passphrase: self.passphrase.is_some(),
                password: false,
            })
        }
    }

    #[test]
    fn a_host_without_a_saved_key_offers_none() {
        let none = Saved {
            key: None,
            passphrase: None,
            locked: false,
        };
        assert!(saved_identity(&none, "h", "web").unwrap().is_none());
        // A locked vault does not matter when there is no key to read.
        let locked_without_key = Saved {
            key: None,
            passphrase: None,
            locked: true,
        };
        assert!(saved_identity(&locked_without_key, "h", "web")
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_saved_key_and_passphrase_become_an_in_memory_identity() {
        let saved = Saved {
            key: Some("-----BEGIN OPENSSH PRIVATE KEY-----\nsecret-body\n"),
            passphrase: Some("hunter2"),
            locked: false,
        };
        let identity = saved_identity(&saved, "h", "web").unwrap().unwrap();
        assert_eq!(identity.label, "saved key of web");
        // What reaches logs and error reports shows neither value.
        let shown = format!("{identity:?}");
        assert!(!shown.contains("secret-body"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
    }

    #[tokio::test]
    async fn a_security_key_file_can_be_saved_and_comes_back_as_an_sk_key() {
        use easyssh_ssh::security_key_software::SoftwareSecurityKey;
        use easyssh_ssh::{SkAlgorithm, SkKeyInfo, FLAG_USER_PRESENCE};
        let device = SoftwareSecurityKey::new();
        let file = device.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
        // The vault takes the file like any other key (it holds a handle, no
        // secret, but it is a credential and is stored encrypted the same way).
        assert!(easyssh_vault::secrets_update(None, Some(file.private_text.clone()), None).is_ok());
        let saved = Saved {
            key: Some(file.private_text.clone().leak()),
            passphrase: None,
            locked: false,
        };
        let identity = saved_identity(&saved, "h", "web").unwrap().unwrap();
        let key = easyssh_ssh::decode_identity(
            &file.private_text,
            std::path::Path::new(&identity.label),
            None,
            &easyssh_ssh::DenyAll,
        )
        .await
        .unwrap();
        assert!(SkKeyInfo::of(&key).is_some());
        // No key material in what reaches logs.
        assert!(!format!("{identity:?}").contains("BEGIN OPENSSH"));
    }

    #[test]
    fn a_locked_vault_with_a_saved_key_reports_vault_locked() {
        let saved = Saved {
            key: Some("key"),
            passphrase: None,
            locked: true,
        };
        let error = saved_identity(&saved, "h", "web").unwrap_err();
        assert!(matches!(error, SshError::Locked(_)), "{error:?}");
        assert_eq!(error_code(&error), "vault_locked");
    }
}
