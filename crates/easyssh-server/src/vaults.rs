//! Vaults, membership and roles.

use crate::app::{AppState, Authed};
use crate::db::now;
use crate::error::{ApiError, ApiJson};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use easyssh_sync::wire::{CreateVaultRequest, MemberInfo, PutMemberRequest, Role, VaultSummary};
use rusqlite::{params, Connection, OptionalExtension};

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
                "SELECT vault_id, role, sealed_key FROM vault_members \
                 WHERE account_id = ?1 ORDER BY vault_id",
            )?;
            let rows = stmt.query_map([&auth.account_id], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, Vec<u8>>(2)?,
                ))
            })?;
            let mut out = Vec::new();
            for row in rows {
                let (vault_id, role, sealed_key) = row?;
                if let Some(role) = Role::parse(&role) {
                    out.push(VaultSummary {
                        vault_id,
                        role,
                        sealed_key,
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

/// Remove a member. Owners only; the last owner cannot be removed.
pub async fn remove_member(
    State(state): State<AppState>,
    auth: Authed,
    Path((vault_id, account_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    state
        .db
        .call(move |conn| {
            let tx = conn.transaction()?;
            if require_member(&tx, &vault_id, &auth.account_id)? != Role::Owner {
                return Err(ApiError::forbidden("only owners change membership"));
            }
            let Some(target) = role_of(&tx, &vault_id, &account_id)? else {
                return Err(ApiError::not_found("no such member"));
            };
            if target == Role::Owner && owner_count(&tx, &vault_id)? <= 1 {
                return Err(ApiError::conflict("a vault needs at least one owner"));
            }
            tx.execute(
                "DELETE FROM vault_members WHERE vault_id = ?1 AND account_id = ?2",
                params![vault_id, account_id],
            )?;
            tx.commit()?;
            tracing::info!("vault member removed");
            Ok(StatusCode::NO_CONTENT)
        })
        .await
}
