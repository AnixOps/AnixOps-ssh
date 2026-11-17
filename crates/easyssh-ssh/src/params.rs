//! Connection parameters.

use crate::known_hosts::{KnownHosts, MemoryKnownHosts};
use crate::security_key::SecurityKey;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use zeroize::Zeroizing;

/// Where the SSH agent lives.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum AgentSource {
    /// Do not use an agent.
    Disabled,
    /// `SSH_AUTH_SOCK` on Unix; on Windows `SSH_AUTH_SOCK` if set, else the
    /// OpenSSH named pipe `\\.\pipe\openssh-ssh-agent`.
    #[default]
    Environment,
    /// A Unix socket path, or a Windows named pipe name (for example the
    /// 1Password agent).
    Path(PathBuf),
}

/// A private key the caller holds in memory (for example one saved in the
/// vault). The engine decodes it for the login and never writes it to disk.
/// The text and the passphrase are zeroized when the last copy is dropped.
#[derive(Clone)]
pub struct MemoryIdentity {
    /// Names the key in prompts and error messages; never a real path.
    pub label: String,
    key: Arc<Zeroizing<String>>,
    passphrase: Option<Arc<Zeroizing<String>>>,
}

impl MemoryIdentity {
    /// `pem` is the key in OpenSSH or PEM text form. `passphrase` unlocks it
    /// when it is encrypted; without one an encrypted key asks the
    /// [`crate::Interaction`].
    pub fn new(
        label: impl Into<String>,
        pem: impl Into<String>,
        passphrase: Option<String>,
    ) -> Self {
        Self {
            label: label.into(),
            key: Arc::new(Zeroizing::new(pem.into())),
            passphrase: passphrase
                .filter(|p| !p.is_empty())
                .map(|p| Arc::new(Zeroizing::new(p))),
        }
    }

    pub(crate) fn text(&self) -> &str {
        self.key.as_str()
    }

    pub(crate) fn passphrase(&self) -> Option<&str> {
        self.passphrase.as_ref().map(|p| p.as_str())
    }
}

impl std::fmt::Debug for MemoryIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryIdentity")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct ConnectParams {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// Interval of SSH keepalive messages; `None` disables them. After three
    /// unanswered keepalives the connection is closed.
    pub keepalive: Option<Duration>,
    /// Limit for the TCP connection (not for prompts to the user).
    pub connect_timeout: Duration,
    pub agent: AgentSource,
    /// Private key files in OpenSSH (or PEM) format, tried in order after the
    /// agent. Never defaulted from `~/.ssh`: the caller decides.
    pub identity_files: Vec<PathBuf>,
    /// Keys held in memory, tried before `identity_files`. Never written to
    /// disk by the engine.
    pub memory_identities: Vec<MemoryIdentity>,
    pub keyboard_interactive: bool,
    pub password: bool,
    /// How often the password prompt is shown when the server refuses it.
    pub password_attempts: u32,
    pub known_hosts: Arc<dyn KnownHosts>,
    /// The authenticator for `sk-*` keys. `None` uses the build's own
    /// ([`crate::default_security_key`]).
    pub security_key: Option<Arc<dyn SecurityKey>>,
}

impl ConnectParams {
    /// Defaults: keepalive 60 s, agent from the environment, no identity
    /// files, keyboard-interactive and password enabled, an in-memory known
    /// hosts store (every host is new in each process; supply a file store
    /// with [`ConnectParams::known_hosts`]).
    pub fn new(host: impl Into<String>, port: u16, username: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port,
            username: username.into(),
            keepalive: Some(Duration::from_secs(60)),
            connect_timeout: Duration::from_secs(30),
            agent: AgentSource::default(),
            identity_files: Vec::new(),
            memory_identities: Vec::new(),
            keyboard_interactive: true,
            password: true,
            password_attempts: 3,
            known_hosts: Arc::new(MemoryKnownHosts::new()),
            security_key: None,
        }
    }

    /// Signs for `sk-*` keys with `security_key` instead of the build's
    /// default authenticator.
    pub fn security_key(mut self, security_key: Arc<dyn SecurityKey>) -> Self {
        self.security_key = Some(security_key);
        self
    }

    pub fn known_hosts(mut self, store: Arc<dyn KnownHosts>) -> Self {
        self.known_hosts = store;
        self
    }

    pub fn identity_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.identity_files.push(path.into());
        self
    }

    pub fn memory_identity(mut self, identity: MemoryIdentity) -> Self {
        self.memory_identities.push(identity);
        self
    }

    pub fn agent(mut self, agent: AgentSource) -> Self {
        self.agent = agent;
        self
    }

    pub fn keepalive(mut self, interval: Option<Duration>) -> Self {
        self.keepalive = interval;
        self
    }
}

impl std::fmt::Debug for ConnectParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectParams")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username)
            .field("keepalive", &self.keepalive)
            .field("agent", &self.agent)
            .field("identity_files", &self.identity_files)
            .field("memory_identities", &self.memory_identities)
            .finish_non_exhaustive()
    }
}
