//! The push/pull loop.

use crate::codec::{open_record, seal_record, DeviceIdentity};
use crate::error::{SyncError, VerifyError};
use crate::merge::{merge, Outcome};
use crate::record::RecordVersion;
use crate::store::{LocalRecord, SyncStore};
use crate::transport::Transport;
use crate::verify::Verifier;
use crate::wire::{Envelope, PushRequest, PushResponse};
use easyssh_crypto::VaultKey;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const PULL_PAGE: u32 = 200;
/// Merge-and-retry rounds for one record before giving up.
const MAX_PUSH_ATTEMPTS: usize = 8;

/// An envelope the client refused, and why. The sync continues without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejected {
    pub record_id: String,
    pub reason: VerifyError,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Envelopes received and applied.
    pub pulled: usize,
    /// Records pushed and accepted.
    pub pushed: usize,
    /// Concurrent versions combined by a field-level merge.
    pub merged: usize,
    /// Pushes the server answered with a conflict (each led to a merge).
    pub conflicts: usize,
    /// Values merges overwrote and handed to the store's history.
    pub overwritten: usize,
    pub rejected: Vec<Rejected>,
}

/// A replaced version fetched from the server's history and decrypted.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryVersion {
    pub seq: u64,
    pub replaced_at: i64,
    /// Name of the signing device's certificate.
    pub device_name: String,
    pub version: RecordVersion,
    pub deleted: bool,
}

pub struct SyncEngine<T: Transport, S: SyncStore> {
    transport: T,
    store: S,
    identity: DeviceIdentity,
    verifier: Verifier,
    vault_keys: HashMap<String, VaultKey>,
}

impl<T: Transport, S: SyncStore> SyncEngine<T, S> {
    pub fn new(transport: T, store: S, identity: DeviceIdentity, verifier: Verifier) -> Self {
        Self {
            transport,
            store,
            identity,
            verifier,
            vault_keys: HashMap::new(),
        }
    }

    pub fn add_vault_key(&mut self, vault_id: &str, key: VaultKey) {
        self.vault_keys.insert(vault_id.to_owned(), key);
    }

    pub fn device_id(&self) -> &str {
        &self.identity.device_id
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut S {
        &mut self.store
    }

    pub fn verifier(&self) -> &Verifier {
        &self.verifier
    }

    pub fn verifier_mut(&mut self) -> &mut Verifier {
        &mut self.verifier
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }

    fn key(&self, vault_id: &str) -> Result<&VaultKey, SyncError> {
        self.vault_keys
            .get(vault_id)
            .ok_or_else(|| SyncError::MissingVaultKey(vault_id.to_owned()))
    }

    fn store_err<E: std::error::Error + Send + Sync + 'static>(e: E) -> SyncError {
        SyncError::Store(Box::new(e))
    }

    /// Apply a local edit and persist it (the record becomes dirty).
    pub fn edit_record(
        &mut self,
        vault_id: &str,
        record_id: &str,
        table: &str,
        changes: impl IntoIterator<Item = (String, Value)>,
    ) -> Result<(), SyncError> {
        let mut record = self
            .store
            .get(vault_id, record_id)
            .map_err(Self::store_err)?
            .unwrap_or_else(|| LocalRecord::new(vault_id, record_id, RecordVersion::new(table)));
        record.version.edit(&self.identity.device_id, changes);
        self.store.put(record, Vec::new()).map_err(Self::store_err)
    }

    /// Delete locally (a signed tombstone once pushed).
    pub fn delete_record(&mut self, vault_id: &str, record_id: &str) -> Result<(), SyncError> {
        let Some(mut record) = self
            .store
            .get(vault_id, record_id)
            .map_err(Self::store_err)?
        else {
            return Ok(());
        };
        record.version.delete(&self.identity.device_id);
        self.store.put(record, Vec::new()).map_err(Self::store_err)
    }

    /// Restore one field from a history version as a new local edit.
    pub fn restore_field(
        &mut self,
        vault_id: &str,
        record_id: &str,
        from: &HistoryVersion,
        field: &str,
    ) -> Result<(), SyncError> {
        let Some(value) = from.version.fields.get(field).cloned() else {
            return Err(SyncError::BadRecord("field not in that version"));
        };
        self.edit_record(
            vault_id,
            record_id,
            &from.version.table,
            [(field.to_owned(), value)],
        )
    }

    /// Pull everything new, then push everything dirty (merging and retrying
    /// on conflicts), for one vault.
    pub async fn sync(&mut self, vault_id: &str) -> Result<SyncReport, SyncError> {
        let mut report = SyncReport::default();
        self.pull_into(vault_id, &mut report).await?;
        self.push_into(vault_id, &mut report).await?;
        Ok(report)
    }

    pub async fn pull(&mut self, vault_id: &str) -> Result<SyncReport, SyncError> {
        let mut report = SyncReport::default();
        self.pull_into(vault_id, &mut report).await?;
        Ok(report)
    }

    pub async fn push(&mut self, vault_id: &str) -> Result<SyncReport, SyncError> {
        let mut report = SyncReport::default();
        self.push_into(vault_id, &mut report).await?;
        Ok(report)
    }

    async fn pull_into(
        &mut self,
        vault_id: &str,
        report: &mut SyncReport,
    ) -> Result<(), SyncError> {
        loop {
            let since = self.store.cursor(vault_id).map_err(Self::store_err)?;
            let page = self.transport.pull(vault_id, since, PULL_PAGE).await?;
            for item in &page.records {
                match self.apply_remote(vault_id, &item.envelope, report) {
                    Ok(()) => report.pulled += 1,
                    Err(SyncError::Verify(reason)) => report.rejected.push(Rejected {
                        record_id: item.envelope.record_id.clone(),
                        reason,
                    }),
                    Err(other) => return Err(other),
                }
            }
            if page.cursor > since {
                self.store
                    .set_cursor(vault_id, page.cursor)
                    .map_err(Self::store_err)?;
            }
            if !page.has_more || page.cursor <= since {
                return Ok(());
            }
        }
    }

    /// Verify, decrypt and merge one envelope from the server into local storage.
    fn apply_remote(
        &mut self,
        vault_id: &str,
        envelope: &Envelope,
        report: &mut SyncReport,
    ) -> Result<(), SyncError> {
        if envelope.vault_id != vault_id {
            return Err(VerifyError::Malformed("envelope is for another vault").into());
        }
        self.verifier.verify_and_record(envelope)?;
        let remote = open_record(self.key(vault_id)?, envelope)?;
        let local = self
            .store
            .get(vault_id, &envelope.record_id)
            .map_err(Self::store_err)?;
        let (version, history) = match local {
            None => (remote.clone(), Vec::new()),
            Some(local) => {
                let merged = merge(&local.version, &remote)?;
                let mut version = merged.version;
                if merged.outcome == Outcome::Merged {
                    // The combination is a new version that dominates both.
                    version.version_vector.increment(&self.identity.device_id);
                    report.merged += 1;
                }
                (version, merged.overwritten)
            }
        };
        report.overwritten += history.len();
        let record = LocalRecord {
            vault_id: vault_id.to_owned(),
            record_id: envelope.record_id.clone(),
            version,
            server_vector: Some(remote.version_vector),
        };
        self.store.put(record, history).map_err(Self::store_err)
    }

    async fn push_into(
        &mut self,
        vault_id: &str,
        report: &mut SyncReport,
    ) -> Result<(), SyncError> {
        let dirty = self.store.dirty(vault_id).map_err(Self::store_err)?;
        for first in dirty {
            self.push_record(vault_id, &first.record_id, report).await?;
        }
        Ok(())
    }

    async fn push_record(
        &mut self,
        vault_id: &str,
        record_id: &str,
        report: &mut SyncReport,
    ) -> Result<(), SyncError> {
        for _ in 0..MAX_PUSH_ATTEMPTS {
            let Some(record) = self
                .store
                .get(vault_id, record_id)
                .map_err(Self::store_err)?
            else {
                return Ok(());
            };
            if !record.is_dirty() {
                return Ok(());
            }
            let envelope = seal_record(
                &self.identity,
                self.key(vault_id)?,
                vault_id,
                record_id,
                &record.version,
            )?;
            let expected: BTreeMap<String, u64> = record
                .server_vector
                .as_ref()
                .map(|v| v.iter().map(|(d, c)| (d.to_owned(), c)).collect())
                .unwrap_or_default();
            let request = PushRequest {
                envelope: envelope.clone(),
                expected,
            };
            match self.transport.push(vault_id, &request).await? {
                PushResponse::Accepted { .. } => {
                    self.verifier
                        .record_local(vault_id, record_id, &record.version.version_vector);
                    let mut record = record;
                    record.server_vector = Some(record.version.version_vector.clone());
                    self.store
                        .put(record, Vec::new())
                        .map_err(Self::store_err)?;
                    report.pushed += 1;
                    return Ok(());
                }
                PushResponse::Conflict { current, .. } => {
                    report.conflicts += 1;
                    match current {
                        Some(current) => self.apply_remote(vault_id, &current, report)?,
                        None => {
                            // The server has nothing: push as a new record.
                            let mut record = record;
                            record.server_vector = None;
                            self.store
                                .put(record, Vec::new())
                                .map_err(Self::store_err)?;
                        }
                    }
                }
            }
        }
        Err(SyncError::TooManyConflicts(record_id.to_owned()))
    }

    /// Fetch, verify and decrypt the server's history of one record,
    /// newest first. Versions that fail verification are skipped.
    pub async fn history(
        &self,
        vault_id: &str,
        record_id: &str,
    ) -> Result<Vec<HistoryVersion>, SyncError> {
        let key = self.key(vault_id)?;
        let response = self.transport.history(vault_id, record_id).await?;
        let mut out = Vec::new();
        for item in response.versions {
            let env = &item.envelope;
            if env.vault_id != vault_id || env.record_id != record_id {
                continue;
            }
            // History is older than the high-water mark by definition: check
            // the signature only.
            if self.verifier.verify_signature(env).is_err() {
                continue;
            }
            let version = open_record(key, env)?;
            let (_, cert) = env.to_signed().map_err(|e| VerifyError::Malformed(e.0))?;
            out.push(HistoryVersion {
                seq: item.seq,
                replaced_at: item.replaced_at,
                device_name: cert.device_name,
                deleted: env.deleted,
                version,
            });
        }
        Ok(out)
    }
}
