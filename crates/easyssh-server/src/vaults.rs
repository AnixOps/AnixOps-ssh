//! Vaults, membership and roles.

use crate::app::{AppState, Authed};
use crate::db::now;
use crate::error::{ApiError, ApiJson};
use crate::records::{item_result, mark_not_applied, push_one};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use easyssh_sync::wire::{
    BatchItemResult, CreateVaultRequest, ErrorCode, MemberInfo, PutMemberRequest, RevokedDevice,
    Role, RotateRequest, RotateResponse, VaultSummary,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeSet;

/// Length of a sealed vault key (`seal_vault_key` output).
const SEALED_LEN: usize = 105;

pub fn valid_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

pub fn role_of(
    conn: &Connection,
    vault_id: &str,
    account_id: &str,
) -> Result<Option<Role>, ApiError> {
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM vault_members WHERE vault_id = ?1 AND account_id = ?2",
            [vault_id, account_id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(role.and_then(|r| Role::parse(&r)))
}

/// Current key generation of the vault (1 until the first rotation).
pub fn vault_generation(conn: &Connection, vault_id: &str) -> Result<u32, ApiError> {
    Ok(conn.query_row(
        "SELECT key_generation FROM vaults WHERE id = ?1",
        [vault_id],
        |r| r.get::<_, i64>(0),
    )? as u32)
}

/// Members see their vaults; everyone else gets 404 so ids do not leak.
pub fn require_member(
    conn: &Connection,
    vault_id: &str,
    account_id: &str,
) -> Result<Role, ApiError> {
    role_of(conn, vault_id, account_id)?.ok_or_else(|| ApiError::not_found("no such vault"))
}

pub async fn create(
    State(state): State<AppState>,
    auth: Authed,
    ApiJson(request): ApiJson<CreateVaultRequest>,
) -> Result<StatusCode, ApiError> {
    if !valid_id(&request.vault_id) {
        return Err(ApiError::bad_request(
            "vault id must be 1-128 characters from a-z, 0-9 and - _ . :",
        ));
    }
    if request.sealed_key.len() != SEALED_LEN {
        return Err(ApiError::bad_request("sealed key has a bad length"));
    }
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            let inserted = tx.execute(
                "INSERT INTO vaults(id, created_by, created_at) VALUES (?1,?2,?3)",
                params![request.vault_id, auth.account_id, now()],
            );
            match inserted {
                Ok(_) => {}
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    return Err(ApiError::conflict("vault id is taken"));
                }
                Err(e) => return Err(e.into()),
            }
            tx.execute(
                "INSERT INTO vault_members(vault_id, account_id, role, sealed_key) \
                 VALUES (?1,?2,'owner',?3)",
                params![request.vault_id, auth.account_id, request.sealed_key],
            )?;
            tx.commit()?;
            Ok(StatusCode::CREATED)
        })
        .await
}

pub async fn list(
    State(state): State<AppState>,
    auth: Authed,
) -> Result<Json<Vec<VaultSummary>>, ApiError> {
    state
        .db
        .call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT m.vault_id, m.role, m.sealed_key, v.key_generation \
                 FROM vault_members m JOIN vaults v ON v.id = m.vault_id \
                 WHERE m.account_id = ?1 ORDER BY m.vault_id",
            )?;
            let rows = stmt.query_map([&auth.account_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (vault_id, role, sealed_key, generation) = row?;
                if let Some(role) = Role::parse(&role) {
                    out.push(VaultSummary {
                        vault_id,
                        role,
                        sealed_key,
                        key_generation: generation as u32,
                    });
                }
            }
            Ok(Json(out))
        })
        .await
}

pub async fn members(
    State(state): State<AppState>,
    auth: Authed,
    Path(vault_id): Path<String>,
) -> Result<Json<Vec<MemberInfo>>, ApiError> {
    state
        .db
        .call(move |conn| {
            require_member(conn, &vault_id, &auth.account_id)?;
            let mut stmt = conn.prepare(
                "SELECT a.id, a.username, m.role, a.seal_public, a.signing_public \
                 FROM vault_members m JOIN accounts a ON a.id = m.account_id \
                 WHERE m.vault_id = ?1 ORDER BY a.username",
            )?;
            let rows = stmt.query_map([&vault_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (account_id, username, role, seal_public, signing_public) = row?;
                if let Some(role) = Role::parse(&role) {
                    out.push(MemberInfo {
                        account_id,
                        username,
                        role,
                        seal_public,
                        signing_public,
                    });
                }
            }
            Ok(Json(out))
        })
        .await
}

fn owner_count(conn: &Connection, vault_id: &str) -> Result<i64, ApiError> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM vault_members WHERE vault_id = ?1 AND role = 'owner'",
        [vault_id],
        |r| r.get(0),
    )?)
}

/// Add a member or change a role. Owners only.
pub async fn put_member(
    State(state): State<AppState>,
    auth: Authed,
    Path((vault_id, account_id)): Path<(String, String)>,
    ApiJson(request): ApiJson<PutMemberRequest>,
) -> Result<StatusCode, ApiError> {
    if let Some(key) = &request.sealed_key {
        if key.len() != SEALED_LEN {
            return Err(ApiError::bad_request("sealed key has a bad length"));
        }
        if request.key_generation.is_none() {
            return Err(ApiError::bad_request(
                "a sealed key needs the generation of the vault key",
            ));
        }
    }
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            if require_member(&tx, &vault_id, &auth.account_id)? != Role::Owner {
                return Err(ApiError::forbidden("only owners change membership"));
            }
            let target_exists = tx
                .query_row(
                    "SELECT 1 FROM accounts WHERE id = ?1",
                    [&account_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !target_exists {
                return Err(ApiError::not_found("no such account"));
            }
            let current = role_of(&tx, &vault_id, &account_id)?;
            if request.sealed_key.is_some()
                && request.key_generation != Some(vault_generation(&tx, &vault_id)?)
            {
                return Err(ApiError::coded(
                    StatusCode::PRECONDITION_FAILED,
                    ErrorCode::StaleKeyGeneration,
                    "the vault key was rotated; seal the current key",
                ));
            }
            match (current, &request.sealed_key) {
                (None, None) => {
                    return Err(ApiError::bad_request(
                        "a new member needs the vault key sealed to them",
                    ));
                }
                (None, Some(key)) => {
                    tx.execute(
                        "INSERT INTO vault_members(vault_id, account_id, role, sealed_key) \
                         VALUES (?1,?2,?3,?4)",
                        params![vault_id, account_id, request.role.as_str(), key],
                    )?;
                }
                (Some(old), key) => {
                    if old == Role::Owner
                        && request.role != Role::Owner
                        && owner_count(&tx, &vault_id)? <= 1
                    {
                        return Err(ApiError::conflict("a vault needs at least one owner"));
                    }
                    match key {
                        Some(key) => tx.execute(
                            "UPDATE vault_members SET role = ?1, sealed_key = ?2 \
                             WHERE vault_id = ?3 AND account_id = ?4",
                            params![request.role.as_str(), key, vault_id, account_id],
                        )?,
                        None => tx.execute(
                            "UPDATE vault_members SET role = ?1 \
                             WHERE vault_id = ?2 AND account_id = ?3",
                            params![request.role.as_str(), vault_id, account_id],
                        )?,
                    };
                }
            }
            tx.commit()?;
            tracing::info!(role = request.role.as_str(), "vault membership changed");
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}

fn refused(generation: u32) -> RotateResponse {
    RotateResponse {
        committed: false,
        key_generation: generation,
        members_changed: false,
        missing: Vec::new(),
        results: Vec::new(),
    }
}

fn reply(response: RotateResponse) -> Response {
    let status = if response.committed {
        StatusCode::OK
    } else {
        StatusCode::CONFLICT
    };
    (status, Json(response)).into_response()
}

/// Replace the vault key and, optionally, remove a member in the same step.
/// Owners only. This is the only way to remove a member: their copy of the
/// old key is worthless for anything written afterwards because the server
/// refuses writes under the old generation and every record is re-encrypted.
///
/// All or nothing: the key generation, the members, and every record of the
/// vault are checked and written in one transaction. See
/// [`RotateRequest`].
pub async fn rotate(
    State(state): State<AppState>,
    auth: Authed,
    Path(vault_id): Path<String>,
    ApiJson(request): ApiJson<RotateRequest>,
) -> Result<Response, ApiError> {
    if request
        .sealed_keys
        .iter()
        .any(|k| k.sealed_key.len() != SEALED_LEN)
    {
        return Err(ApiError::bad_request("sealed key has a bad length"));
    }
    let sealed_ids: BTreeSet<&str> = request
        .sealed_keys
        .iter()
        .map(|k| k.account_id.as_str())
        .collect();
    if sealed_ids.len() != request.sealed_keys.len() {
        return Err(ApiError::bad_request("a member appears twice"));
    }
    let mut ids = BTreeSet::new();
    if !request
        .records
        .iter()
        .all(|r| ids.insert(r.envelope.record_id.clone()))
    {
        return Err(ApiError::bad_request("a record appears twice"));
    }
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            if require_member(&tx, &vault_id, &auth.account_id)? != Role::Owner {
                return Err(ApiError::forbidden("only owners rotate the vault key"));
            }
            let generation = vault_generation(&tx, &vault_id)?;
            if request.from_generation != generation {
                return Ok(reply(refused(generation)));
            }

            // Members after the removal must be exactly those given a key.
            let mut members: BTreeSet<String> = tx
                .prepare("SELECT account_id FROM vault_members WHERE vault_id = ?1")?
                .query_map([&vault_id], |r| r.get::<_, String>(0))?
                .collect::<Result<_, _>>()?;
            if let Some(removed) = &request.remove_member {
                if !members.remove(removed) {
                    return Ok(reply(RotateResponse {
                        members_changed: true,
                        ..refused(generation)
                    }));
                }
                let removing_owner = role_of(&tx, &vault_id, removed)? == Some(Role::Owner);
                if removing_owner && owner_count(&tx, &vault_id)? <= 1 {
                    return Err(ApiError::conflict("a vault needs at least one owner"));
                }
            }
            let given: BTreeSet<String> = request
                .sealed_keys
                .iter()
                .map(|k| k.account_id.clone())
                .collect();
            if given != members {
                return Ok(reply(RotateResponse {
                    members_changed: true,
                    ..refused(generation)
                }));
            }

            // Every record the vault holds, re-encrypted under the new key.
            let new_generation = generation + 1;
            let mut results = Vec::with_capacity(request.records.len());
            for item in &request.records {
                results.push(item_result(push_one(
                    &tx,
                    &auth.account_id,
                    &vault_id,
                    new_generation,
                    item,
                ))?);
            }
            let missing: Vec<String> = tx
                .prepare("SELECT record_id FROM records WHERE vault_id = ?1 ORDER BY record_id")?
                .query_map([&vault_id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?
                .into_iter()
                .filter(|id| !ids.contains(id))
                .collect();
            let all_ok = results
                .iter()
                .all(|r| matches!(r, BatchItemResult::Accepted { .. }));
            if !all_ok || !missing.is_empty() {
                // Dropping the transaction rolls everything back.
                mark_not_applied(&mut results);
                return Ok(reply(RotateResponse {
                    committed: false,
                    key_generation: generation,
                    members_changed: false,
                    missing,
                    results,
                }));
            }

            tx.execute(
                "UPDATE vaults SET key_generation = ?1 WHERE id = ?2",
                params![i64::from(new_generation), vault_id],
            )?;
            if let Some(removed) = &request.remove_member {
                tx.execute(
                    "DELETE FROM vault_members WHERE vault_id = ?1 AND account_id = ?2",
                    params![vault_id, removed],
                )?;
            }
            for key in &request.sealed_keys {
                tx.execute(
                    "UPDATE vault_members SET sealed_key = ?1 \
                     WHERE vault_id = ?2 AND account_id = ?3",
                    params![key.sealed_key, vault_id, key.account_id],
                )?;
            }
            tx.commit()?;
            tracing::info!(
                generation = new_generation,
                removed = request.remove_member.is_some(),
                records = results.len(),
                "vault key rotated"
            );
            Ok(reply(RotateResponse {
                committed: true,
                key_generation: new_generation,
                members_changed: false,
                missing: Vec::new(),
                results,
            }))
        })
        .await
}

/// Devices of the vault's members that were revoked, with the time. Clients
/// refuse envelopes such a device signed that the server received later.
pub async fn revoked_devices(
    State(state): State<AppState>,
    auth: Authed,
    Path(vault_id): Path<String>,
) -> Result<Json<Vec<RevokedDevice>>, ApiError> {
    state
        .db
        .call(move |conn| {
            require_member(conn, &vault_id, &auth.account_id)?;
            let mut stmt = conn.prepare(
                "SELECT d.id, d.revoked_at FROM devices d \
                 JOIN vault_members m ON m.account_id = d.account_id \
                 WHERE m.vault_id = ?1 AND d.revoked_at IS NOT NULL ORDER BY d.id",
            )?;
            let rows = stmt
                .query_map([&vault_id], |r| {
                    Ok(RevokedDevice {
                        device_id: r.get(0)?,
                        revoked_at: r.get(1)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Json(rows))
        })
        .await
}
