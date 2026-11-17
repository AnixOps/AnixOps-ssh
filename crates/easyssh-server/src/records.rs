//! Record push (compare-and-set), pull and history.
//!
//! The server verifies signatures and certificates as defence in depth but
//! never decrypts anything and never merges: conflicts go back to clients.

use crate::app::{AppState, Authed};
use crate::db::now;
use crate::devices::account_signing_key;
use crate::error::{ApiError, ApiJson};
use crate::vaults::{require_member, valid_id};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use easyssh_crypto::{verify_envelope, Ordering, VersionVector};
use easyssh_sync::wire::{
    device_id_for, ConflictReason, Envelope, ErrorCode, HistoryItem, HistoryResponse, PullResponse,
    PulledRecord, PushRequest, PushResponse,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Replaced versions kept per record.
pub const HISTORY_LIMIT: i64 = 50;
/// Largest ciphertext accepted for one record.
pub const MAX_CIPHERTEXT: usize = 1024 * 1024;
const MAX_PULL: u32 = 500;

const COLUMNS: &str =
    "record_id, vault_id, tbl, version_vector, deleted, ciphertext, signer, signature, cert";

/// Read an envelope from nine consecutive columns starting at `o`
/// (the order of [`COLUMNS`]).
fn envelope_at(r: &Row<'_>, o: usize) -> rusqlite::Result<Envelope> {
    let vector: String = r.get(o + 3)?;
    Ok(Envelope {
        record_id: r.get(o)?,
        vault_id: r.get(o + 1)?,
        table: r.get(o + 2)?,
        version_vector: serde_json::from_str(&vector).unwrap_or_default(),
        deleted: r.get::<_, i64>(o + 4)? != 0,
        ciphertext: r.get(o + 5)?,
        signer: r.get(o + 6)?,
        signature: r.get(o + 7)?,
        cert: r.get(o + 8)?,
    })
}

fn current(
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

pub async fn push(
    State(state): State<AppState>,
    auth: Authed,
    Path(vault_id): Path<String>,
    ApiJson(request): ApiJson<PushRequest>,
) -> Result<Response, ApiError> {
    check_shape(&request.envelope, &vault_id)?;
    let (signed, cert) = request
        .envelope
        .to_signed()
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let role = require_member(&tx, &vault_id, &auth.account_id)?;
            if !role.can_write() {
                return Err(ApiError::forbidden("viewers cannot write"));
            }

            // The signing device must be a live device of the pushing account.
            let device_id = device_id_for(&signed.signer);
            let device: Option<(String, Option<i64>)> = tx
                .query_row(
                    "SELECT account_id, revoked_at FROM devices WHERE id = ?1",
                    [&device_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match device {
                Some((owner, None)) if owner == auth.account_id => {}
                Some((owner, Some(_))) if owner == auth.account_id => {
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
            let account_key = account_signing_key(&tx, &auth.account_id)?;
            verify_envelope(&signed, &cert, &account_key).map_err(|_| {
                ApiError::coded(
                    StatusCode::FORBIDDEN,
                    ErrorCode::InvalidSignature,
                    "envelope signature or certificate is invalid",
                )
            })?;

            let envelope = &request.envelope;
            let existing = current(&tx, &vault_id, &envelope.record_id)?;
            let expected = vector_of(&request.expected);
            match &existing {
                None if !expected.is_empty() => {
                    return Ok(conflict(ConflictReason::Cas, None));
                }
                Some((_, cur)) => {
                    if cur.table != envelope.table {
                        return Err(ApiError::bad_request("a record cannot change its table"));
                    }
                    let cur_vector = cur.vector();
                    if cur_vector != expected {
                        return Ok(conflict(ConflictReason::Cas, Some(cur.clone())));
                    }
                    if envelope.vector().compare(&cur_vector) != Ordering::After {
                        return Ok(conflict(ConflictReason::Stale, Some(cur.clone())));
                    }
                }
                None => {}
            }

            let seq: i64 = tx.query_row(
                "SELECT COALESCE(MAX(seq), 0) + 1 FROM records",
                [],
                |r| r.get(0),
            )?;
            // Records are never deleted, so MAX(seq) only grows; replaced
            // versions are archived before the row is overwritten.
            if let Some((old_seq, old)) = &existing {
                tx.execute(
                    "INSERT INTO record_history(vault_id, record_id, seq, tbl, version_vector, \
                     deleted, ciphertext, signer, signature, cert, replaced_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                    params![
                        vault_id,
                        old.record_id,
                        old_seq,
                        old.table,
                        serde_json::to_string(&old.version_vector).unwrap_or_default(),
                        i64::from(old.deleted),
                        old.ciphertext,
                        old.signer,
                        old.signature,
                        old.cert,
                        now()
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
                &format!(
                    "INSERT OR REPLACE INTO records(seq, updated_at, {COLUMNS}) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)"
                ),
                params![
                    seq,
                    now(),
                    envelope.record_id,
                    vault_id,
                    envelope.table,
                    serde_json::to_string(&envelope.version_vector).unwrap_or_default(),
                    i64::from(envelope.deleted),
                    envelope.ciphertext,
                    envelope.signer,
                    envelope.signature,
                    envelope.cert
                ],
            )?;
            tx.commit()?;
            Ok(Json(PushResponse::Accepted { seq: seq as u64 }).into_response())
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
