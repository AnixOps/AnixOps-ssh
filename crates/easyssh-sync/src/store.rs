//! What the sync engine needs from local storage.

use crate::merge::Overwritten;
use crate::record::RecordVersion;
use easyssh_crypto::VersionVector;
use std::collections::HashMap;

/// One record in local storage plus its sync bookkeeping.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalRecord {
    pub vault_id: String,
    pub record_id: String,
    pub version: RecordVersion,
    /// The version vector of the copy the server is known to hold (the
    /// compare-and-set token). `None` if the record was never synced.
    pub server_vector: Option<VersionVector>,
}

impl LocalRecord {
    pub fn new(vault_id: &str, record_id: &str, version: RecordVersion) -> Self {
        Self {
            vault_id: vault_id.to_owned(),
            record_id: record_id.to_owned(),
            version,
            server_vector: None,
        }
    }

    /// True if the local version has not been pushed yet.
    pub fn is_dirty(&self) -> bool {
        self.server_vector.as_ref() != Some(&self.version.version_vector)
    }
}

/// Storage adapter. `easyssh-store` implements it in the engine crate;
/// [`MemoryStore`] is for tests.
pub trait SyncStore {
    type Error: std::error::Error + Send + Sync + 'static;

    fn get(&self, vault_id: &str, record_id: &str) -> Result<Option<LocalRecord>, Self::Error>;

    /// Every record of the vault that [`LocalRecord::is_dirty`].
    fn dirty(&self, vault_id: &str) -> Result<Vec<LocalRecord>, Self::Error>;

    /// Store `record` and write `history` (values a merge overwrote) in one
    /// transaction.
    fn put(&mut self, record: LocalRecord, history: Vec<Overwritten>) -> Result<(), Self::Error>;

    /// Highest server sequence already pulled for the vault (0 initially).
    fn cursor(&self, vault_id: &str) -> Result<u64, Self::Error>;

    fn set_cursor(&mut self, vault_id: &str, cursor: u64) -> Result<(), Self::Error>;
}

/// In-memory [`SyncStore`].
#[derive(Debug, Default)]
pub struct MemoryStore {
    records: HashMap<(String, String), LocalRecord>,
    cursors: HashMap<String, u64>,
    /// Everything merges overwrote, in order, with the record it belongs to.
    pub history: Vec<(String, String, Overwritten)>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a record as a local application write would.
    pub fn save(&mut self, record: LocalRecord) {
        self.records
            .insert((record.vault_id.clone(), record.record_id.clone()), record);
    }

    pub fn record(&self, vault_id: &str, record_id: &str) -> Option<&LocalRecord> {
        self.records
            .get(&(vault_id.to_owned(), record_id.to_owned()))
    }
}

impl SyncStore for MemoryStore {
    type Error = std::convert::Infallible;

    fn get(&self, vault_id: &str, record_id: &str) -> Result<Option<LocalRecord>, Self::Error> {
        Ok(self.record(vault_id, record_id).cloned())
    }

    fn dirty(&self, vault_id: &str) -> Result<Vec<LocalRecord>, Self::Error> {
        let mut out: Vec<_> = self
            .records
            .values()
            .filter(|r| r.vault_id == vault_id && r.is_dirty())
            .cloned()
            .collect();
        out.sort_by(|a, b| a.record_id.cmp(&b.record_id));
        Ok(out)
    }

    fn put(&mut self, record: LocalRecord, history: Vec<Overwritten>) -> Result<(), Self::Error> {
        for entry in history {
            self.history
                .push((record.vault_id.clone(), record.record_id.clone(), entry));
        }
        self.save(record);
        Ok(())
    }

    fn cursor(&self, vault_id: &str) -> Result<u64, Self::Error> {
        Ok(self.cursors.get(vault_id).copied().unwrap_or(0))
    }

    fn set_cursor(&mut self, vault_id: &str, cursor: u64) -> Result<(), Self::Error> {
        self.cursors.insert(vault_id.to_owned(), cursor);
        Ok(())
    }
}
