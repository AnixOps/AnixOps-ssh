//! SSH engine on russh: one connection that carries commands, shells, SFTP
//! and port forwards, with an authentication chain the caller can answer
//! through structured prompts ([`Interaction`]).
//!
//! ```no_run
//! # async fn demo(interaction: &dyn easyssh_ssh::Interaction) -> easyssh_ssh::Result<()> {
//! use easyssh_ssh::{ConnectParams, Session};
//! let session = Session::connect(ConnectParams::new("example.org", 22, "me"), interaction).await?;
//! let out = session.exec("uname -a").await?;
//! println!("{} via {}", out.stdout_text(), session.auth_method());
//! # Ok(()) }
//! ```

mod auth;
mod channels;
mod error;
mod forward;
mod interaction;
pub mod known_hosts;
mod params;
mod session;
mod sftp;
pub mod socks5;

pub use auth::{load_identity, plan as plan_auth, security_key_type, Stage, StageConfig};
pub use channels::{ExecOutput, ExitStatus, ShellHandle, ShellOptions};
pub use error::{AuthMethod, Error, Result};
pub use forward::{Forward, ForwardKind};
pub use interaction::{Decision, DenyAll, Interaction, KbdPrompt};
pub use known_hosts::{KnownHostStatus, KnownHosts, KnownHostsFile, MemoryKnownHosts};
pub use params::{AgentSource, ConnectParams};
pub use session::Session;
pub use sftp::{DirEntry, EntryKind, FileStat, Sftp, SftpFile};
