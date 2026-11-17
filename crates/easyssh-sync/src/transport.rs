//! The record channel the sync engine talks to, and an in-memory server for tests.

use crate::error::TransportError;
use crate::wire::{
    ConflictReason, Envelope, HistoryItem, HistoryResponse, PullResponse, PulledRecord,
    PushRequest, PushResponse,
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

#[derive(Default)]
struct VaultData {
    records: HashMap<String, (u64, Envelope)>,
    history: HashMap<String, Vec<HistoryItem>>,
}

#[derive(Default)]
struct MemoryInner {
    seq: u64,
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

impl Transport for MemoryTransport {
    async fn push(
        &self,
        vault_id: &str,
        request: &PushRequest,
    ) -> Result<PushResponse, TransportError> {
        let mut inner = self.server.inner.lock().expect("lock");
        let envelope = &request.envelope;
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
        let vault = inner.vaults.entry(vault_id.to_owned()).or_default();
        if let Some((old_seq, old)) = vault
            .records
            .insert(envelope.record_id.clone(), (seq, envelope.clone()))
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
