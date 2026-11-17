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
//! - [`hosts`]: the host list and new hosts, for the mobile UI.
//! - [`known_hosts`]: host keys in the store (shared with the daemon).
//! - [`prompts`]: passwords and host-key questions asked through events.
//! - [`Engine`]: opens shells and keeps them by id.

pub mod hosts;
pub mod known_hosts;
pub mod prompts;
mod shell;
mod utf8;

pub use shell::{error_code, Engine, OpenError, ShellSink, Target};
pub use utf8::Utf8Stream;
