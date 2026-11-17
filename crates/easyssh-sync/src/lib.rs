//! Sync client for EasySSH v2 (architecture-v2.md §8).
//!
//! The server only relays signed, encrypted envelopes. Everything that needs
//! plaintext happens here: [`merge`] combines concurrent versions field by
//! field, [`Verifier`] rejects forged, uncertified and rolled-back
//! envelopes, and [`SyncEngine`] drives push and pull over a [`Transport`]
//! (HTTP via [`HttpClient`], or [`MemoryServer`] in tests).

#![forbid(unsafe_code)]

pub mod account;
pub mod auth;
pub mod codec;
pub mod engine;
mod error;
pub mod http;
pub mod merge;
pub mod record;
pub mod store;
pub mod transport;
pub mod verify;
pub mod wire;

pub use codec::{open_record, seal_record, DeviceIdentity};
pub use engine::{HistoryVersion, Rejected, SyncEngine, SyncReport};
pub use error::{AuthError, MergeError, SyncError, TransportError, VerifyError, WireError};
pub use http::HttpClient;
pub use merge::{merge, Merged, Outcome, Overwritten, Side};
pub use record::{RecordVersion, Stamp};
pub use store::{LocalRecord, MemoryStore, SyncStore};
pub use transport::{MemoryServer, MemoryTransport, Transport, HISTORY_LIMIT};
pub use verify::{HighWater, Verifier};
