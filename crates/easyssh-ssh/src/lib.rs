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
mod security_key;
#[cfg(all(
    feature = "hardware-key",
    any(target_os = "linux", target_os = "macos")
))]
mod security_key_hid;
#[cfg(any(test, feature = "software-key"))]
pub mod security_key_software;
#[cfg(test)]
mod security_key_tests;
mod security_key_windows;
#[cfg(all(feature = "hardware-key", windows))]
mod security_key_winhello;
mod session;
#[cfg(test)]
mod session_tests;
mod sftp;
pub mod socks5;

pub use auth::{
    decode_identity, load_identity, plan as plan_auth, security_key_type, Stage, StageConfig,
};
pub use channels::{ExecOutput, ExitStatus, ShellHandle, ShellOptions};
pub use error::{AuthMethod, Error, Result};
pub use forward::{Endpoint, Forward, ForwardKind};
pub use interaction::{Decision, DenyAll, HostKeyDetails, Interaction, KbdPrompt};
pub use known_hosts::{KnownHostStatus, KnownHosts, KnownHostsFile, MemoryKnownHosts};
pub use params::{
    AgentSource, ConnectParams, MemoryIdentity, DEFAULT_KEEPALIVE, DEFAULT_KEEPALIVE_MAX,
};
pub use security_key::{
    default_security_key, hardware_backend_available, sign_with_security_key, HidSecurityKey,
    SecurityKey, SkAlgorithm, SkAssertion, SkError, SkKeyInfo, SkSignRequest, WebAuthnSecurityKey,
    FLAG_USER_PRESENCE, FLAG_USER_VERIFICATION,
};
pub use session::{Session, CHANNEL_OPEN_TIMEOUT};
pub use sftp::{DirEntry, EntryKind, FileStat, Sftp, SftpFile};
