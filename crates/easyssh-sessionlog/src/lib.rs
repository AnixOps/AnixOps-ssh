//! Encrypted session logs.
//!
//! * [`format`]: the `.esl` file format (a libsodium-secretstream style
//!   chunked AEAD stream over XChaCha20-Poly1305), writer and reader.
//! * [`store`]: where logs live (`<data dir>/logs/<host>/<time>.esl`), listing,
//!   deleting and retention cleanup.
//! * [`settings`]: the global default, per-host overrides and retention.
//! * [`text`]: best-effort plain-text export of a decrypted log.
//! * [`share`]: the key-wrapping hook for sharing a log through a vault.
//!
//! The log key is never stored. It is derived per log from the vault's device
//! key with HKDF-SHA256 and the random salt in the file header; callers pass
//! the derivation as a closure, so this crate never touches the vault.

#![forbid(unsafe_code)]

pub mod format;
pub mod settings;
pub mod share;
pub mod store;
pub mod text;

pub use format::{
    decrypt, Decrypted, Header, KeyFn, LogError, LogKey, LogWriter, CHUNK_BYTES, FLUSH_INTERVAL,
    KEY_INFO,
};
pub use settings::{LogSettings, DEFAULT_RETENTION_DAYS};
pub use store::{CleanupReport, LogEntry, LogStore};
