//! Local database for EasySSH v2.
//!
//! SQLite with numbered migrations, typed repositories, sync metadata on every
//! syncable row (so the future sync lane can merge field by field), secret
//! fields that only ever reach disk through a [`FieldCipher`], and migration
//! of the v1 JSON configuration. The app (daemon and in-process fallback) opens it
//! through [`app::open_app_store`].

#[macro_use]
mod entities;
pub mod app;
pub mod cipher;
pub mod error;
mod known_hosts;
pub mod meta;
mod migrations;
mod store;
mod v1;

pub use app::{db_path_for, is_invalid_input, open_app_store, AppStore, DB_FILE_NAME};
#[cfg(test)]
pub use cipher::PlaintextForTests;
pub use cipher::{CipherError, FieldCipher, NotConfigured};
pub use entities::{Host, HostSecrets, JumpChain, Tag};
pub use error::{Result, StoreError};
pub use known_hosts::{fingerprint_of, KnownHost, KnownHostsImport, SkippedLine};
pub use meta::{FieldChange, FieldStamp, HistoryEntry, Meta};
pub use migrations::latest_version;
pub use store::{Record, Store, Vault, VaultKind, PERSONAL_VAULT_ID};
pub use v1::{EntityCounts, MigrationReport, Skipped, MIGRATION_SETTING};

#[cfg(test)]
mod tests;
