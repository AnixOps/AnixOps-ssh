//! Accounts, OPAQUE registration and login, sessions.

use crate::app::{hash_token, AppState, Authed, PendingLogin, LOGIN_TTL};
use crate::db::now;
use crate::error::{ApiError, ApiJson};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use easyssh_crypto::{KdfHeader, KdfParams};
use easyssh_sync::auth::OpaqueSuite;
use easyssh_sync::wire::*;
use hmac::{Hmac, Mac};
use opaque_ke::{
    CredentialFinalization, CredentialRequest, RegistrationRequest, RegistrationUpload,
    ServerLogin, ServerLoginParameters, ServerRegistration,
};
use rand_core::{OsRng, RngCore};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use sha2::Sha256;
use std::time::Instant;

/// Length of `wrap_account_keys` output; anything else is not a wrapped blob.
const WRAPPED_LEN: usize = 105;

#[derive(Deserialize)]
pub struct UsernameQuery {
    username: String,
}

fn normalize(username: &str) -> Result<String, ApiError> {
    let name = username.trim().to_lowercase();
    let ok = (3..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '@' | '+'));
    if ok {
        Ok(name)
    } else {
        Err(ApiError::bad_request(
            "username must be 3-64 characters from a-z, 0-9 and . _ - @ +",
        ))
    }
}

fn limit_user(state: &AppState, name: &str) -> Result<(), ApiError> {
    state
        .limiter
        .check(&format!("user:{name}"), state.config.auth_rate_per_minute)
        .map_err(ApiError::rate_limited)
}

/// `GET /v1/auth/kdf`: the public KDF header the client needs before it can
/// derive its auth secret. Unknown names get a stable, plausible fake so the
/// endpoint does not reveal which accounts exist.
pub async fn kdf_header(
    State(state): State<AppState>,
    Query(query): Query<UsernameQuery>,
) -> Result<Json<KdfHeaderResponse>, ApiError> {
    let name = normalize(&query.username)?;
    limit_user(&state, &name)?;
    let lookup = name.clone();
    let stored = state
        .db
        .call(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT kdf_header FROM accounts WHERE username = ?1",
                    [&lookup],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .optional()?)
        })
        .await?;
    let header = match stored {
        Some(header) => header,
        None => {
            let mut mac = Hmac::<Sha256>::new_from_slice(state.fake_kdf_key.as_slice())
                .map_err(|_| ApiError::internal("hmac key"))?;
            mac.update(name.as_bytes());
            let tag = mac.finalize().into_bytes();
            let mut salt = [0u8; 16];
            salt.copy_from_slice(&tag[..16]);
            KdfHeader {
                params: KdfParams::DESKTOP_V1,
                salt,
            }
            .to_bytes()
            .to_vec()
        }
    };
    Ok(Json(KdfHeaderResponse { kdf_header: header }))
}

pub async fn register_start(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RegisterStartRequest>,
) -> Result<Json<RegisterStartResponse>, ApiError> {
    if !state.config.allow_registration {
        return Err(ApiError::coded(
            StatusCode::FORBIDDEN,
            ErrorCode::RegistrationClosed,
            "registration is closed on this server",
        ));
    }
    let name = normalize(&request.username)?;
    limit_user(&state, &name)?;
    let lookup = name.clone();
    let exists = state
        .db
        .call(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM accounts WHERE username = ?1",
                    [&lookup],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })
        .await?;
    if exists {
        return Err(ApiError::conflict("username is taken"));
    }
    let message = RegistrationRequest::deserialize(&request.request)
        .map_err(|_| ApiError::bad_request("malformed registration request"))?;
    let result = ServerRegistration::<OpaqueSuite>::start(&state.opaque, message, name.as_bytes())
        .map_err(|_| ApiError::bad_request("registration request rejected"))?;
    Ok(Json(RegisterStartResponse {
        response: result.message.serialize().to_vec(),
    }))
}

pub async fn register_finish(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RegisterFinishRequest>,
) -> Result<Json<RegisterFinishResponse>, ApiError> {
    if !state.config.allow_registration {
        return Err(ApiError::coded(
            StatusCode::FORBIDDEN,
            ErrorCode::RegistrationClosed,
            "registration is closed on this server",
        ));
    }
    let name = normalize(&request.username)?;
    limit_user(&state, &name)?;
    KdfHeader::from_bytes(&request.kdf_header)
        .map_err(|_| ApiError::bad_request("invalid KDF header"))?;
    if request.wrapped_account_keys.len() != WRAPPED_LEN {
        return Err(ApiError::bad_request(
            "wrapped account keys have a bad length",
        ));
    }
    if request.seal_public.len() != 32 {
        return Err(ApiError::bad_request("seal public key must be 32 bytes"));
    }
    let signing: [u8; 32] = request
        .signing_public
        .as_slice()
        .try_into()
        .map_err(|_| ApiError::bad_request("signing public key must be 32 bytes"))?;
    ed25519_dalek::VerifyingKey::from_bytes(&signing)
        .map_err(|_| ApiError::bad_request("signing public key is invalid"))?;
    let upload = RegistrationUpload::<OpaqueSuite>::deserialize(&request.upload)
        .map_err(|_| ApiError::bad_request("malformed registration upload"))?;
    let record = ServerRegistration::<OpaqueSuite>::finish(upload)
        .serialize()
        .to_vec();

    let account_id = uuid::Uuid::new_v4().to_string();
    let id = account_id.clone();
    state
        .db
        .call(move |conn| {
            let inserted = conn.execute(
                "INSERT INTO accounts(id, username, opaque_record, kdf_header, wrapped_keys, \
                 seal_public, signing_public, created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    id,
                    name,
                    record,
                    request.kdf_header,
                    request.wrapped_account_keys,
                    request.seal_public,
                    request.signing_public,
                    now()
                ],
            );
            match inserted {
                Ok(_) => Ok(()),
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                {
                    Err(ApiError::conflict("username is taken"))
                }
                Err(e) => Err(e.into()),
            }
        })
        .await?;
    tracing::info!("account registered");
    Ok(Json(RegisterFinishResponse { account_id }))
}

pub async fn login_start(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<LoginStartRequest>,
) -> Result<Json<LoginStartResponse>, ApiError> {
    let name = normalize(&request.username)?;
    limit_user(&state, &name)?;
    let message = CredentialRequest::deserialize(&request.request)
        .map_err(|_| ApiError::bad_request("malformed credential request"))?;
    let lookup = name.clone();
    let account = state
        .db
        .call(move |conn| {
            Ok(conn
                .query_row(
                    "SELECT id, opaque_record FROM accounts WHERE username = ?1",
                    [&lookup],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )
                .optional()?)
        })
        .await?;
    let (account_id, file) = match account {
        Some((id, record)) => {
            let file = ServerRegistration::<OpaqueSuite>::deserialize(&record)
                .map_err(|_| ApiError::internal("stored OPAQUE record is corrupt"))?;
            (Some(id), Some(file))
        }
        // A dummy record keeps the response indistinguishable.
        None => (None, None),
    };
    let result = ServerLogin::start(
        &mut OsRng,
        &state.opaque,
        file,
        message,
        name.as_bytes(),
        ServerLoginParameters::default(),
    )
    .map_err(|_| ApiError::bad_request("credential request rejected"))?;

    let login_id = uuid::Uuid::new_v4().to_string();
    {
        let mut logins = state.logins.lock().unwrap_or_else(|p| p.into_inner());
        logins.retain(|_, p| p.created.elapsed() < LOGIN_TTL);
        if logins.len() >= 10_000 {
            return Err(ApiError::rate_limited(LOGIN_TTL.as_secs()));
        }
        logins.insert(
            login_id.clone(),
            PendingLogin {
                state: result.state,
                account_id,
                created: Instant::now(),
            },
        );
    }
    Ok(Json(LoginStartResponse {
        login_id,
        response: result.message.serialize().to_vec(),
    }))
}

pub async fn login_finish(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<LoginFinishRequest>,
) -> Result<Json<LoginFinishResponse>, ApiError> {
    let failed = || ApiError::unauthorized("sign-in failed");
    let pending = state
        .logins
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&request.login_id)
        .filter(|p| p.created.elapsed() < LOGIN_TTL)
        .ok_or_else(failed)?;
    let finalization = CredentialFinalization::<OpaqueSuite>::deserialize(&request.finalization)
        .map_err(|_| failed())?;
    pending
        .state
        .finish(finalization, ServerLoginParameters::default())
        .map_err(|_| failed())?;
    let account_id = pending.account_id.ok_or_else(failed)?;

    let mut raw = [0u8; 32];
    OsRng.fill_bytes(&mut raw);
    let token = format!("es_{}", URL_SAFE_NO_PAD.encode(raw));
    let token_hash = hash_token(&token);
    let ttl = i64::from(state.config.session_days) * 86_400;
    let expires_at = now() + ttl;
    let id = account_id.clone();
    let (wrapped, seal, signing) = state
        .db
        .call(move |conn| {
            let t = now();
            conn.execute("DELETE FROM sessions WHERE expires_at <= ?1", [t])?;
            conn.execute(
                "INSERT INTO sessions(token_hash, account_id, created_at, expires_at) \
                 VALUES (?1,?2,?3,?4)",
                params![token_hash, id, t, expires_at],
            )?;
            Ok(conn.query_row(
                "SELECT wrapped_keys, seal_public, signing_public FROM accounts WHERE id = ?1",
                [&id],
                |r| {
                    Ok((
                        r.get::<_, Vec<u8>>(0)?,
                        r.get::<_, Vec<u8>>(1)?,
                        r.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )?)
        })
        .await?;
    tracing::info!("session created");
    Ok(Json(LoginFinishResponse {
        token,
        expires_at,
        account_id,
        wrapped_account_keys: wrapped,
        seal_public: seal,
        signing_public: signing,
    }))
}

pub async fn logout(State(state): State<AppState>, auth: Authed) -> Result<StatusCode, ApiError> {
    state
        .db
        .call(move |conn| {
            conn.execute(
                "DELETE FROM sessions WHERE token_hash = ?1",
                [auth.token_hash],
            )?;
            Ok(())
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn lookup(
    State(state): State<AppState>,
    _auth: Authed,
    Query(query): Query<UsernameQuery>,
) -> Result<Json<AccountInfo>, ApiError> {
    let name = normalize(&query.username)?;
    state
        .db
        .call(move |conn| {
            conn.query_row(
                "SELECT id, username, seal_public, signing_public FROM accounts WHERE username = ?1",
                [&name],
                |r| {
                    Ok(AccountInfo {
                        account_id: r.get(0)?,
                        username: r.get(1)?,
                        seal_public: r.get(2)?,
                        signing_public: r.get(3)?,
                    })
                },
            )
            .optional()?
            .map(Json)
            .ok_or_else(|| ApiError::not_found("no such account"))
        })
        .await
}
