//! Device registration (account-certified keys), listing and revocation.

use crate::app::{AppState, Authed};
use crate::db::now;
use crate::error::{ApiError, ApiJson};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use easyssh_crypto::DeviceCert;
use easyssh_sync::wire::{device_id_for, DeviceInfo, RegisterDeviceRequest};
use ed25519_dalek::VerifyingKey;
use rusqlite::{params, OptionalExtension};

/// Account signing key of `account_id`.
pub fn account_signing_key(
    conn: &rusqlite::Connection,
    account_id: &str,
) -> Result<VerifyingKey, ApiError> {
    let bytes: Vec<u8> = conn.query_row(
        "SELECT signing_public FROM accounts WHERE id = ?1",
        [account_id],
        |r| r.get(0),
    )?;
    let array: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::internal("stored signing key has a bad length"))?;
    VerifyingKey::from_bytes(&array).map_err(|_| ApiError::internal("stored signing key invalid"))
}

pub async fn register(
    State(state): State<AppState>,
    auth: Authed,
    ApiJson(request): ApiJson<RegisterDeviceRequest>,
) -> Result<Json<DeviceInfo>, ApiError> {
    let public: [u8; 32] = request
        .device_public
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::bad_request("device public key must be 32 bytes"))?;
    if request.name.is_empty() || request.name.len() > 64 {
        return Err(ApiError::bad_request("device name must be 1-64 bytes"));
    }
    let cert = DeviceCert::from_bytes(&request.cert)
        .map_err(|_| ApiError::bad_request("malformed device certificate"))?;
    if cert.device_public != public {
        return Err(ApiError::bad_request(
            "certificate is for a different device key",
        ));
    }
    let device_id = device_id_for(&public);
    state
        .db
        .call(move |conn| {
            let account_key = account_signing_key(conn, &auth.account_id)?;
            cert.verify(&account_key).map_err(|_| {
                ApiError::coded(
                    StatusCode::FORBIDDEN,
                    easyssh_sync::wire::ErrorCode::InvalidSignature,
                    "certificate is not signed by this account",
                )
            })?;
            let tx = conn.transaction()?;
            let existing: Option<(String, Option<i64>)> = tx
                .query_row(
                    "SELECT account_id, revoked_at FROM devices WHERE id = ?1",
                    [&device_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match existing {
                Some((owner, _)) if owner != auth.account_id => {
                    return Err(ApiError::conflict(
                        "device key is registered to another account",
                    ));
                }
                Some((_, Some(_))) => {
                    return Err(ApiError::coded(
                        StatusCode::FORBIDDEN,
                        easyssh_sync::wire::ErrorCode::DeviceRevoked,
                        "this device key was revoked",
                    ));
                }
                Some(_) => {}
                None => {
                    tx.execute(
                        "INSERT INTO devices(id, account_id, name, public, cert, created_at) \
                         VALUES (?1,?2,?3,?4,?5,?6)",
                        params![
                            device_id,
                            auth.account_id,
                            request.name,
                            request.device_public,
                            request.cert,
                            now()
                        ],
                    )?;
                }
            }
            tx.execute(
                "UPDATE sessions SET device_id = ?1 WHERE token_hash = ?2",
                params![device_id, auth.token_hash],
            )?;
            let info = tx.query_row(
                "SELECT id, name, public, cert, created_at, revoked_at FROM devices WHERE id = ?1",
                [&device_id],
                row_to_info,
            )?;
            tx.commit()?;
            Ok(Json(info))
        })
        .await
}

fn row_to_info(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceInfo> {
    Ok(DeviceInfo {
        device_id: r.get(0)?,
        name: r.get(1)?,
        device_public: r.get(2)?,
        cert: r.get(3)?,
        created_at: r.get(4)?,
        revoked: r.get::<_, Option<i64>>(5)?.is_some(),
    })
}

pub async fn list(
    State(state): State<AppState>,
    auth: Authed,
) -> Result<Json<Vec<DeviceInfo>>, ApiError> {
    state
        .db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, name, public, cert, created_at, revoked_at FROM devices \
                 WHERE account_id = ?1 ORDER BY created_at, id",
            )?;
            let rows = stmt
                .query_map([&auth.account_id], row_to_info)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Json(rows))
        })
        .await
}

/// Revoke a device: its key can no longer push and its sessions end.
/// Records it already signed stay valid (their certificate is unchanged).
pub async fn revoke(
    State(state): State<AppState>,
    auth: Authed,
    Path(device_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let changed = tx.execute(
                "UPDATE devices SET revoked_at = COALESCE(revoked_at, ?1) \
                 WHERE id = ?2 AND account_id = ?3",
                params![now(), device_id, auth.account_id],
            )?;
            if changed == 0 {
                return Err(ApiError::not_found("no such device"));
            }
            tx.execute("DELETE FROM sessions WHERE device_id = ?1", [&device_id])?;
            tx.commit()?;
            tracing::info!("device revoked");
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}
