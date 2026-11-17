//! The record channel the sync engine talks to, and an in-memory server for tests.

use crate::error::TransportError;
use crate::wire::{
    BatchItemResult, BatchPushRequest, BatchPushResponse, ConflictReason, Envelope, ErrorCode,
    HistoryItem, HistoryResponse, PullResponse, PulledRecord, PushRequest, PushResponse,
    RotateRequest, RotateResponse,
};
use easyssh_crypto::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::{Arc, Mutex};

/// How many replaced versions a server keeps per record.
pub const HISTORY_LIMIT: usize = 50;

/// Record push, pull and history for one signed-in device.
pub trait Transport: Send + Sync {
    fn push(
        &self,
        vault_id: &str,
        request: &PushRequest,
    ) -> impl Future<Output = Result<PushResponse, TransportError>> + Send;

    /// Several pushes in one transaction; all are applied or none.
    fn push_batch(
        &self,
        vault_id: &str,
        request: &BatchPushRequest,
    ) -> impl Future<Output = Result<BatchPushResponse, TransportError>> + Send;

    /// Replace the vault key: re-encrypted records for every record of the
    /// vault, applied atomically (owners only on a real server).
    fn rotate(
        &self,
        vault_id: &str,
        request: &RotateRequest,
    ) -> impl Future<Output = Result<RotateResponse, TransportError>> + Send;

    fn pull(
        &self,
        vault_id: &str,
        since: u64,
        limit: u32,
    ) -> impl Future<Output = Result<PullResponse, TransportError>> + Send;

    fn history(
        &self,
        vault_id: &str,
        record_id: &str,
    ) -> impl Future<Output = Result<HistoryResponse, TransportError>> + Send;
}

#[derive(Default, Clone)]
struct VaultData {
    records: HashMap<String, (u64, Envelope)>,
    history: HashMap<String, Vec<HistoryItem>>,
    /// 0 stands for the initial generation 1.
    key_generation: u32,
}

impl VaultData {
    fn generation(&self) -> u32 {
        self.key_generation.max(1)
    }
}

#[derive(Default, Clone)]
struct MemoryInner {
    seq: u64,
    /// Receive time stamped on stored envelopes (0 = unknown).
    now: i64,
    vaults: HashMap<String, VaultData>,
}

/// A tiny in-memory stand-in for the server: compare-and-set, sequence
/// numbers and history, without accounts or signature checks. Cloning shares
/// the same state, so several engines can talk to one "server".
#[derive(Clone, Default)]
pub struct MemoryServer {
    inner: Arc<Mutex<MemoryInner>>,
}

impl MemoryServer {
    pub fn new() -> Self {
        Self::default()
    }

    /// A transport handle for one client.
    pub fn transport(&self) -> MemoryTransport {
        MemoryTransport {
            server: self.clone(),
        }
    }

    /// Overwrite what the server holds, as a malicious or restored server
    /// could (used by rollback tests).
    pub fn force_set(&self, envelope: Envelope) {
        let mut inner = self.inner.lock().expect("lock");
        inner.seq += 1;
        let seq = inner.seq;
        inner
            .vaults
            .entry(envelope.vault_id.clone())
            .or_default()
            .records
            .insert(envelope.record_id.clone(), (seq, envelope));
    }

    /// Set the time the server stamps on envelopes it receives from now on.
    pub fn set_time(&self, now: i64) {
        self.inner.lock().expect("lock").now = now;
    }

    /// Current key generation of a vault (1 if it holds nothing yet).
    pub fn key_generation(&self, vault_id: &str) -> u32 {
        let inner = self.inner.lock().expect("lock");
        inner.vaults.get(vault_id).map_or(1, VaultData::generation)
    }

    pub fn current(&self, vault_id: &str, record_id: &str) -> Option<Envelope> {
        let inner = self.inner.lock().expect("lock");
        inner
            .vaults
            .get(vault_id)?
            .records
            .get(record_id)
            .map(|(_, e)| e.clone())
    }
}

#[derive(Clone)]
pub struct MemoryTransport {
    server: MemoryServer,
}

fn stale_generation(current: u32) -> TransportError {
    TransportError::Api {
        status: 412,
        code: ErrorCode::StaleKeyGeneration,
        message: format!("the vault key is at generation {current}"),
    }
}

/// Compare-and-set one record into `inner` (no role or signature checks).
fn apply_push(
    inner: &mut MemoryInner,
    vault_id: &str,
    request: &PushRequest,
) -> Result<PushResponse, TransportError> {
    let envelope = &request.envelope;
    let generation = inner.vaults.get(vault_id).map_or(1, VaultData::generation);
    if envelope.key_generation != generation {
        return Err(stale_generation(generation));
    }
    let current = inner
        .vaults
        .get(vault_id)
        .and_then(|v| v.records.get(&envelope.record_id))
        .map(|(_, e)| e.clone());
    if let Some(cur) = &current {
        if cur.version_vector != request.expected {
            return Ok(PushResponse::Conflict {
                reason: ConflictReason::Cas,
                current,
            });
        }
        if envelope.vector().compare(&cur.vector()) != Ordering::After {
            return Ok(PushResponse::Conflict {
                reason: ConflictReason::Stale,
                current,
            });
        }
    } else if !request.expected.is_empty() {
        return Ok(PushResponse::Conflict {
            reason: ConflictReason::Cas,
            current: None,
        });
    }
    inner.seq += 1;
    let seq = inner.seq;
    let mut stored = envelope.clone();
    stored.received_at = inner.now;
    let vault = inner.vaults.entry(vault_id.to_owned()).or_default();
    if let Some((old_seq, old)) = vault
        .records
        .insert(envelope.record_id.clone(), (seq, stored))
    {
        let list = vault.history.entry(envelope.record_id.clone()).or_default();
        list.insert(
            0,
            HistoryItem {
                seq: old_seq,
                replaced_at: 0,
                envelope: old,
            },
        );
        list.truncate(HISTORY_LIMIT);
    }
    Ok(PushResponse::Accepted { seq })
}

/// Apply every item to a copy of the state; keep the copy only if all passed.
fn apply_all(
    inner: &mut MemoryInner,
    vault_id: &str,
    items: &[PushRequest],
) -> Result<(bool, Vec<BatchItemResult>), TransportError> {
    let mut staged = inner.clone();
    let mut results = Vec::with_capacity(items.len());
    let mut all_ok = true;
    for item in items {
        results.push(match apply_push(&mut staged, vault_id, item) {
            Ok(PushResponse::Accepted { seq }) => BatchItemResult::Accepted { seq },
            Ok(PushResponse::Conflict { reason, current }) => {
                all_ok = false;
                BatchItemResult::Conflict { reason, current }
            }
            Err(TransportError::Api { code, message, .. }) => {
                all_ok = false;
                BatchItemResult::Rejected { code, message }
            }
            Err(other) => return Err(other),
        });
    }
    if all_ok {
        *inner = staged;
    } else {
        for result in &mut results {
            if matches!(result, BatchItemResult::Accepted { .. }) {
                *result = BatchItemResult::NotApplied;
            }
        }
    }
    Ok((all_ok, results))
}

impl Transport for MemoryTransport {
    async fn push(
        &self,
        vault_id: &str,
        request: &PushRequest,
    ) -> Result<PushResponse, TransportError> {
        let mut inner = self.server.inner.lock().expect("lock");
        apply_push(&mut inner, vault_id, request)
    }

    async fn push_batch(
        &self,
        vault_id: &str,
        request: &BatchPushRequest,
    ) -> Result<BatchPushResponse, TransportError> {
        let mut inner = self.server.inner.lock().expect("lock");
        let (committed, results) = apply_all(&mut inner, vault_id, &request.items)?;
        Ok(BatchPushResponse { committed, results })
    }

    async fn rotate(
        &self,
        vault_id: &str,
        request: &RotateRequest,
    ) -> Result<RotateResponse, TransportError> {
        let mut inner = self.server.inner.lock().expect("lock");
        let generation = inner.vaults.get(vault_id).map_or(1, VaultData::generation);
        let refused = |missing: Vec<String>, results| RotateResponse {
            committed: false,
            key_generation: generation,
            members_changed: false,
            missing,
            results,
        };
        if request.from_generation != generation {
            return Ok(refused(Vec::new(), Vec::new()));
        }
        let missing: Vec<String> = inner
            .vaults
            .get(vault_id)
            .map(|v| {
                let mut ids: Vec<String> = v
                    .records
                    .keys()
                    .filter(|id| !request.records.iter().any(|r| &r.envelope.record_id == *id))
                    .cloned()
                    .collect();
                ids.sort();
                ids
            })
            .unwrap_or_default();
        let mut staged = inner.clone();
        staged
            .vaults
            .entry(vault_id.to_owned())
            .or_default()
            .key_generation = generation + 1;
        let (all_ok, results) = apply_all(&mut staged, vault_id, &request.records)?;
        if !all_ok || !missing.is_empty() {
            return Ok(refused(missing, results));
        }
        *inner = staged;
        Ok(RotateResponse {
            committed: true,
            key_generation: generation + 1,
            members_changed: false,
            missing: Vec::new(),
            results,
        })
    }

    async fn pull(
        &self,
        vault_id: &str,
        since: u64,
        limit: u32,
    ) -> Result<PullResponse, TransportError> {
        let inner = self.server.inner.lock().expect("lock");
        let mut all: BTreeMap<u64, Envelope> = BTreeMap::new();
        if let Some(vault) = inner.vaults.get(vault_id) {
            for (seq, env) in vault.records.values() {
                if *seq > since {
                    all.insert(*seq, env.clone());
                }
            }
        }
        let limit = limit.max(1) as usize;
        let has_more = all.len() > limit;
        let records: Vec<PulledRecord> = all
            .into_iter()
            .take(limit)
            .map(|(seq, envelope)| PulledRecord { seq, envelope })
            .collect();
        let cursor = records.last().map_or(since, |r| r.seq);
        Ok(PullResponse {
            records,
            cursor,
            has_more,
        })
    }

    async fn history(
        &self,
        vault_id: &str,
        record_id: &str,
    ) -> Result<HistoryResponse, TransportError> {
        let inner = self.server.inner.lock().expect("lock");
        let versions = inner
            .vaults
            .get(vault_id)
            .and_then(|v| v.history.get(record_id))
            .cloned()
            .unwrap_or_default();
        Ok(HistoryResponse { versions })
    }
}
