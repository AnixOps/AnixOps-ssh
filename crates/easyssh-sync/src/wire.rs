//! JSON wire protocol between sync clients and `easyssh-server`.
//!
//! Binary values travel as standard base64 strings. Everything here is
//! routing metadata or ciphertext; the server never sees a plaintext field.
//! The crate README documents every endpoint.

use crate::error::WireError;
use easyssh_crypto::{DeviceCert, Envelope as SignedEnvelope, VersionVector};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Serde adapter: `Vec<u8>` as a base64 string.
pub mod b64 {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

/// Device ids are the lowercase hex of the device's Ed25519 public key.
pub fn device_id_for(device_public: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in device_public {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Parse a hex device id back into a public key.
pub fn device_public_from_id(id: &str) -> Option<[u8; 32]> {
    if id.len() != 64 || !id.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&id[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// A signed record version as stored and relayed by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    pub record_id: String,
    pub vault_id: String,
    /// Entity table (`hosts`, `keys`, ...). Covered by the signature; it is
    /// also repeated inside the ciphertext and clients reject a mismatch.
    pub table: String,
    /// Generation of the vault key the ciphertext is encrypted under
    /// (signed). The server accepts writes only under the current one.
    pub key_generation: u32,
    pub version_vector: BTreeMap<String, u64>,
    pub deleted: bool,
    #[serde(with = "b64")]
    pub ciphertext: Vec<u8>,
    /// Signing device public key (32 bytes).
    #[serde(with = "b64")]
    pub signer: Vec<u8>,
    /// Ed25519 signature (64 bytes).
    #[serde(with = "b64")]
    pub signature: Vec<u8>,
    /// `DeviceCert::to_bytes` of the signer's certificate.
    #[serde(with = "b64")]
    pub cert: Vec<u8>,
    /// Unix seconds at which the server received this version. Set by the
    /// server, not signed and ignored on push; 0 when unknown. Clients judge
    /// device revocation and certificate expiry against it.
    #[serde(default)]
    pub received_at: i64,
}

impl Envelope {
    pub fn vector(&self) -> VersionVector {
        VersionVector::from_entries(self.version_vector.iter().map(|(d, c)| (d.as_str(), *c)))
    }

    /// Split into the crypto crate's signed envelope and the parsed cert.
    pub fn to_signed(&self) -> Result<(SignedEnvelope, DeviceCert), WireError> {
        let signer: [u8; 32] = self
            .signer
            .as_slice()
            .try_into()
            .map_err(|_| WireError("signer must be 32 bytes"))?;
        let signature: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| WireError("signature must be 64 bytes"))?;
        let cert =
            DeviceCert::from_bytes(&self.cert).map_err(|_| WireError("malformed certificate"))?;
        Ok((
            SignedEnvelope {
                record_id: self.record_id.clone(),
                vault_id: self.vault_id.clone(),
                table: self.table.clone(),
                key_generation: self.key_generation,
                version_vector: self.vector(),
                deleted: self.deleted,
                ciphertext: self.ciphertext.clone(),
                signer,
                signature,
            },
            cert,
        ))
    }

    pub fn from_signed(signed: SignedEnvelope, cert: &DeviceCert) -> Self {
        Self {
            record_id: signed.record_id,
            vault_id: signed.vault_id,
            table: signed.table,
            key_generation: signed.key_generation,
            version_vector: signed
                .version_vector
                .iter()
                .map(|(d, c)| (d.to_owned(), c))
                .collect(),
            deleted: signed.deleted,
            ciphertext: signed.ciphertext,
            signer: signed.signer.to_vec(),
            signature: signed.signature.to_vec(),
            cert: cert.to_bytes(),
            received_at: 0,
        }
    }
}

// ---- records ---------------------------------------------------------------

/// `POST /v1/vaults/{vault}/records`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushRequest {
    pub envelope: Envelope,
    /// Version vector of the server copy this push replaces (compare-and-set).
    /// Empty when the client believes the record does not exist yet.
    pub expected: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    /// The server copy is not the one the client expected.
    Cas,
    /// The expected copy matched but the new vector does not dominate it.
    Stale,
}

/// 200 for `accepted`, 409 for `conflict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
// A wire type built once per push; boxing would only add noise.
#[allow(clippy::large_enum_variant)]
pub enum PushResponse {
    Accepted {
        seq: u64,
    },
    Conflict {
        reason: ConflictReason,
        /// The envelope the server holds; the client merges it and retries.
        current: Option<Envelope>,
    },
}

/// `POST /v1/vaults/{vault}/records/batch`: several pushes in one
/// transaction. Every item is checked like a single push (compare-and-set,
/// signature, certificate, key generation); the batch is applied only if all
/// of them pass, otherwise nothing is written and each item's outcome is
/// reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchPushRequest {
    pub items: Vec<PushRequest>,
}

/// The outcome of one item of a batch or rotation, in request order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[allow(clippy::large_enum_variant)]
pub enum BatchItemResult {
    Accepted {
        seq: u64,
    },
    Conflict {
        reason: ConflictReason,
        current: Option<Envelope>,
    },
    /// The item itself is not acceptable (bad signature, revoked device,
    /// wrong key generation, bad shape, ...).
    Rejected {
        code: ErrorCode,
        message: String,
    },
    /// The item was fine but not applied because another item failed.
    NotApplied,
}

/// 200 when `committed`, 409 when nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchPushResponse {
    pub committed: bool,
    pub results: Vec<BatchItemResult>,
}

/// The new vault key sealed to one member.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberKey {
    pub account_id: String,
    #[serde(with = "b64")]
    pub sealed_key: Vec<u8>,
}

/// `POST /v1/vaults/{vault}/rotate` (owners only): replace the vault key.
///
/// In one transaction the server checks that `from_generation` is current,
/// removes `remove_member` if given, checks that `sealed_keys` covers
/// exactly the remaining members, and accepts one re-encrypted envelope
/// (key generation `from_generation + 1`) for every record the vault holds.
/// All or nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateRequest {
    pub from_generation: u32,
    #[serde(default)]
    pub remove_member: Option<String>,
    pub sealed_keys: Vec<MemberKey>,
    pub records: Vec<PushRequest>,
}

/// 200 when `committed`, 409 when nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateResponse {
    pub committed: bool,
    /// The vault's key generation now (the new one when committed).
    pub key_generation: u32,
    /// The membership differs from what `sealed_keys` and `remove_member`
    /// assumed (somebody was added or removed meanwhile).
    #[serde(default)]
    pub members_changed: bool,
    /// Records the server holds that `records` did not include.
    #[serde(default)]
    pub missing: Vec<String>,
    /// Per record, in request order.
    #[serde(default)]
    pub results: Vec<BatchItemResult>,
}

/// A device of a vault member that was revoked, and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedDevice {
    pub device_id: String,
    /// Unix seconds on the server's clock.
    pub revoked_at: i64,
}

/// One pulled record with its server sequence number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PulledRecord {
    pub seq: u64,
    pub envelope: Envelope,
}

/// `GET /v1/vaults/{vault}/records?since=&limit=`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullResponse {
    pub records: Vec<PulledRecord>,
    /// Highest sequence in `records`, or the requested `since` when empty.
    pub cursor: u64,
    pub has_more: bool,
}

/// A replaced version kept by the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryItem {
    pub seq: u64,
    /// Unix seconds at which a newer version replaced it.
    pub replaced_at: i64,
    pub envelope: Envelope,
}

/// `GET /v1/vaults/{vault}/records/{record}/history`, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub versions: Vec<HistoryItem>,
}

// ---- accounts --------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterStartRequest {
    pub username: String,
    /// OPAQUE `RegistrationRequest`.
    #[serde(with = "b64")]
    pub request: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterStartResponse {
    #[serde(with = "b64")]
    pub response: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterFinishRequest {
    pub username: String,
    /// OPAQUE `RegistrationUpload`.
    #[serde(with = "b64")]
    pub upload: Vec<u8>,
    /// `KdfHeader::to_bytes` (public).
    #[serde(with = "b64")]
    pub kdf_header: Vec<u8>,
    /// `wrap_account_keys` blob; opaque to the server.
    #[serde(with = "b64")]
    pub wrapped_account_keys: Vec<u8>,
    /// Account X25519 public key (vault keys are sealed to it).
    #[serde(with = "b64")]
    pub seal_public: Vec<u8>,
    /// Account Ed25519 public key (verifies device certificates).
    #[serde(with = "b64")]
    pub signing_public: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterFinishResponse {
    pub account_id: String,
}

/// `GET /v1/auth/kdf?username=`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfHeaderResponse {
    #[serde(with = "b64")]
    pub kdf_header: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStartRequest {
    pub username: String,
    /// OPAQUE `CredentialRequest`.
    #[serde(with = "b64")]
    pub request: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginStartResponse {
    pub login_id: String,
    /// OPAQUE `CredentialResponse`.
    #[serde(with = "b64")]
    pub response: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginFinishRequest {
    pub login_id: String,
    /// OPAQUE `CredentialFinalization`.
    #[serde(with = "b64")]
    pub finalization: Vec<u8>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginFinishResponse {
    /// Bearer token for every later request. Only its hash is stored.
    pub token: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub account_id: String,
    #[serde(with = "b64")]
    pub wrapped_account_keys: Vec<u8>,
    #[serde(with = "b64")]
    pub seal_public: Vec<u8>,
    #[serde(with = "b64")]
    pub signing_public: Vec<u8>,
}

impl std::fmt::Debug for LoginFinishResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginFinishResponse")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("account_id", &self.account_id)
            .finish_non_exhaustive()
    }
}

/// `GET /v1/accounts/lookup?username=`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub account_id: String,
    pub username: String,
    #[serde(with = "b64")]
    pub seal_public: Vec<u8>,
    #[serde(with = "b64")]
    pub signing_public: Vec<u8>,
}

// ---- devices ---------------------------------------------------------------

/// `POST /v1/devices`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterDeviceRequest {
    #[serde(with = "b64")]
    pub device_public: Vec<u8>,
    pub name: String,
    /// `DeviceCert::to_bytes`, signed by the account key.
    #[serde(with = "b64")]
    pub cert: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub device_id: String,
    pub name: String,
    #[serde(with = "b64")]
    pub device_public: Vec<u8>,
    #[serde(with = "b64")]
    pub cert: Vec<u8>,
    pub created_at: i64,
    pub revoked: bool,
    /// When the server revoked it (unix seconds).
    #[serde(default)]
    pub revoked_at: Option<i64>,
    /// When the registered certificate expires (unix seconds).
    #[serde(default)]
    pub cert_expires_at: i64,
}

// ---- vaults ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Editor,
    Viewer,
}

impl Role {
    pub fn can_write(self) -> bool {
        matches!(self, Role::Owner | Role::Editor)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Editor => "editor",
            Role::Viewer => "viewer",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "owner" => Some(Role::Owner),
            "editor" => Some(Role::Editor),
            "viewer" => Some(Role::Viewer),
            _ => None,
        }
    }
}

/// `POST /v1/vaults`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateVaultRequest {
    pub vault_id: String,
    /// The vault key sealed to the creator's account key.
    #[serde(with = "b64")]
    pub sealed_key: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSummary {
    pub vault_id: String,
    pub role: Role,
    /// The vault key of generation `key_generation`, sealed to this account.
    #[serde(with = "b64")]
    pub sealed_key: Vec<u8>,
    /// Current key generation (1 until the first rotation).
    pub key_generation: u32,
}

/// `PUT /v1/vaults/{vault}/members/{account}` (owners only)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PutMemberRequest {
    pub role: Role,
    /// The vault key sealed to that member's account key. Required when the
    /// member is new; optional when only the role changes.
    #[serde(default, with = "opt_b64")]
    pub sealed_key: Option<Vec<u8>>,
    /// Generation of the key in `sealed_key`; required with it. The server
    /// refuses a key of an older generation (a rotation happened meanwhile).
    #[serde(default)]
    pub key_generation: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberInfo {
    pub account_id: String,
    pub username: String,
    pub role: Role,
    #[serde(with = "b64")]
    pub seal_public: Vec<u8>,
    #[serde(with = "b64")]
    pub signing_public: Vec<u8>,
}

mod opt_b64 {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => s.serialize_some(&STANDARD.encode(bytes)),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            Some(text) => STANDARD
                .decode(text)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

// ---- errors ----------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict,
    TooLarge,
    RateLimited,
    DeviceNotRegistered,
    DeviceRevoked,
    InvalidSignature,
    /// The device certificate is past its expiry.
    CertificateExpired,
    /// A write used a vault key generation other than the current one.
    StaleKeyGeneration,
    RegistrationClosed,
    Internal,
}

/// Body of every non-2xx response (except push conflicts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_round_trip() {
        let key = [0xab; 32];
        let id = device_id_for(&key);
        assert_eq!(id.len(), 64);
        assert_eq!(device_public_from_id(&id), Some(key));
        assert_eq!(device_public_from_id("zz"), None);
    }

    #[test]
    fn push_response_tags() {
        let json = serde_json::to_string(&PushResponse::Accepted { seq: 4 }).unwrap();
        assert_eq!(json, r#"{"status":"accepted","seq":4}"#);
        let back: PushResponse =
            serde_json::from_str(r#"{"status":"conflict","reason":"cas","current":null}"#).unwrap();
        assert_eq!(
            back,
            PushResponse::Conflict {
                reason: ConflictReason::Cas,
                current: None
            }
        );
    }

    #[test]
    fn batch_results_use_snake_case_tags() {
        let json = serde_json::to_string(&BatchItemResult::NotApplied).unwrap();
        assert_eq!(json, r#"{"status":"not_applied"}"#);
        let back: BatchItemResult = serde_json::from_str(
            r#"{"status":"rejected","code":"stale_key_generation","message":"x"}"#,
        )
        .unwrap();
        assert_eq!(
            back,
            BatchItemResult::Rejected {
                code: ErrorCode::StaleKeyGeneration,
                message: "x".into()
            }
        );
    }

    #[test]
    fn member_request_optional_key() {
        let req: PutMemberRequest = serde_json::from_str(r#"{"role":"viewer"}"#).unwrap();
        assert_eq!(req.sealed_key, None);
        let req = PutMemberRequest {
            role: Role::Editor,
            sealed_key: Some(vec![1, 2, 3]),
            key_generation: Some(2),
        };
        let back: PutMemberRequest =
            serde_json::from_str(&serde_json::to_string(&req).unwrap()).unwrap();
        assert_eq!(back, req);
    }
}
