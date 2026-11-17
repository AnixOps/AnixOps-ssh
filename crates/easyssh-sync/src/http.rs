//! HTTP client for `easyssh-server` (reqwest + rustls).

use crate::error::TransportError;
use crate::transport::Transport;
use crate::wire::*;
use reqwest::{Method, Url};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Response header with the largest request body the server accepts.
pub const MAX_BODY_HEADER: &str = "x-easyssh-max-body";

/// No limit learned yet.
const NO_LIMIT: usize = usize::MAX;

/// Talks to one server. Cloning shares the session token.
#[derive(Clone)]
pub struct HttpClient {
    base: Url,
    http: reqwest::Client,
    token: Arc<RwLock<Option<String>>>,
    /// The server's request body limit, from its `x-easyssh-max-body`
    /// header on an earlier response; shared by clones.
    max_body: Arc<AtomicUsize>,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("base", &self.base.as_str())
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    /// `base_url` is the server root, for example `https://sync.example.com`.
    /// Use `https` outside tests: the session token is a bearer credential.
    pub fn new(base_url: &str) -> Result<Self, TransportError> {
        let base = Url::parse(base_url).map_err(|e| TransportError::Network(e.to_string()))?;
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("easyssh-sync/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| TransportError::Network(e.to_string()))?;
        Ok(Self {
            base,
            http,
            token: Arc::new(RwLock::new(None)),
            max_body: Arc::new(AtomicUsize::new(NO_LIMIT)),
        })
    }

    pub fn set_token(&self, token: Option<String>) {
        *self.token.write().expect("token lock") = token;
    }

    /// The current session token (a bearer credential: handle with care).
    pub fn token(&self) -> Option<String> {
        self.token.read().expect("token lock").clone()
    }

    pub fn has_token(&self) -> bool {
        self.token.read().expect("token lock").is_some()
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .expect("http(s) URL has path segments")
            .pop_if_empty()
            .extend(segments);
        url
    }

    async fn raw(
        &self,
        method: Method,
        url: Url,
        body: Option<serde_json::Value>,
    ) -> Result<(u16, Vec<u8>), TransportError> {
        let mut request = self.http.request(method, url);
        if let Some(token) = self.token.read().expect("token lock").as_deref() {
            request = request.bearer_auth(token);
        }
        if let Some(body) = body {
            let bytes = serde_json::to_vec(&body).expect("a JSON value serializes");
            // A server that answers an oversized body before it has been
            // sent may reset the connection mid-upload, and the client then
            // never sees its 413. Once the server has said what it accepts,
            // refuse here with the same error instead.
            let max = self.max_body.load(Ordering::Relaxed);
            if bytes.len() > max {
                return Err(TransportError::Api {
                    status: 413,
                    code: ErrorCode::TooLarge,
                    message: format!(
                        "request body of {} bytes exceeds the server limit of {max} bytes",
                        bytes.len()
                    ),
                });
            }
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(bytes);
        }
        let response = request
            .send()
            .await
            .map_err(|e| TransportError::Network(e.without_url().to_string()))?;
        let status = response.status().as_u16();
        if let Some(max) = response
            .headers()
            .get(MAX_BODY_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
        {
            self.max_body.store(max, Ordering::Relaxed);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| TransportError::Network(e.without_url().to_string()))?;
        Ok((status, bytes.to_vec()))
    }

    fn api_error(status: u16, bytes: &[u8]) -> TransportError {
        match serde_json::from_slice::<ErrorBody>(bytes) {
            Ok(body) => TransportError::Api {
                status,
                code: body.code,
                message: body.message,
            },
            Err(_) => TransportError::Protocol(format!("HTTP {status} without an error body")),
        }
    }

    async fn call<R: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<serde_json::Value>,
    ) -> Result<R, TransportError> {
        let (status, bytes) = self.raw(method, url, body).await?;
        if !(200..300).contains(&status) {
            return Err(Self::api_error(status, &bytes));
        }
        serde_json::from_slice(&bytes).map_err(|e| TransportError::Protocol(e.to_string()))
    }

    async fn call_empty(
        &self,
        method: Method,
        url: Url,
        body: Option<serde_json::Value>,
    ) -> Result<(), TransportError> {
        let (status, bytes) = self.raw(method, url, body).await?;
        if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(Self::api_error(status, &bytes))
        }
    }

    fn json<B: Serialize>(body: &B) -> Option<serde_json::Value> {
        Some(serde_json::to_value(body).expect("wire types serialize"))
    }

    // ---- accounts ----

    pub async fn kdf_header(&self, username: &str) -> Result<KdfHeaderResponse, TransportError> {
        let mut url = self.url(&["v1", "auth", "kdf"]);
        url.query_pairs_mut().append_pair("username", username);
        self.call(Method::GET, url, None).await
    }

    pub async fn register_start(
        &self,
        request: &RegisterStartRequest,
    ) -> Result<RegisterStartResponse, TransportError> {
        let url = self.url(&["v1", "auth", "register", "start"]);
        self.call(Method::POST, url, Self::json(request)).await
    }

    pub async fn register_finish(
        &self,
        request: &RegisterFinishRequest,
    ) -> Result<RegisterFinishResponse, TransportError> {
        let url = self.url(&["v1", "auth", "register", "finish"]);
        self.call(Method::POST, url, Self::json(request)).await
    }

    pub async fn login_start(
        &self,
        request: &LoginStartRequest,
    ) -> Result<LoginStartResponse, TransportError> {
        let url = self.url(&["v1", "auth", "login", "start"]);
        self.call(Method::POST, url, Self::json(request)).await
    }

    /// On success the returned token is installed for later calls.
    pub async fn login_finish(
        &self,
        request: &LoginFinishRequest,
    ) -> Result<LoginFinishResponse, TransportError> {
        let url = self.url(&["v1", "auth", "login", "finish"]);
        let response: LoginFinishResponse =
            self.call(Method::POST, url, Self::json(request)).await?;
        self.set_token(Some(response.token.clone()));
        Ok(response)
    }

    /// Ends the current session on the server and forgets the token.
    pub async fn logout(&self) -> Result<(), TransportError> {
        let url = self.url(&["v1", "auth", "logout"]);
        let result = self.call_empty(Method::POST, url, None).await;
        self.set_token(None);
        result
    }

    pub async fn lookup_account(&self, username: &str) -> Result<AccountInfo, TransportError> {
        let mut url = self.url(&["v1", "accounts", "lookup"]);
        url.query_pairs_mut().append_pair("username", username);
        self.call(Method::GET, url, None).await
    }

    // ---- devices ----

    pub async fn register_device(
        &self,
        request: &RegisterDeviceRequest,
    ) -> Result<DeviceInfo, TransportError> {
        let url = self.url(&["v1", "devices"]);
        self.call(Method::POST, url, Self::json(request)).await
    }

    pub async fn list_devices(&self) -> Result<Vec<DeviceInfo>, TransportError> {
        let url = self.url(&["v1", "devices"]);
        self.call(Method::GET, url, None).await
    }

    pub async fn revoke_device(&self, device_id: &str) -> Result<(), TransportError> {
        let url = self.url(&["v1", "devices", device_id]);
        self.call_empty(Method::DELETE, url, None).await
    }

    // ---- vaults ----

    pub async fn create_vault(&self, request: &CreateVaultRequest) -> Result<(), TransportError> {
        let url = self.url(&["v1", "vaults"]);
        self.call_empty(Method::POST, url, Self::json(request))
            .await
    }

    pub async fn list_vaults(&self) -> Result<Vec<VaultSummary>, TransportError> {
        let url = self.url(&["v1", "vaults"]);
        self.call(Method::GET, url, None).await
    }

    pub async fn members(&self, vault_id: &str) -> Result<Vec<MemberInfo>, TransportError> {
        let url = self.url(&["v1", "vaults", vault_id, "members"]);
        self.call(Method::GET, url, None).await
    }

    pub async fn put_member(
        &self,
        vault_id: &str,
        account_id: &str,
        request: &PutMemberRequest,
    ) -> Result<(), TransportError> {
        let url = self.url(&["v1", "vaults", vault_id, "members", account_id]);
        self.call_empty(Method::PUT, url, Self::json(request)).await
    }

    pub async fn remove_member(
        &self,
        vault_id: &str,
        account_id: &str,
    ) -> Result<(), TransportError> {
        let url = self.url(&["v1", "vaults", vault_id, "members", account_id]);
        self.call_empty(Method::DELETE, url, None).await
    }
}

impl Transport for HttpClient {
    async fn push(
        &self,
        vault_id: &str,
        request: &PushRequest,
    ) -> Result<PushResponse, TransportError> {
        let url = self.url(&["v1", "vaults", vault_id, "records"]);
        let (status, bytes) = self.raw(Method::POST, url, Self::json(request)).await?;
        match status {
            200 | 409 => {
                serde_json::from_slice(&bytes).map_err(|e| TransportError::Protocol(e.to_string()))
            }
            _ => Err(Self::api_error(status, &bytes)),
        }
    }

    async fn pull(
        &self,
        vault_id: &str,
        since: u64,
        limit: u32,
    ) -> Result<PullResponse, TransportError> {
        let mut url = self.url(&["v1", "vaults", vault_id, "records"]);
        url.query_pairs_mut()
            .append_pair("since", &since.to_string())
            .append_pair("limit", &limit.to_string());
        self.call(Method::GET, url, None).await
    }

    async fn history(
        &self,
        vault_id: &str,
        record_id: &str,
    ) -> Result<HistoryResponse, TransportError> {
        let url = self.url(&["v1", "vaults", vault_id, "records", record_id, "history"]);
        self.call(Method::GET, url, None).await
    }
}
