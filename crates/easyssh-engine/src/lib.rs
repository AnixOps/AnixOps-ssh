//! The SSH engine that runs inside the app process.
//!
//! On the desktop the engine lives in `easysshd`, a daemon that outlives the
//! window. A phone or tablet does not allow a resident background process, so
//! the mobile app embeds this library instead: a terminal session is a russh
//! connection owned by the app, and it is rebuilt by reconnecting when the
//! operating system suspends the app (see `docs/mobile-prototype.md`).
//!
//! Only what every platform can do is here: no PTY, no serial port, no
//! OpenSSH process, no OS keychain. That keeps the crate buildable for
//! `aarch64-linux-android` and `aarch64-apple-ios`.
//!
//! - [`hosts`]: the host list and host editing, for the mobile UI.
//! - [`known_hosts`]: host keys in the store (shared with the daemon).
//! - [`events`]: the sink the engine reports to; the daemon and the app
//!   each implement it.
//! - [`prompts`]: passwords and host-key questions asked through events
//!   (the host-key part is shared with the daemon).
//! - [`registry`]: one connection per host, shared by terminal, files and
//!   tunnels (shared with the daemon).
//! - [`files`]: SFTP sessions and the transfer queue (shared with the daemon).
//! - [`tunnels`]: forwards on the shared connection (shared with the daemon).
//! - [`sync`]: the account, devices and the sync loop (shared with the daemon).
//! - [`Engine`]: puts them together for the app: opens shells, file sessions
//!   and tunnels on leases of the shared connection.
//! - [`supervisor`]: tmux/screen attach and reconnect with backoff on top of it.

pub mod events;
pub mod files;
pub mod hosts;
pub mod known_hosts;
pub mod local_files;
pub mod login;
pub mod prompts;
pub mod registry;
mod shell;
pub mod supervisor;
pub mod sync;
pub mod terminal_files;
pub mod tunnels;
mod utf8;

use easyssh_ssh::{Error as SshError, Session};
use std::sync::Arc;

pub use events::{EventSink, Events};
pub use login::{saved_login, with_saved_login, SavedLogin, SavedPassword};
pub use shell::{error_code, Engine, OpenError, ShellSink, Target};
pub use utf8::{escape_byte, text_to_bytes, Utf8Stream};

/// A connect failure that several waiting callers can share.
pub type SharedError = Arc<SshError>;
/// A counted reference to a host's shared connection.
pub type Lease = registry::Lease<Session, SharedError>;
