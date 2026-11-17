//! Connection parameters.

use crate::known_hosts::{KnownHosts, MemoryKnownHosts};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

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
    pub keyboard_interactive: bool,
    pub password: bool,
    /// How often the password prompt is shown when the server refuses it.
    pub password_attempts: u32,
    pub known_hosts: Arc<dyn KnownHosts>,
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
            keyboard_interactive: true,
            password: true,
            password_attempts: 3,
            known_hosts: Arc::new(MemoryKnownHosts::new()),
        }
    }

    pub fn known_hosts(mut self, store: Arc<dyn KnownHosts>) -> Self {
        self.known_hosts = store;
        self
    }

    pub fn identity_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.identity_files.push(path.into());
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
            .finish_non_exhaustive()
    }
}
