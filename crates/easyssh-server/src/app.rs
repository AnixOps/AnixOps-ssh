//! Shared state, request authentication, middleware and the router.

use crate::config::Config;
use crate::db::{meta_blob, now, Db};
use crate::error::ApiError;
use crate::ratelimit::RateLimiter;
use crate::{accounts, devices, records, vaults};
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, MatchedPath, Request, State};
use axum::http::header;
use axum::http::request::Parts;
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::Router;
use easyssh_sync::auth::OpaqueSuite;
use http_body_util::BodyExt;
use opaque_ke::{ServerLogin, ServerSetup};
use rand_core::{OsRng, RngCore};
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a login handshake may take between `start` and `finish`.
pub const LOGIN_TTL: Duration = Duration::from_secs(60);

pub struct PendingLogin {
    pub state: ServerLogin<OpaqueSuite>,
    /// `None` for an unknown username (the handshake then always fails).
    pub account_id: Option<String>,
    pub created: Instant,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub config: Arc<Config>,
    pub opaque: Arc<ServerSetup<OpaqueSuite>>,
    /// Derives the fake KDF header shown for unknown usernames.
    pub fake_kdf_key: Arc<[u8; 32]>,
    pub logins: Arc<Mutex<HashMap<String, PendingLogin>>>,
    pub limiter: Arc<RateLimiter>,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self, ApiError> {
        let db = Db::open(&config.db)
            .map_err(|e| ApiError::internal(&format!("cannot open database: {e}")))?;
        let (setup, fake) = db
            .call(|conn| {
                let setup = meta_blob(conn, "opaque_setup", || {
                    ServerSetup::<OpaqueSuite>::new(&mut OsRng)
                        .serialize()
                        .to_vec()
                })?;
                let fake = meta_blob(conn, "fake_kdf_key", || {
                    let mut key = [0u8; 32];
                    OsRng.fill_bytes(&mut key);
                    key.to_vec()
                })?;
                Ok((setup, fake))
            })
            .await?;
        let opaque = ServerSetup::<OpaqueSuite>::deserialize(&setup)
            .map_err(|_| ApiError::internal("stored OPAQUE setup is corrupt"))?;
        let fake_kdf_key: [u8; 32] = fake
            .as_slice()
            .try_into()
            .map_err(|_| ApiError::internal("stored key is corrupt"))?;
        Ok(Self {
            db,
            config: Arc::new(config),
            opaque: Arc::new(opaque),
            fake_kdf_key: Arc::new(fake_kdf_key),
            logins: Arc::new(Mutex::new(HashMap::new())),
            limiter: Arc::new(RateLimiter::new(Duration::from_secs(60))),
        })
    }
}

pub fn hash_token(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The signed-in account, from a valid unexpired session token.
pub struct Authed {
    pub account_id: String,
    pub token_hash: Vec<u8>,
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| ApiError::unauthorized("missing bearer token"))?
            .to_owned();
        let token_hash = hash_token(&token);
        let lookup = token_hash.clone();
        let account_id = state
            .db
            .call(move |conn| {
                Ok(conn
                    .query_row(
                        "SELECT account_id FROM sessions WHERE token_hash = ?1 AND expires_at > ?2",
                        rusqlite::params![lookup, now()],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()?)
            })
            .await?
            .ok_or_else(|| ApiError::unauthorized("invalid or expired session"))?;
        Ok(Authed {
            account_id,
            token_hash,
        })
    }
}

fn client_key(request: &Request, trust_proxy: bool) -> String {
    if trust_proxy {
        if let Some(first) = request
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
        {
            return first.trim().to_owned();
        }
    }
    request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "unknown".to_owned(), |c| c.0.ip().to_string())
}

/// Per-address limit on the unauthenticated auth endpoints.
async fn limit_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let key = format!("ip:{}", client_key(&request, state.config.trust_proxy));
    state
        .limiter
        .check(&key, state.config.auth_rate_per_minute)
        .map_err(ApiError::rate_limited)?;
    Ok(next.run(request).await)
}

/// One structured log line per request: method, route pattern, status and
/// latency. Headers, bodies, tokens and concrete ids never appear.
async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |p| p.as_str().to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    tracing::info!(
        method = %method,
        route = %route,
        status = response.status().as_u16(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "request"
    );
    response
}

/// Request body limits: for everything, and for key rotation.
#[derive(Clone, Copy)]
struct BodyLimits {
    normal: usize,
    rotate: usize,
}

/// Response header carrying the largest request body the server accepts, in
/// bytes. Clients may refuse locally before sending more.
pub const MAX_BODY_HEADER: &str = easyssh_sync::http::MAX_BODY_HEADER;

/// How much of an oversized body is read and thrown away before answering
/// 413, as a multiple of the limit. Bigger announced bodies are refused at
/// once; their sender may see the connection reset instead of the 413.
const DRAIN_FACTOR: usize = 4;

/// How long an oversized body may take to arrive while being discarded.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Announces the body limit on every response and answers an oversized
/// request with a clean 413.
///
/// Answering without reading the body makes hyper close the connection while
/// the client is still writing, and the client can then see a reset (macOS
/// does) instead of the response. So a body that announces up to
/// `DRAIN_FACTOR` times the limit is read and discarded first, frame by
/// frame and never stored, which bounds the memory it costs (none) and the
/// bandwidth (`DRAIN_FACTOR` times the limit) a refused request can use.
async fn enforce_body_limit(
    State(limits): State<BodyLimits>,
    request: Request,
    next: Next,
) -> Response {
    // Key rotation carries every record of a vault and has its own limit; the
    // announced header stays the normal one.
    let max = if request.uri().path().ends_with("/rotate") {
        limits.rotate
    } else {
        limits.normal
    };
    let announced = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    let mut response = match announced {
        Some(len) if len > max => {
            if len <= max.saturating_mul(DRAIN_FACTOR) {
                let mut body = request.into_body();
                let _ = tokio::time::timeout(DRAIN_TIMEOUT, async {
                    while let Some(Ok(_frame)) = body.frame().await {}
                })
                .await;
            }
            ApiError::too_large("request body too large").into_response()
        }
        _ => next.run(request).await,
    };
    if let Ok(value) = header::HeaderValue::from_str(&limits.normal.to_string()) {
        response.headers_mut().insert(MAX_BODY_HEADER, value);
    }
    response
}

async fn healthz() -> &'static str {
    "ok"
}

pub fn router(state: AppState) -> Router {
    let auth = Router::new()
        .route("/v1/auth/kdf", get(accounts::kdf_header))
        .route("/v1/auth/register/start", post(accounts::register_start))
        .route("/v1/auth/register/finish", post(accounts::register_finish))
        .route("/v1/auth/login/start", post(accounts::login_start))
        .route("/v1/auth/login/finish", post(accounts::login_finish))
        .layer(from_fn_with_state(state.clone(), limit_auth));

    let api = Router::new()
        .route("/v1/auth/logout", post(accounts::logout))
        .route("/v1/accounts/lookup", get(accounts::lookup))
        .route("/v1/devices", post(devices::register).get(devices::list))
        .route("/v1/devices/{device_id}", delete(devices::revoke))
        .route("/v1/vaults", post(vaults::create).get(vaults::list))
        .route("/v1/vaults/{vault_id}/members", get(vaults::members))
        .route(
            "/v1/vaults/{vault_id}/members/{account_id}",
            put(vaults::put_member),
        )
        .route(
            "/v1/vaults/{vault_id}/rotate",
            post(vaults::rotate).layer(DefaultBodyLimit::max(state.config.max_rotate_body_bytes)),
        )
        .route(
            "/v1/vaults/{vault_id}/revoked-devices",
            get(vaults::revoked_devices),
        )
        .route(
            "/v1/vaults/{vault_id}/records",
            post(records::push).get(records::pull),
        )
        .route(
            "/v1/vaults/{vault_id}/records/batch",
            post(records::push_batch),
        )
        .route(
            "/v1/vaults/{vault_id}/records/{record_id}/history",
            get(records::history),
        );

    Router::new()
        .route("/healthz", get(healthz))
        .merge(auth)
        .merge(api)
        .layer(DefaultBodyLimit::max(state.config.max_body_bytes))
        .layer(from_fn_with_state(
            BodyLimits {
                normal: state.config.max_body_bytes,
                rotate: state.config.max_rotate_body_bytes,
            },
            enforce_body_limit,
        ))
        .layer(from_fn(log_request))
        .with_state(state)
}
