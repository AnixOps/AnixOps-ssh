//! The push/pull loop.

use crate::codec::{open_record, seal_record, DeviceIdentity};
use crate::error::{RotationFailure, SyncError, VerifyError};
use crate::merge::{merge, Outcome};
use crate::record::RecordVersion;
use crate::store::{LocalRecord, SyncStore};
use crate::transport::Transport;
use crate::verify::Verifier;
use crate::wire::{
    BatchItemResult, BatchPushRequest, Envelope, MemberKey, PushRequest, PushResponse,
    RotateRequest, RotateResponse,
};
use easyssh_crypto::VaultKey;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const PULL_PAGE: u32 = 200;
/// Most records in one batch push.
const BATCH_ITEMS: usize = 100;
/// Most ciphertext bytes in one batch push (the server's body limit is
/// 2 MiB by default).
const BATCH_BYTES: usize = 256 * 1024;
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
    /// Vault keys by vault and key generation. The highest generation is
    /// the one new versions are sealed under.
    vault_keys: HashMap<String, BTreeMap<u32, VaultKey>>,
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

    /// Teach the engine `key`, generation `generation` of the vault's key.
    pub fn add_vault_key(&mut self, vault_id: &str, generation: u32, key: VaultKey) {
        self.vault_keys
            .entry(vault_id.to_owned())
            .or_default()
            .insert(generation, key);
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

    /// The key new versions are sealed under, and its generation.
    fn current_key(&self, vault_id: &str) -> Result<(u32, &VaultKey), SyncError> {
        self.vault_keys
            .get(vault_id)
            .and_then(|keys| keys.iter().next_back())
            .map(|(generation, key)| (*generation, key))
            .ok_or_else(|| SyncError::MissingVaultKey(vault_id.to_owned()))
    }

    /// The key of one generation, for reading an envelope.
    fn key_of(&self, vault_id: &str, generation: u32) -> Result<&VaultKey, SyncError> {
        let keys = self
            .vault_keys
            .get(vault_id)
            .ok_or_else(|| SyncError::MissingVaultKey(vault_id.to_owned()))?;
        keys.get(&generation)
            .ok_or_else(|| SyncError::UnknownKeyGeneration {
                vault_id: vault_id.to_owned(),
                generation,
            })
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
        let remote = open_record(self.key_of(vault_id, envelope.key_generation)?, envelope)?;
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
        // Records the server has never seen go up in batches (initial
        // upload); anything a batch could not take falls through to the
        // per-record path below, which merges and retries on conflicts.
        let fresh: Vec<&LocalRecord> = dirty.iter().filter(|r| r.server_vector.is_none()).collect();
        if fresh.len() > 1 {
            self.push_fresh(vault_id, &fresh, report).await?;
        }
        for first in dirty {
            self.push_record(vault_id, &first.record_id, report).await?;
        }
        Ok(())
    }

    async fn push_fresh(
        &mut self,
        vault_id: &str,
        fresh: &[&LocalRecord],
        report: &mut SyncReport,
    ) -> Result<(), SyncError> {
        let mut start = 0;
        while start < fresh.len() {
            let mut items = Vec::new();
            let mut bytes = 0usize;
            let (generation, key) = self.current_key(vault_id)?;
            while start + items.len() < fresh.len() && items.len() < BATCH_ITEMS {
                let record = fresh[start + items.len()];
                let envelope = seal_record(
                    &self.identity,
                    key,
                    generation,
                    vault_id,
                    &record.record_id,
                    &record.version,
                )?;
                bytes += envelope.ciphertext.len();
                items.push(PushRequest {
                    envelope,
                    expected: BTreeMap::new(),
                });
                if bytes >= BATCH_BYTES {
                    break;
                }
            }
            let count = items.len();
            let response = self
                .transport
                .push_batch(vault_id, &BatchPushRequest { items })
                .await?;
            if response.committed && response.results.len() == count {
                for record in &fresh[start..start + count] {
                    self.verifier.record_local(
                        vault_id,
                        &record.record_id,
                        &record.version.version_vector,
                    );
                    let mut done = (*record).clone();
                    done.server_vector = Some(done.version.version_vector.clone());
                    self.store.put(done, Vec::new()).map_err(Self::store_err)?;
                    report.pushed += 1;
                }
            }
            start += count;
        }
        Ok(())
    }

    /// Replace the vault's key (owners only; the server enforces it).
    ///
    /// Re-encrypts every record this device holds for the vault under
    /// `new_key` as a new version signed by this device, and sends them with
    /// the new key sealed to each remaining member (`sealed_keys`, which must
    /// name exactly them) and, optionally, the member to remove. The server
    /// applies all of it or none, and afterwards refuses writes under the old
    /// generation. Sync the vault first so nothing is missing; on a retryable
    /// [`RotationFailure`] (see [`RotationFailure::is_retryable`]) pull and
    /// call again. Returns the new key generation.
    pub async fn rotate_vault_key(
        &mut self,
        vault_id: &str,
        new_key: VaultKey,
        remove_member: Option<String>,
        sealed_keys: Vec<MemberKey>,
    ) -> Result<u32, SyncError> {
        let from_generation = self.current_key(vault_id)?.0;
        let to_generation = from_generation + 1;
        let records = self.store.all(vault_id).map_err(Self::store_err)?;
        let mut items = Vec::with_capacity(records.len());
        let mut staged = Vec::with_capacity(records.len());
        for record in records {
            let mut version = record.version.clone();
            version.version_vector.increment(&self.identity.device_id);
            let envelope = seal_record(
                &self.identity,
                &new_key,
                to_generation,
                vault_id,
                &record.record_id,
                &version,
            )?;
            let expected: BTreeMap<String, u64> = record
                .server_vector
                .as_ref()
                .map(|v| v.iter().map(|(d, c)| (d.to_owned(), c)).collect())
                .unwrap_or_default();
            items.push(PushRequest { envelope, expected });
            staged.push((record.record_id, version));
        }
        let response = self
            .transport
            .rotate(
                vault_id,
                &RotateRequest {
                    from_generation,
                    remove_member,
                    sealed_keys,
                    records: items.clone(),
                },
            )
            .await?;
        if !response.committed {
            return Err(SyncError::Rotation(rotation_failure(&response, &items)));
        }
        for (record_id, version) in staged {
            self.verifier
                .record_local(vault_id, &record_id, &version.version_vector);
            let record = LocalRecord {
                vault_id: vault_id.to_owned(),
                record_id,
                server_vector: Some(version.version_vector.clone()),
                version,
            };
            self.store
                .put(record, Vec::new())
                .map_err(Self::store_err)?;
        }
        self.add_vault_key(vault_id, response.key_generation, new_key);
        Ok(response.key_generation)
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
            let (generation, key) = self.current_key(vault_id)?;
            let envelope = seal_record(
                &self.identity,
                key,
                generation,
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
            // Versions from before a key rotation are encrypted under a key
            // this device no longer holds (the rotation re-encrypted the
            // current versions only); they are skipped.
            let Ok(key) = self.key_of(vault_id, env.key_generation) else {
                continue;
            };
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

fn rotation_failure(response: &RotateResponse, items: &[PushRequest]) -> RotationFailure {
    let ids_where = |wanted: fn(&BatchItemResult) -> bool| -> Vec<String> {
        response
            .results
            .iter()
            .zip(items)
            .filter(|(result, _)| wanted(result))
            .map(|(_, item)| item.envelope.record_id.clone())
            .collect()
    };
    if response.results.is_empty() && !response.members_changed && response.missing.is_empty() {
        return RotationFailure::Generation {
            current: response.key_generation,
        };
    }
    if response.members_changed {
        return RotationFailure::Members;
    }
    if let Some((BatchItemResult::Rejected { code, message }, item)) = response
        .results
        .iter()
        .zip(items)
        .find(|(result, _)| matches!(result, BatchItemResult::Rejected { .. }))
    {
        return RotationFailure::Rejected {
            record_id: item.envelope.record_id.clone(),
            code: *code,
            message: message.clone(),
        };
    }
    let conflicts = ids_where(|r| matches!(r, BatchItemResult::Conflict { .. }));
    if !conflicts.is_empty() {
        return RotationFailure::Conflict(conflicts);
    }
    RotationFailure::Missing(response.missing.clone())
}
