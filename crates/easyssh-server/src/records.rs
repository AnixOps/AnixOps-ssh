//! Record push (compare-and-set), batch push, pull and history.
//!
//! The server verifies signatures and certificates as defence in depth but
//! never decrypts anything and never merges: conflicts go back to clients.

use crate::app::{AppState, Authed};
use crate::db::now;
use crate::devices::account_signing_key;
use crate::error::{ApiError, ApiJson};
use crate::vaults::{require_member, valid_id, vault_generation};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use easyssh_crypto::{verify_envelope, CryptoError, Ordering, VersionVector};
use easyssh_sync::wire::{
    device_id_for, BatchItemResult, BatchPushRequest, BatchPushResponse, ConflictReason, Envelope,
    ErrorCode, HistoryItem, HistoryResponse, PullResponse, PulledRecord, PushRequest, PushResponse,
};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};

/// Replaced versions kept per record.
pub const HISTORY_LIMIT: i64 = 50;
/// Largest ciphertext accepted for one record.
pub const MAX_CIPHERTEXT: usize = 1024 * 1024;
/// Most records in one batch push.
pub const MAX_BATCH: usize = 500;
const MAX_PULL: u32 = 500;

/// Envelope columns. `updated_at` is when the server received the version;
/// clients see it as `received_at`.
const COLUMNS: &str = "record_id, vault_id, tbl, key_generation, version_vector, deleted, \
                       ciphertext, signer, signature, cert, updated_at";

/// Read an envelope from eleven consecutive columns starting at `o`
/// (the order of [`COLUMNS`]).
fn envelope_at(r: &Row<'_>, o: usize) -> rusqlite::Result<Envelope> {
    let vector: String = r.get(o + 4)?;
    Ok(Envelope {
        record_id: r.get(o)?,
        vault_id: r.get(o + 1)?,
        table: r.get(o + 2)?,
        key_generation: r.get::<_, i64>(o + 3)? as u32,
        version_vector: serde_json::from_str(&vector).unwrap_or_default(),
        deleted: r.get::<_, i64>(o + 5)? != 0,
        ciphertext: r.get(o + 6)?,
        signer: r.get(o + 7)?,
        signature: r.get(o + 8)?,
        cert: r.get(o + 9)?,
        received_at: r.get(o + 10)?,
    })
}

pub(crate) fn current(
    conn: &Connection,
    vault_id: &str,
    record_id: &str,
) -> Result<Option<(i64, Envelope)>, ApiError> {
    Ok(conn
        .query_row(
            &format!("SELECT seq, {COLUMNS} FROM records WHERE vault_id = ?1 AND record_id = ?2"),
            [vault_id, record_id],
            |r| Ok((r.get::<_, i64>(0)?, envelope_at(r, 1)?)),
        )
        .optional()?)
}

fn vector_of(map: &BTreeMap<String, u64>) -> VersionVector {
    VersionVector::from_entries(map.iter().map(|(d, c)| (d.as_str(), *c)))
}

fn conflict(reason: ConflictReason, current: Option<Envelope>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(PushResponse::Conflict { reason, current }),
    )
        .into_response()
}

/// Everything about the envelope that does not need the database.
fn check_shape(envelope: &Envelope, vault_id: &str) -> Result<(), ApiError> {
    if envelope.vault_id != vault_id {
        return Err(ApiError::bad_request("envelope is for another vault"));
    }
    if !valid_id(&envelope.record_id) {
        return Err(ApiError::bad_request("invalid record id"));
    }
    if envelope.table.is_empty()
        || envelope.table.len() > 64
        || !envelope
            .table
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    {
        return Err(ApiError::bad_request("invalid table name"));
    }
    if envelope.ciphertext.len() > MAX_CIPHERTEXT {
        return Err(ApiError::too_large("record ciphertext is too large"));
    }
    if envelope.version_vector.len() > 256 || envelope.version_vector.keys().any(|k| k.len() > 128)
    {
        return Err(ApiError::bad_request("version vector is too large"));
    }
    Ok(())
}

/// What one push did.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Pushed {
    Accepted(i64),
    Conflict(ConflictReason, Option<Envelope>),
}

/// Check and apply one push inside `tx`. The caller has already checked
/// that the account may write to the vault and passes the vault's current
/// key `generation` (for a rotation: the new one).
///
/// Checks, in order: shape, key generation, the signing device is a live
/// device of the account (a revoked device is refused: anything it sends is
/// received after its revocation), the certificate is valid now and the
/// signature covers the envelope, then compare-and-set.
pub(crate) fn push_one(
    tx: &Transaction<'_>,
    account_id: &str,
    vault_id: &str,
    generation: u32,
    request: &PushRequest,
) -> Result<Pushed, ApiError> {
    let envelope = &request.envelope;
    check_shape(envelope, vault_id)?;
    if envelope.key_generation != generation {
        return Err(ApiError::coded(
            StatusCode::PRECONDITION_FAILED,
            ErrorCode::StaleKeyGeneration,
            format!("the vault key is at generation {generation}"),
        ));
    }
    let (signed, cert) = envelope
        .to_signed()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;

    let device_id = device_id_for(&signed.signer);
    let device: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT account_id, revoked_at FROM devices WHERE id = ?1",
            [&device_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match device {
        Some((owner, None)) if owner == account_id => {}
        Some((owner, Some(_))) if owner == account_id => {
            return Err(ApiError::coded(
                StatusCode::FORBIDDEN,
                ErrorCode::DeviceRevoked,
                "this device was revoked",
            ));
        }
        _ => {
            return Err(ApiError::coded(
                StatusCode::FORBIDDEN,
                ErrorCode::DeviceNotRegistered,
                "the signing device is not registered for this account",
            ));
        }
    }
    let received_at = now();
    let account_key = account_signing_key(tx, account_id)?;
    verify_envelope(&signed, &cert, &account_key, received_at).map_err(|e| match e {
        CryptoError::CertificateExpired => ApiError::coded(
            StatusCode::FORBIDDEN,
            ErrorCode::CertificateExpired,
            "the device certificate has expired; sign in again to renew it",
        ),
        _ => ApiError::coded(
            StatusCode::FORBIDDEN,
            ErrorCode::InvalidSignature,
            "envelope signature or certificate is invalid",
        ),
    })?;

    let existing = current(tx, vault_id, &envelope.record_id)?;
    let expected = vector_of(&request.expected);
    match &existing {
        None if !expected.is_empty() => {
            return Ok(Pushed::Conflict(ConflictReason::Cas, None));
        }
        Some((_, cur)) => {
            if cur.table != envelope.table {
                return Err(ApiError::bad_request("a record cannot change its table"));
            }
            let cur_vector = cur.vector();
            if cur_vector != expected {
                return Ok(Pushed::Conflict(ConflictReason::Cas, Some(cur.clone())));
            }
            if envelope.vector().compare(&cur_vector) != Ordering::After {
                return Ok(Pushed::Conflict(ConflictReason::Stale, Some(cur.clone())));
            }
        }
        None => {}
    }

    let seq: i64 = tx.query_row("SELECT COALESCE(MAX(seq), 0) + 1 FROM records", [], |r| {
        r.get(0)
    })?;
    // Records are never deleted, so MAX(seq) only grows; replaced versions
    // are archived before the row is overwritten.
    if let Some((old_seq, old)) = &existing {
        tx.execute(
            "INSERT INTO record_history(vault_id, record_id, seq, tbl, key_generation, \
             version_vector, deleted, ciphertext, signer, signature, cert, updated_at, \
             replaced_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                vault_id,
                old.record_id,
                old_seq,
                old.table,
                i64::from(old.key_generation),
                serde_json::to_string(&old.version_vector).unwrap_or_default(),
                i64::from(old.deleted),
                old.ciphertext,
                old.signer,
                old.signature,
                old.cert,
                old.received_at,
                received_at
            ],
        )?;
        tx.execute(
            "DELETE FROM record_history WHERE vault_id = ?1 AND record_id = ?2 AND id NOT IN \
             (SELECT id FROM record_history WHERE vault_id = ?1 AND record_id = ?2 \
              ORDER BY id DESC LIMIT ?3)",
            params![vault_id, old.record_id, HISTORY_LIMIT],
        )?;
    }
    tx.execute(
        "INSERT OR REPLACE INTO records(seq, record_id, vault_id, tbl, key_generation, \
         version_vector, deleted, ciphertext, signer, signature, cert, updated_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            seq,
            envelope.record_id,
            vault_id,
            envelope.table,
            i64::from(envelope.key_generation),
            serde_json::to_string(&envelope.version_vector).unwrap_or_default(),
            i64::from(envelope.deleted),
            envelope.ciphertext,
            envelope.signer,
            envelope.signature,
            envelope.cert,
            received_at
        ],
    )?;
    Ok(Pushed::Accepted(seq))
}

pub async fn push(
    State(state): State<AppState>,
    auth: Authed,
    Path(vault_id): Path<String>,
    ApiJson(request): ApiJson<PushRequest>,
) -> Result<Response, ApiError> {
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let role = require_member(&tx, &vault_id, &auth.account_id)?;
            if !role.can_write() {
                return Err(ApiError::forbidden("viewers cannot write"));
            }
            let generation = vault_generation(&tx, &vault_id)?;
            match push_one(&tx, &auth.account_id, &vault_id, generation, &request)? {
                Pushed::Accepted(seq) => {
                    tx.commit()?;
                    Ok(Json(PushResponse::Accepted { seq: seq as u64 }).into_response())
                }
                Pushed::Conflict(reason, current) => Ok(conflict(reason, current)),
            }
        })
        .await
}

/// Turn one item's result into its report; internal errors abort the call.
pub(crate) fn item_result(outcome: Result<Pushed, ApiError>) -> Result<BatchItemResult, ApiError> {
    match outcome {
        Ok(Pushed::Accepted(seq)) => Ok(BatchItemResult::Accepted { seq: seq as u64 }),
        Ok(Pushed::Conflict(reason, current)) => Ok(BatchItemResult::Conflict { reason, current }),
        Err(error) if error.code == ErrorCode::Internal => Err(error),
        Err(error) => Ok(BatchItemResult::Rejected {
            code: error.code,
            message: error.message,
        }),
    }
}

/// Accepted items of a batch that is not applied are reported as such.
pub(crate) fn mark_not_applied(results: &mut [BatchItemResult]) {
    for result in results {
        if matches!(result, BatchItemResult::Accepted { .. }) {
            *result = BatchItemResult::NotApplied;
        }
    }
}

/// `POST /v1/vaults/{vault}/records/batch`: all-or-nothing, with a per-item
/// report. Every item is checked exactly like a single push.
pub async fn push_batch(
    State(state): State<AppState>,
    auth: Authed,
    Path(vault_id): Path<String>,
    ApiJson(request): ApiJson<BatchPushRequest>,
) -> Result<Response, ApiError> {
    if request.items.is_empty() || request.items.len() > MAX_BATCH {
        return Err(ApiError::bad_request(format!(
            "a batch holds 1 to {MAX_BATCH} records"
        )));
    }
    let mut seen = HashSet::new();
    if !request
        .items
        .iter()
        .all(|item| seen.insert(item.envelope.record_id.as_str()))
    {
        return Err(ApiError::bad_request("a record appears twice in the batch"));
    }
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let role = require_member(&tx, &vault_id, &auth.account_id)?;
            if !role.can_write() {
                return Err(ApiError::forbidden("viewers cannot write"));
            }
            let generation = vault_generation(&tx, &vault_id)?;
            let mut results = Vec::with_capacity(request.items.len());
            for item in &request.items {
                results.push(item_result(push_one(
                    &tx,
                    &auth.account_id,
                    &vault_id,
                    generation,
                    item,
                ))?);
            }
            let committed = results
                .iter()
                .all(|r| matches!(r, BatchItemResult::Accepted { .. }));
            if committed {
                tx.commit()?;
                Ok((
                    StatusCode::OK,
                    Json(BatchPushResponse { committed, results }),
                )
                    .into_response())
            } else {
                // Dropping the transaction rolls every item back.
                mark_not_applied(&mut results);
                Ok((
                    StatusCode::CONFLICT,
                    Json(BatchPushResponse { committed, results }),
                )
                    .into_response())
            }
        })
        .await
}

#[derive(Deserialize)]
pub struct PullQuery {
    #[serde(default)]
    since: u64,
    limit: Option<u32>,
}

pub async fn pull(
    State(state): State<AppState>,
    auth: Authed,
    Path(vault_id): Path<String>,
    Query(query): Query<PullQuery>,
) -> Result<Json<PullResponse>, ApiError> {
    let limit = query.limit.unwrap_or(MAX_PULL).clamp(1, MAX_PULL);
    state
        .db
        .call(move |conn| {
            require_member(conn, &vault_id, &auth.account_id)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT seq, {COLUMNS} FROM records WHERE vault_id = ?1 AND seq > ?2 \
                 ORDER BY seq LIMIT ?3"
            ))?;
            let rows = stmt.query_map(
                params![vault_id, query.since as i64, i64::from(limit) + 1],
                |r| Ok((r.get::<_, i64>(0)?, envelope_at(r, 1)?)),
            )?;
            let mut records = Vec::new();
            for row in rows {
                let (seq, envelope) = row?;
                records.push(PulledRecord {
                    seq: seq as u64,
                    envelope,
                });
            }
            let has_more = records.len() > limit as usize;
            records.truncate(limit as usize);
            let cursor = records.last().map_or(query.since, |r| r.seq);
            Ok(Json(PullResponse {
                records,
                cursor,
                has_more,
            }))
        })
        .await
}

pub async fn history(
    State(state): State<AppState>,
    auth: Authed,
    Path((vault_id, record_id)): Path<(String, String)>,
) -> Result<Json<HistoryResponse>, ApiError> {
    state
        .db
        .call(move |conn| {
            require_member(conn, &vault_id, &auth.account_id)?;
            let mut stmt = conn.prepare(&format!(
                "SELECT seq, replaced_at, {COLUMNS} FROM record_history \
                 WHERE vault_id = ?1 AND record_id = ?2 ORDER BY id DESC LIMIT ?3"
            ))?;
            let rows = stmt.query_map(params![vault_id, record_id, HISTORY_LIMIT], |r| {
                Ok(HistoryItem {
                    seq: r.get::<_, i64>(0)? as u64,
                    replaced_at: r.get(1)?,
                    envelope: envelope_at(r, 2)?,
                })
            })?;
            let versions = rows.collect::<Result<Vec<_>, _>>()?;
            Ok(Json(HistoryResponse { versions }))
        })
        .await
}
