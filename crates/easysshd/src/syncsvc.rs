//! Account, device and vault flows against an `easyssh-server`, and the
//! background sync loop (architecture-v2 section 8).
//!
//! The daemon owns the store, so it owns the account too. What is stored:
//!
//! * the server URL and username in plain text (they are not secret);
//! * the account key pairs, the device signing seed and the session token in
//!   one blob sealed with the vault's device-key cipher
//!   (`sync_account.secrets_enc`);
//! * never the passphrase: it is stretched on this device while signing up
//!   or in, and dropped.
//!
//! While the vault is locked the blob cannot be opened: the state is
//! `locked` and sync resumes when the vault unlocks. Keys that were already
//! open stay in memory, so a vault that locks by itself does not stop sync.

use base64::Engine;
use easyssh_crypto::{
    account_fingerprint, fingerprints_match, open_vault_key, seal_vault_key, AccountKeyPair,
    DeviceCert, DeviceSigningKey, KdfParams, SealPublicKey,
};
use easyssh_store::{personal_vault_server_id, Store, StoreError, StoreSync, VaultKind};
use easyssh_sync::account::{create_vault, enroll_device, login, register, Account};
use easyssh_sync::wire::{device_id_for, ErrorCode, PutMemberRequest, Role};
use easyssh_sync::{
    AuthError, DeviceIdentity, HttpClient, SyncEngine, SyncError, SyncReport, TransportError,
    Verifier,
};
use easyssh_vault::{Vault, VaultError};
use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use tokio::sync::Notify;
use zeroize::Zeroizing;

/// How often the background loop syncs on its own.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(120);
/// How long after a local change the sync starts (changes in a burst share one run).
pub const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(1500);
/// How often the loop looks for writes by other processes.
pub const DEFAULT_POLL: Duration = Duration::from_secs(3);
/// Shortest passphrase for a new account.
pub const MIN_PASSPHRASE_CHARS: usize = 8;

/// Tunables of the sync service; tests use fast ones.
#[derive(Clone)]
pub struct SyncOptions {
    pub kdf: KdfParams,
    pub interval: Duration,
    pub debounce: Duration,
    pub poll: Duration,
}

impl Default for SyncOptions {
    fn default() -> Self {
        Self {
            kdf: KdfParams::DESKTOP_V1,
            interval: DEFAULT_INTERVAL,
            debounce: DEFAULT_DEBOUNCE,
            poll: DEFAULT_POLL,
        }
    }
}

/// An error with a stable code, as the protocol reports it.
#[derive(Debug, Clone)]
pub struct SyncFailure {
    pub code: &'static str,
    pub message: String,
}

pub fn failure(code: &'static str, message: impl Into<String>) -> SyncFailure {
    SyncFailure {
        code,
        message: message.into(),
    }
}

impl From<TransportError> for SyncFailure {
    fn from(error: TransportError) -> Self {
        let code = match &error {
            TransportError::Api { code, .. } => match code {
                ErrorCode::BadRequest => "bad_request",
                ErrorCode::Unauthorized => "unauthorized",
                ErrorCode::Forbidden => "forbidden",
                ErrorCode::NotFound => "not_found",
                ErrorCode::Conflict => "conflict",
                ErrorCode::TooLarge => "too_large",
                ErrorCode::RateLimited => "rate_limited",
                ErrorCode::DeviceNotRegistered => "device_not_registered",
                ErrorCode::DeviceRevoked => "device_revoked",
                ErrorCode::InvalidSignature => "invalid_signature",
                ErrorCode::RegistrationClosed => "registration_closed",
                ErrorCode::Internal => "server_error",
            },
            TransportError::Network(_) => "network_error",
            TransportError::Protocol(_) => "server_error",
        };
        failure(code, error.to_string())
    }
}

impl From<AuthError> for SyncFailure {
    fn from(error: AuthError) -> Self {
        match error {
            AuthError::Opaque => failure(
                "wrong_passphrase",
                "wrong passphrase, or this account does not exist",
            ),
            AuthError::Transport(error) => error.into(),
            other => failure("server_error", other.to_string()),
        }
    }
}

impl From<StoreError> for SyncFailure {
    fn from(error: StoreError) -> Self {
        if matches!(
            error,
            StoreError::Cipher(easyssh_crypto::CryptoError::Locked)
        ) {
            return failure("vault_locked", "the vault is locked");
        }
        failure("store_error", error.to_string())
    }
}

impl From<VaultError> for SyncFailure {
    fn from(error: VaultError) -> Self {
        failure(error.code(), error.to_string())
    }
}

impl From<SyncError> for SyncFailure {
    fn from(error: SyncError) -> Self {
        match error {
            SyncError::Transport(error) => error.into(),
            SyncError::Store(inner) => match inner.downcast::<StoreError>() {
                Ok(store) => (*store).into(),
                Err(other) => failure("store_error", other.to_string()),
            },
            other => failure("sync_error", other.to_string()),
        }
    }
}

/// Accepts `https://` URLs, and `http://` only for a loopback address (the
/// session token is a bearer credential). Returns the URL without a trailing
/// slash.
pub fn validate_server_url(raw: &str) -> Result<String, SyncFailure> {
    let url = raw.trim();
    let bad = |why: &str| Err(failure("invalid_url", why.to_owned()));
    let (scheme, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return bad("the server address must start with https://");
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return bad("the server address needs a host and no user name");
    }
    let host = if let Some(inner) = authority.strip_prefix('[') {
        inner.split(']').next().unwrap_or("")
    } else {
        authority.split(':').next().unwrap_or("")
    };
    if host.is_empty() {
        return bad("the server address needs a host");
    }
    if !scheme && !is_loopback(host) {
        return bad("plain http:// is only allowed for localhost; use https://");
    }
    Ok(url.trim_end_matches('/').to_owned())
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

#[derive(Serialize, Deserialize)]
struct Secrets {
    v: u32,
    account_id: String,
    device_name: String,
    /// X25519 secret then Ed25519 seed, base64.
    keys: String,
    device_seed: String,
    device_cert: String,
    token: String,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn unb64(text: &str) -> Result<Vec<u8>, SyncFailure> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|_| failure("store_error", "the stored account is damaged"))
}

fn seed32(bytes: &[u8]) -> Result<[u8; 32], SyncFailure> {
    bytes
        .try_into()
        .map_err(|_| failure("store_error", "the stored account is damaged"))
}

/// A signed-in account on this device.
struct Session {
    server_url: String,
    username: String,
    device_name: String,
    client: HttpClient,
    account: Account,
    device_seed: [u8; 32],
    device_cert: DeviceCert,
    personal_vault: String,
    device_hex: String,
    fingerprint: String,
}

impl Session {
    fn identity(&self, device_id: &str) -> DeviceIdentity {
        DeviceIdentity {
            device_id: device_id.to_owned(),
            signing_key: DeviceSigningKey::from_seed(self.device_seed),
            cert: self.device_cert.clone(),
        }
    }

    fn secrets(&self) -> Result<Zeroizing<Vec<u8>>, SyncFailure> {
        let keys = self.account.keys.to_seeds();
        let secrets = Secrets {
            v: 1,
            account_id: self.account.account_id.clone(),
            device_name: self.device_name.clone(),
            keys: b64(&*keys),
            device_seed: b64(&self.device_seed),
            device_cert: b64(&self.device_cert.to_bytes()),
            token: self.client.token().unwrap_or_default(),
        };
        serde_json::to_vec(&secrets)
            .map(Zeroizing::new)
            .map_err(|e| failure("internal", e.to_string()))
    }

    fn restore(server_url: &str, username: &str, blob: &[u8]) -> Result<Self, SyncFailure> {
        let secrets: Secrets = serde_json::from_slice(blob)
            .map_err(|_| failure("store_error", "the stored account is damaged"))?;
        let keys = unb64(&secrets.keys)?;
        if keys.len() != 64 {
            return Err(failure("store_error", "the stored account is damaged"));
        }
        let account = Account {
            account_id: secrets.account_id.clone(),
            username: username.to_owned(),
            keys: AccountKeyPair::from_seeds(seed32(&keys[..32])?, seed32(&keys[32..])?),
        };
        let device_seed = seed32(&unb64(&secrets.device_seed)?)?;
        let device_cert = DeviceCert::from_bytes(&unb64(&secrets.device_cert)?)
            .map_err(|_| failure("store_error", "the stored account is damaged"))?;
        let client = HttpClient::new(server_url)?;
        client.set_token(Some(secrets.token).filter(|t| !t.is_empty()));
        Ok(Self::assemble(
            server_url,
            username,
            &secrets.device_name,
            client,
            account,
            device_seed,
            device_cert,
        ))
    }

    fn assemble(
        server_url: &str,
        username: &str,
        device_name: &str,
        client: HttpClient,
        account: Account,
        device_seed: [u8; 32],
        device_cert: DeviceCert,
    ) -> Self {
        let device_hex = device_id_for(&DeviceSigningKey::from_seed(device_seed).public());
        let fingerprint = account_fingerprint(
            &account.keys.sealing.public().0,
            &account.keys.signing.public().to_bytes(),
        );
        Self {
            server_url: server_url.to_owned(),
            username: username.to_owned(),
            device_name: device_name.to_owned(),
            personal_vault: personal_vault_server_id(&account.account_id),
            client,
            account,
            device_seed,
            device_cert,
            device_hex,
            fingerprint,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    SignedOut,
    Idle,
    Syncing,
    Error,
    Locked,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::SignedOut => "signed_out",
            State::Idle => "idle",
            State::Syncing => "syncing",
            State::Error => "error",
            State::Locked => "locked",
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Counts {
    pulled: usize,
    pushed: usize,
    merged: usize,
    resolved: usize,
}

struct Status {
    state: State,
    last_sync_at: Option<chrono::DateTime<chrono::Utc>>,
    error: Option<(String, String)>,
    last: Counts,
}

type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct SyncService {
    store: Arc<Mutex<Store>>,
    vault: Arc<Vault>,
    options: SyncOptions,
    emit: Emit,
    session: Mutex<Option<Arc<Session>>>,
    status: Mutex<Status>,
    /// One sync run at a time.
    run: tokio::sync::Mutex<()>,
    wake: Arc<Notify>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl SyncService {
    /// Builds the service and starts its background loop. The loop ends
    /// when the service is dropped.
    pub fn start(
        store: Arc<Mutex<Store>>,
        vault: Arc<Vault>,
        options: SyncOptions,
        emit: Emit,
    ) -> Arc<Self> {
        let service = Arc::new(Self {
            store,
            vault,
            options,
            emit,
            session: Mutex::new(None),
            status: Mutex::new(Status {
                state: State::SignedOut,
                last_sync_at: None,
                error: None,
                last: Counts::default(),
            }),
            run: tokio::sync::Mutex::new(()),
            wake: Arc::new(Notify::new()),
        });
        service.restore();
        tokio::spawn(background(Arc::downgrade(&service), service.wake.clone()));
        service
    }

    fn with_store<R>(&self, f: impl FnOnce(&Store) -> R) -> R {
        f(&lock(&self.store))
    }

    fn session(&self) -> Option<Arc<Session>> {
        lock(&self.session).clone()
    }

    fn need_session(&self) -> Result<Arc<Session>, SyncFailure> {
        self.session().ok_or_else(|| {
            failure(
                "not_signed_in",
                "sign in to a sync server first (Settings, Sync)",
            )
        })
    }

    /// Asks for a sync soon (local data changed).
    pub fn kick(&self) {
        self.wake.notify_one();
    }

    /// Opens the stored account if there is one. Called at start and after
    /// the vault unlocks.
    pub fn restore(&self) {
        if self.session().is_some() {
            return;
        }
        let row = match self.with_store(|s| s.sync_account()) {
            Ok(Some(row)) => row,
            Ok(None) => return,
            Err(error) => {
                self.set_error(&error.into());
                return;
            }
        };
        let opened = self
            .vault
            .ensure_unlocked()
            .map_err(SyncFailure::from)
            .and_then(|()| {
                self.with_store(|s| s.open_sync_secrets(&row))
                    .map_err(SyncFailure::from)
            })
            .and_then(|blob| {
                Session::restore(&row.server_url, &row.username, &Zeroizing::new(blob))
            });
        match opened {
            Ok(session) => {
                *lock(&self.session) = Some(Arc::new(session));
                let mut status = lock(&self.status);
                status.state = State::Idle;
                status.error = None;
                drop(status);
                self.kick();
            }
            Err(error) if error.code == "vault_locked" => {
                lock(&self.status).state = State::Locked;
            }
            Err(error) => self.set_error(&error),
        }
        self.publish();
    }

    fn set_error(&self, error: &SyncFailure) {
        let mut status = lock(&self.status);
        status.state = if error.code == "vault_locked" {
            State::Locked
        } else {
            State::Error
        };
        status.error = Some((error.code.to_owned(), error.message.clone()));
    }

    fn publish(&self) {
        (self.emit)("sync.state", self.status_json());
    }

    pub fn status_json(&self) -> Value {
        let session = self.session();
        let status = lock(&self.status);
        let account = self.with_store(|s| s.sync_account().ok().flatten());
        let pending = session.as_ref().map_or(0, |session| {
            self.with_store(|s| s.sync_pending(&session.personal_vault).unwrap_or(0))
        });
        // A locked, signed-in account has no open session but is not signed out.
        let signed_in = session.is_some() || account.is_some();
        let state = if !signed_in {
            State::SignedOut
        } else {
            status.state
        };
        json!({
            "signed_in": signed_in,
            "state": state.as_str(),
            "server_url": session.as_ref().map(|s| s.server_url.clone())
                .or_else(|| account.as_ref().map(|a| a.server_url.clone())),
            "username": session.as_ref().map(|s| s.username.clone())
                .or_else(|| account.as_ref().map(|a| a.username.clone())),
            "device_name": session.as_ref().map(|s| s.device_name.clone()),
            "device_id": session.as_ref().map(|s| s.device_hex.clone()),
            "fingerprint": session.as_ref().map(|s| s.fingerprint.clone()),
            "last_sync_at": status.last_sync_at.map(|t| t.to_rfc3339()),
            "last_error": status.error.as_ref().map(|e| e.1.clone()),
            "last_error_code": status.error.as_ref().map(|e| e.0.clone()),
            "pending": pending,
            "last": {
                "pulled": status.last.pulled,
                "pushed": status.last.pushed,
                "merged": status.last.merged,
                "resolved": status.last.resolved,
            },
        })
    }

    // ---- account ---------------------------------------------------------

    fn check_credentials(
        server_url: &str,
        username: &str,
        passphrase: &str,
        device_name: Option<&str>,
        new_account: bool,
    ) -> Result<(String, String, String), SyncFailure> {
        let url = validate_server_url(server_url)?;
        let username = username.trim().to_owned();
        if username.is_empty() || username.chars().count() > 64 {
            return Err(failure(
                "invalid_command",
                "the user name needs 1 to 64 characters",
            ));
        }
        if new_account && passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
            return Err(failure(
                "invalid_passphrase",
                format!("the passphrase needs at least {MIN_PASSPHRASE_CHARS} characters"),
            ));
        }
        if passphrase.is_empty() {
            return Err(failure("invalid_passphrase", "enter the passphrase"));
        }
        let name = device_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(default_device_name);
        let name: String = name.chars().take(48).collect();
        Ok((url, username, name))
    }

    fn ensure_free_to_sign_in(&self, url: &str, username: &str) -> Result<(), SyncFailure> {
        let existing = self.with_store(|s| s.sync_account().ok().flatten());
        match existing {
            Some(row) if row.server_url != url || row.username != username => Err(failure(
                "already_signed_in",
                "sign out of the current account first",
            )),
            _ => Ok(()),
        }
    }

    pub async fn signup(
        &self,
        server_url: &str,
        username: &str,
        passphrase: &str,
        device_name: Option<&str>,
    ) -> Result<Value, SyncFailure> {
        let (url, username, device_name) =
            Self::check_credentials(server_url, username, passphrase, device_name, true)?;
        self.ensure_free_to_sign_in(&url, &username)?;
        self.vault.ensure_unlocked()?;
        let client = HttpClient::new(&url)?;
        register(&client, &username, passphrase.as_bytes(), self.options.kdf).await?;
        let account = login(&client, &username, passphrase.as_bytes()).await?;
        let session = self
            .enroll(&url, &username, &device_name, client, account)
            .await?;
        // The personal vault: a random key sealed to the account key.
        let personal = session.personal_vault.clone();
        create_vault(&session.client, &session.account, &personal).await?;
        self.finish_sign_in(session).await
    }

    pub async fn signin(
        &self,
        server_url: &str,
        username: &str,
        passphrase: &str,
        device_name: Option<&str>,
    ) -> Result<Value, SyncFailure> {
        let (url, username, device_name) =
            Self::check_credentials(server_url, username, passphrase, device_name, false)?;
        self.ensure_free_to_sign_in(&url, &username)?;
        self.vault.ensure_unlocked()?;
        let client = HttpClient::new(&url)?;
        let account = login(&client, &username, passphrase.as_bytes()).await?;
        let session = self
            .enroll(&url, &username, &device_name, client, account)
            .await?;
        // An account created elsewhere may not have its personal vault yet
        // (it is created with the first sign-up, so this is only a safety net).
        let vaults = session.client.list_vaults().await?;
        if !vaults.iter().any(|v| v.vault_id == session.personal_vault) {
            create_vault(&session.client, &session.account, &session.personal_vault).await?;
        }
        self.finish_sign_in(session).await
    }

    /// A fresh device key, certified by the account key and registered.
    async fn enroll(
        &self,
        url: &str,
        username: &str,
        device_name: &str,
        client: HttpClient,
        account: Account,
    ) -> Result<Session, SyncFailure> {
        let mut seed = [0u8; 32];
        fill_random(&mut seed);
        let key = DeviceSigningKey::from_seed(seed);
        let identity = enroll_device(
            &client,
            &account,
            device_name,
            key,
            chrono::Utc::now().timestamp(),
        )
        .await?;
        Ok(Session::assemble(
            url,
            username,
            device_name,
            client,
            account,
            seed,
            identity.cert,
        ))
    }

    async fn finish_sign_in(&self, session: Session) -> Result<Value, SyncFailure> {
        let blob = session.secrets()?;
        self.with_store(|s| s.set_sync_account(&session.server_url, &session.username, &blob))?;
        *lock(&self.session) = Some(Arc::new(session));
        {
            let mut status = lock(&self.status);
            status.state = State::Idle;
            status.error = None;
        }
        // The first sync is part of signing in; its failure shows in the
        // status, it does not undo the sign-in.
        self.run_once().await;
        Ok(self.status_json())
    }

    pub async fn signout(&self) -> Result<Value, SyncFailure> {
        let _run = self.run.lock().await;
        if let Some(session) = self.session() {
            // Best effort: the server also expires sessions on its own.
            let _ = session.client.logout().await;
        }
        *lock(&self.session) = None;
        self.with_store(|s| s.clear_sync())?;
        {
            let mut status = lock(&self.status);
            status.state = State::SignedOut;
            status.error = None;
            status.last_sync_at = None;
            status.last = Counts::default();
        }
        self.publish();
        Ok(self.status_json())
    }

    pub async fn devices_list(&self) -> Result<Value, SyncFailure> {
        let session = self.need_session()?;
        let devices = session.client.list_devices().await?;
        let list: Vec<Value> = devices
            .into_iter()
            .map(|d| {
                json!({
                    "device_id": d.device_id,
                    "name": d.name,
                    "created_at": d.created_at.max(0),
                    "revoked": d.revoked,
                    "this_device": d.device_id == session.device_hex,
                })
            })
            .collect();
        Ok(json!({"devices": list}))
    }

    pub async fn devices_revoke(&self, device_id: &str) -> Result<(), SyncFailure> {
        let session = self.need_session()?;
        if device_id == session.device_hex {
            return Err(failure(
                "invalid_command",
                "this device cannot revoke itself; sign out instead",
            ));
        }
        session.client.revoke_device(device_id).await?;
        Ok(())
    }

    // ---- vaults ----------------------------------------------------------

    pub fn vaults_list(&self) -> Result<Value, SyncFailure> {
        let vaults = self.with_store(|s| s.list_vaults())?;
        let list: Vec<Value> = vaults
            .into_iter()
            .map(|v| {
                json!({
                    "id": v.id,
                    "name": v.name,
                    "kind": match v.kind { VaultKind::Personal => "personal", VaultKind::Team => "team" },
                    "role": v.role,
                })
            })
            .collect();
        Ok(json!({"vaults": list}))
    }

    pub async fn create_shared(&self, name: &str) -> Result<Value, SyncFailure> {
        let session = self.need_session()?;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(failure(
                "invalid_command",
                "the vault name needs 1 to 64 characters",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        create_vault(&session.client, &session.account, &id).await?;
        self.with_store(|s| s.create_team_vault(&id, name))?;
        self.run_once().await;
        Ok(json!({"id": id, "name": name, "kind": "team", "role": "owner"}))
    }

    fn fingerprint_of(seal: &[u8], signing: &[u8]) -> Result<String, SyncFailure> {
        let seal: [u8; 32] = seal
            .try_into()
            .map_err(|_| failure("server_error", "the server sent a bad public key"))?;
        let signing: [u8; 32] = signing
            .try_into()
            .map_err(|_| failure("server_error", "the server sent a bad public key"))?;
        Ok(account_fingerprint(&seal, &signing))
    }

    pub async fn members_list(&self, vault_id: &str) -> Result<Value, SyncFailure> {
        let session = self.need_session()?;
        let members = session.client.members(vault_id).await?;
        let mut out = Vec::new();
        for member in members {
            let fingerprint = Self::fingerprint_of(&member.seal_public, &member.signing_public)?;
            let trust = if member.account_id == session.account.account_id {
                "self"
            } else {
                match self.with_store(|s| s.sync_pin(&member.account_id))? {
                    Some(pin) if fingerprints_match(&pin.fingerprint, &fingerprint) => {
                        if pin.confirmed {
                            "confirmed"
                        } else {
                            "unconfirmed"
                        }
                    }
                    Some(_) => "changed",
                    None => "unconfirmed",
                }
            };
            out.push(json!({
                "account_id": member.account_id,
                "username": member.username,
                "role": member.role.as_str(),
                "fingerprint": fingerprint,
                "trust": trust,
            }));
        }
        Ok(json!({"members": out}))
    }

    pub async fn members_lookup(&self, username: &str) -> Result<Value, SyncFailure> {
        let session = self.need_session()?;
        let info = session.client.lookup_account(username.trim()).await?;
        let fingerprint = Self::fingerprint_of(&info.seal_public, &info.signing_public)?;
        Ok(json!({
            "account_id": info.account_id,
            "username": info.username,
            "fingerprint": fingerprint,
        }))
    }

    pub async fn members_add(
        &self,
        vault_id: &str,
        username: &str,
        role: Role,
        fingerprint: &str,
    ) -> Result<Value, SyncFailure> {
        let session = self.need_session()?;
        let info = session.client.lookup_account(username.trim()).await?;
        let actual = Self::fingerprint_of(&info.seal_public, &info.signing_public)?;
        if !fingerprints_match(fingerprint, &actual) {
            return Err(failure(
                "fingerprint_mismatch",
                "the fingerprint does not match the keys the server has for this account",
            ));
        }
        if let Some(pin) = self.with_store(|s| s.sync_pin(&info.account_id))? {
            if pin.confirmed && !fingerprints_match(&pin.fingerprint, &actual) {
                return Err(failure(
                    "fingerprint_mismatch",
                    "this account's keys changed since you confirmed them",
                ));
            }
        }
        // The vault key, opened with our own account key.
        let vaults = session.client.list_vaults().await?;
        let summary = vaults
            .iter()
            .find(|v| v.vault_id == vault_id)
            .ok_or_else(|| failure("not_found", "no such vault"))?;
        let vault_key = open_vault_key(&summary.sealed_key, &session.account.keys.sealing)
            .map_err(|e| failure("crypto_error", e.to_string()))?;
        let public: [u8; 32] = info
            .seal_public
            .as_slice()
            .try_into()
            .map_err(|_| failure("server_error", "the server sent a bad public key"))?;
        let sealed = seal_vault_key(&vault_key, &SealPublicKey(public))
            .map_err(|e| failure("crypto_error", e.to_string()))?;
        session
            .client
            .put_member(
                vault_id,
                &info.account_id,
                &PutMemberRequest {
                    role,
                    sealed_key: Some(sealed),
                },
            )
            .await?;
        self.with_store(|s| s.set_sync_pin(&info.account_id, &info.username, &actual, true))?;
        Ok(json!({
            "account_id": info.account_id,
            "username": info.username,
            "role": role.as_str(),
            "fingerprint": actual,
            "trust": "confirmed",
        }))
    }

    pub async fn members_remove(
        &self,
        vault_id: &str,
        account_id: &str,
    ) -> Result<(), SyncFailure> {
        let session = self.need_session()?;
        session.client.remove_member(vault_id, account_id).await?;
        Ok(())
    }

    pub async fn members_role(
        &self,
        vault_id: &str,
        account_id: &str,
        role: Role,
    ) -> Result<(), SyncFailure> {
        let session = self.need_session()?;
        session
            .client
            .put_member(
                vault_id,
                account_id,
                &PutMemberRequest {
                    role,
                    sealed_key: None,
                },
            )
            .await?;
        Ok(())
    }

    // ---- syncing ---------------------------------------------------------

    /// One sync run now; the result is in [`SyncService::status_json`].
    pub async fn sync_now(&self) -> Value {
        self.restore();
        self.run_once().await;
        self.status_json()
    }

    async fn run_once(&self) {
        let _run = self.run.lock().await;
        let Some(session) = self.session() else {
            return;
        };
        lock(&self.status).state = State::Syncing;
        self.publish();
        let outcome = self.sync_session(&session).await;
        let mut changed = false;
        {
            let mut status = lock(&self.status);
            match outcome {
                Ok(counts) => {
                    changed = counts.pulled > 0 || counts.merged > 0;
                    status.state = State::Idle;
                    status.error = None;
                    status.last_sync_at = Some(chrono::Utc::now());
                    status.last = counts;
                }
                Err(error) => {
                    status.state = if error.code == "vault_locked" {
                        State::Locked
                    } else {
                        State::Error
                    };
                    status.error = Some((error.code.to_owned(), error.message));
                }
            }
        }
        if changed {
            (self.emit)("hosts.changed", json!({}));
        }
        self.publish();
    }

    async fn sync_session(&self, session: &Session) -> Result<Counts, SyncFailure> {
        let device_id = self.with_store(|s| s.device_id().to_owned());
        let high_water = self.with_store(|s| s.load_high_water())?;
        let verifier =
            Verifier::new(session.account.keys.signing.public()).with_high_water(high_water);
        let mut engine = SyncEngine::new(
            session.client.clone(),
            StoreSync::new(self.store.clone(), session.personal_vault.clone()),
            session.identity(&device_id),
            verifier,
        );

        let mut vaults = session.client.list_vaults().await?;
        // The personal vault first, then the shared ones.
        vaults.sort_by_key(|v| v.vault_id != session.personal_vault);
        for vault in &vaults {
            let key = open_vault_key(&vault.sealed_key, &session.account.keys.sealing)
                .map_err(|e| failure("crypto_error", e.to_string()))?;
            engine.add_vault_key(&vault.vault_id, key);
            if vault.vault_id != session.personal_vault {
                self.with_store(|s| {
                    s.ensure_team_vault(&vault.vault_id, "Shared vault", vault.role.as_str())
                })?;
                self.trust_members(session, &mut engine, &vault.vault_id)
                    .await?;
            }
        }

        let mut total = SyncReport::default();
        let mut failed: Option<SyncFailure> = None;
        for vault in &vaults {
            let result = if vault.role.can_write() {
                engine.sync(&vault.vault_id).await
            } else {
                engine.pull(&vault.vault_id).await
            };
            match result {
                Ok(report) => {
                    total.pulled += report.pulled;
                    total.pushed += report.pushed;
                    total.merged += report.merged;
                    total.overwritten += report.overwritten;
                    total.rejected.extend(report.rejected);
                }
                Err(error) => {
                    failed = Some(error.into());
                    break;
                }
            }
        }
        // What was verified so far stays verified, even after an error.
        let saved = self.with_store(|s| {
            s.save_high_water(engine.verifier().high_water())?;
            s.sync_repair_references()
        });
        if let Some(error) = failed {
            return Err(error);
        }
        saved?;
        Ok(Counts {
            pulled: total.pulled,
            pushed: total.pushed,
            merged: total.merged,
            resolved: total.overwritten,
        })
    }

    /// Trust the signing keys of the other members of a shared vault, but
    /// only while their fingerprint is the one pinned (or first seen).
    async fn trust_members(
        &self,
        session: &Session,
        engine: &mut SyncEngine<HttpClient, StoreSync>,
        vault_id: &str,
    ) -> Result<(), SyncFailure> {
        for member in session.client.members(vault_id).await? {
            if member.account_id == session.account.account_id {
                continue;
            }
            let fingerprint = Self::fingerprint_of(&member.seal_public, &member.signing_public)?;
            let pinned = self.with_store(|s| s.sync_pin(&member.account_id))?;
            match pinned {
                Some(pin) if !fingerprints_match(&pin.fingerprint, &fingerprint) => continue,
                Some(_) => {}
                None => self.with_store(|s| {
                    s.set_sync_pin(&member.account_id, &member.username, &fingerprint, false)
                })?,
            }
            let bytes: [u8; 32] = member
                .signing_public
                .as_slice()
                .try_into()
                .map_err(|_| failure("server_error", "the server sent a bad public key"))?;
            if let Ok(key) = VerifyingKey::from_bytes(&bytes) {
                engine.verifier_mut().trust_account(key);
            }
        }
        Ok(())
    }
}

fn fill_random(buf: &mut [u8]) {
    use rand::RngCore;
    rand::rngs::OsRng.fill_bytes(buf);
}

fn default_device_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "EasySSH device".to_owned())
}

/// The sync loop: on start, every interval, after a local change
/// (debounced) and when another process wrote to the database.
async fn background(service: Weak<SyncService>, wake: Arc<Notify>) {
    let (interval, debounce, poll) = match service.upgrade() {
        Some(s) => (s.options.interval, s.options.debounce, s.options.poll),
        None => return,
    };
    let mut every = tokio::time::interval(interval);
    let mut look = tokio::time::interval(poll);
    let mut seen = service
        .upgrade()
        .and_then(|s| s.with_store(|st| st.data_version().ok()));
    loop {
        let debounced = tokio::select! {
            _ = every.tick() => false,
            _ = wake.notified() => true,
            _ = look.tick() => {
                let Some(s) = service.upgrade() else { return };
                let now = s.with_store(|st| st.data_version().ok());
                if now == seen {
                    continue;
                }
                true
            }
        };
        if debounced {
            tokio::time::sleep(debounce).await;
        }
        let Some(s) = service.upgrade() else { return };
        s.restore();
        s.run_once().await;
        // Our own run changes nothing this connection sees, but a write by
        // another process during it must not be missed.
        seen = s.with_store(|st| st.data_version().ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_urls_must_be_https_except_loopback() {
        assert_eq!(
            validate_server_url(" https://sync.example.org/ ").unwrap(),
            "https://sync.example.org"
        );
        assert!(validate_server_url("https://sync.example.org:8443/base").is_ok());
        for ok in [
            "http://localhost:8787",
            "http://127.0.0.1:8787",
            "http://127.1.2.3",
            "http://[::1]:8787",
        ] {
            assert!(validate_server_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://sync.example.org",
            "http://10.0.0.5:8787",
            "http://localhost.evil.org",
            "ftp://x",
            "sync.example.org",
            "https://",
            "https://user:pw@sync.example.org",
            "http://127.0.0.1@evil.org",
            "",
        ] {
            let error = validate_server_url(bad).unwrap_err();
            assert_eq!(error.code, "invalid_url", "{bad}");
        }
    }

    #[test]
    fn secrets_round_trip_without_the_passphrase() {
        let keys = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let account = Account {
            account_id: "acct".into(),
            username: "ann".into(),
            keys,
        };
        let key = DeviceSigningKey::from_seed([3; 32]);
        let cert = easyssh_crypto::certify_device(&account.keys.signing, &key.public(), "mac", 5);
        let client = HttpClient::new("http://127.0.0.1:1").unwrap();
        client.set_token(Some("TOKEN".into()));
        let session = Session::assemble(
            "http://127.0.0.1:1",
            "ann",
            "mac",
            client,
            account,
            [3; 32],
            cert,
        );
        let blob = session.secrets().unwrap();
        let text = String::from_utf8_lossy(&blob).into_owned();
        assert!(!text.contains("passphrase"));
        let back = Session::restore("http://127.0.0.1:1", "ann", &blob).unwrap();
        assert_eq!(back.account.account_id, "acct");
        assert_eq!(back.fingerprint, session.fingerprint);
        assert_eq!(back.device_hex, session.device_hex);
        assert_eq!(back.client.token().as_deref(), Some("TOKEN"));
    }
}
