use clap::{Parser, ValueEnum};
use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    Text,
    Json,
}

/// Configuration, from flags or `EASYSSH_*` environment variables.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "easyssh-server",
    version,
    about = "Self-hosted EasySSH sync server"
)]
pub struct Config {
    /// Address to listen on. Put a TLS-terminating proxy in front of it.
    #[arg(long, env = "EASYSSH_LISTEN", default_value = "127.0.0.1:8787")]
    pub listen: SocketAddr,

    /// SQLite database file (created if missing).
    #[arg(long, env = "EASYSSH_DB", default_value = "easyssh-server.db")]
    pub db: PathBuf,

    /// Lifetime of a session token, in days.
    #[arg(long, env = "EASYSSH_SESSION_DAYS", default_value_t = 30)]
    pub session_days: u32,

    /// Allow anyone who can reach the server to register an account.
    #[arg(
        long,
        env = "EASYSSH_ALLOW_REGISTRATION",
        default_value_t = true,
        action = clap::ArgAction::Set
    )]
    pub allow_registration: bool,

    /// Largest accepted request body, in bytes.
    #[arg(long, env = "EASYSSH_MAX_BODY_BYTES", default_value_t = 2 * 1024 * 1024)]
    pub max_body_bytes: usize,

    /// Largest accepted body of a vault key rotation (it carries every
    /// record of the vault), in bytes.
    #[arg(
        long,
        env = "EASYSSH_MAX_ROTATE_BODY_BYTES",
        default_value_t = 64 * 1024 * 1024
    )]
    pub max_rotate_body_bytes: usize,

    /// Auth requests allowed per client address per minute.
    #[arg(long, env = "EASYSSH_AUTH_RATE_PER_MINUTE", default_value_t = 30)]
    pub auth_rate_per_minute: u32,

    /// Take the client address from `X-Forwarded-For` (only behind a proxy
    /// that sets it; otherwise clients could pick their own address).
    #[arg(
        long,
        env = "EASYSSH_TRUST_PROXY",
        default_value_t = false,
        action = clap::ArgAction::Set
    )]
    pub trust_proxy: bool,

    /// Log format on stderr.
    #[arg(long, env = "EASYSSH_LOG_FORMAT", value_enum, default_value = "text")]
    pub log_format: LogFormat,
}

impl Config {
    /// Defaults with an ephemeral port, for tests and embedding.
    pub fn for_db(db: PathBuf) -> Self {
        let mut config = Self::parse_from(["easyssh-server"]);
        config.listen = SocketAddr::from(([127, 0, 0, 1], 0));
        config.db = db;
        config
    }
}
