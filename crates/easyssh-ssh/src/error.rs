//! The typed error of the engine.

use std::fmt;

/// An authentication method the engine can try. Used to report which method
/// succeeded and which ones were tried before a failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthMethod {
    /// The server accepted the "none" method.
    None,
    /// A key held by an SSH agent.
    Agent,
    /// A private key file (the path as configured).
    IdentityFile(std::path::PathBuf),
    /// A private key the caller handed over in memory (the label names it).
    StoredKey(String),
    KeyboardInteractive,
    Password,
}

impl fmt::Display for AuthMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthMethod::None => f.write_str("none"),
            AuthMethod::Agent => f.write_str("agent"),
            AuthMethod::IdentityFile(path) => write!(f, "identity file {}", path.display()),
            AuthMethod::StoredKey(label) => write!(f, "saved key {label}"),
            AuthMethod::KeyboardInteractive => f.write_str("keyboard-interactive"),
            AuthMethod::Password => f.write_str("password"),
        }
    }
}

fn join_methods(methods: &[AuthMethod]) -> String {
    if methods.is_empty() {
        return "nothing".to_owned();
    }
    methods
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// TCP, DNS or transport trouble, including a connection that dropped.
    #[error("network error: {0}")]
    Network(String),

    /// Every method the server allows was tried and none worked.
    /// `notes` explains skipped methods (agent not running, unreadable key,
    /// unsupported key type, ...).
    #[error("authentication failed (tried: {}){}", join_methods(.tried), format_notes(.notes))]
    AuthFailed {
        tried: Vec<AuthMethod>,
        notes: Vec<String>,
    },

    /// The user (or the Interaction) did not trust the server's host key.
    #[error("host key of {host}:{port} was rejected ({fingerprint})")]
    HostKeyRejected {
        host: String,
        port: u16,
        fingerprint: String,
    },

    /// The server presents a key that differs from the recorded one. Never
    /// accepted automatically.
    #[error(
        "host key of {host}:{port} CHANGED (now {fingerprint}, recorded {}); \
         this could be a man-in-the-middle attack",
        .expected.join(", ")
    )]
    HostKeyMismatch {
        host: String,
        port: u16,
        fingerprint: String,
        expected: Vec<String>,
    },

    /// The server's host key is on a revocation list (`@revoked`).
    #[error("host key of {host}:{port} is revoked ({fingerprint})")]
    HostKeyRevoked {
        host: String,
        port: u16,
        fingerprint: String,
    },

    /// The outbound proxy of the host failed or refused; the message says
    /// which side said no.
    #[error("{0}")]
    Proxy(#[from] crate::proxy::ProxyError),

    /// A channel could not be opened or broke.
    #[error("channel error: {0}")]
    Channel(String),

    /// The server refused to open a session channel. OpenSSH does this when
    /// the connection already carries `MaxSessions` sessions; another
    /// connection to the same host can take more.
    #[error("the server refused to open a channel: {0}")]
    ChannelRefused(String),

    /// The peer violated the protocol or negotiation failed.
    #[error("protocol error: {0}")]
    Protocol(String),

    /// An SFTP operation failed.
    #[error("sftp error: {0}")]
    Sftp(String),

    /// An SFTP path does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A port forward could not be set up (bind failure, request denied).
    #[error("forward error: {0}")]
    Forward(String),

    /// Something the engine does not support (for example FIDO2 `sk-*` keys).
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The Interaction declined to answer (for example no passphrase given).
    #[error("cancelled: {0}")]
    Cancelled(String),

    /// A saved credential cannot be used until its vault is unlocked.
    #[error("locked: {0}")]
    Locked(String),

    /// The known_hosts store could not be read or written.
    #[error("known_hosts error: {0}")]
    KnownHosts(String),

    /// An operation exceeded its time limit.
    #[error("timed out: {0}")]
    Timeout(String),

    /// The session is closed.
    #[error("the connection is closed")]
    Disconnected,
}

fn format_notes(notes: &[String]) -> String {
    if notes.is_empty() {
        String::new()
    } else {
        format!("; {}", notes.join("; "))
    }
}

impl From<russh::Error> for Error {
    fn from(error: russh::Error) -> Self {
        use russh::Error as R;
        match error {
            R::IO(error) => Error::Network(error.to_string()),
            R::Disconnect | R::HUP => Error::Disconnected,
            R::ConnectionTimeout | R::KeepaliveTimeout | R::InactivityTimeout => {
                Error::Network(error.to_string())
            }
            R::ChannelOpenFailure(_) | R::WrongChannel | R::SendError => {
                Error::Channel(error.to_string())
            }
            other => Error::Protocol(other.to_string()),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Network(error.to_string())
    }
}

impl From<russh::keys::Error> for Error {
    fn from(error: russh::keys::Error) -> Self {
        Error::Protocol(error.to_string())
    }
}

impl From<russh_sftp::client::error::Error> for Error {
    fn from(error: russh_sftp::client::error::Error) -> Self {
        use russh_sftp::client::error::Error as S;
        use russh_sftp::protocol::StatusCode;
        match error {
            S::Status(status) if status.status_code == StatusCode::NoSuchFile => {
                Error::NotFound(status.error_message)
            }
            other => Error::Sftp(other.to_string()),
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
