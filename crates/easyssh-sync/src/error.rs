use crate::wire::ErrorCode;
use easyssh_crypto::CryptoError;
use thiserror::Error;

/// A wire value that could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("malformed wire value: {0}")]
pub struct WireError(pub &'static str);

/// Why an envelope was rejected before any merge.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VerifyError {
    #[error("malformed envelope: {0}")]
    Malformed(&'static str),
    #[error("the signer's certificate is not signed by a trusted account")]
    UntrustedCertificate,
    #[error("the certificate belongs to a different device than the signer")]
    SignerMismatch,
    #[error("invalid envelope signature")]
    BadSignature,
    #[error("rollback: version vector is older than one already seen for this record")]
    Rollback,
    #[error("the signer's certificate had expired when the server received this version")]
    CertificateExpired,
    #[error("the signing device had been revoked when the server received this version")]
    DeviceRevoked,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MergeError {
    #[error("records belong to different tables ({local} vs {remote})")]
    TableMismatch { local: String, remote: String },
}

#[derive(Debug, Error)]
pub enum TransportError {
    /// The server answered with an error status.
    #[error("server error {status}: {code:?}: {message}")]
    Api {
        status: u16,
        code: ErrorCode,
        message: String,
    },
    #[error("network error: {0}")]
    Network(String),
    #[error("unexpected server response: {0}")]
    Protocol(String),
}

impl TransportError {
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            TransportError::Api { code, .. } => Some(*code),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum SyncError {
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("envelope rejected: {0}")]
    Verify(#[from] VerifyError),
    #[error(transparent)]
    Merge(#[from] MergeError),
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    #[error("local store error: {0}")]
    Store(Box<dyn std::error::Error + Send + Sync>),
    #[error("no key for vault {0}")]
    MissingVaultKey(String),
    #[error("record content is invalid: {0}")]
    BadRecord(&'static str),
    #[error("gave up after repeated conflicts on record {0}")]
    TooManyConflicts(String),
    /// The server holds a vault key generation this client has no key for
    /// (the vault was rotated); fetch the vault list again and retry.
    #[error("no key for generation {generation} of vault {vault_id}")]
    UnknownKeyGeneration { vault_id: String, generation: u32 },
    #[error("the vault key rotation was refused: {0}")]
    Rotation(RotationFailure),
}

impl SyncError {
    /// The vault key changed under this client: refresh the keys and retry.
    pub fn is_stale_key(&self) -> bool {
        matches!(
            self,
            SyncError::UnknownKeyGeneration { .. }
                | SyncError::Transport(TransportError::Api {
                    code: ErrorCode::StaleKeyGeneration,
                    ..
                })
        )
    }
}

/// Why the server did not apply a key rotation. Nothing was written.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RotationFailure {
    #[error("the vault key is already at generation {current}")]
    Generation { current: u32 },
    #[error("the members changed meanwhile")]
    Members,
    #[error("{} record(s) changed meanwhile", .0.len())]
    Conflict(Vec<String>),
    #[error("the server holds {} record(s) this device cannot re-encrypt", .0.len())]
    Missing(Vec<String>),
    #[error("record {record_id} was refused: {message}")]
    Rejected {
        record_id: String,
        code: ErrorCode,
        message: String,
    },
}

impl RotationFailure {
    /// Pulling and trying again can fix it.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            RotationFailure::Generation { .. }
                | RotationFailure::Members
                | RotationFailure::Conflict(_)
        )
    }
}

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("OPAQUE protocol failure (wrong passphrase or tampered message)")]
    Opaque,
    #[error("key derivation failed: {0}")]
    Crypto(#[from] CryptoError),
    #[error(transparent)]
    Transport(#[from] TransportError),
    #[error("server sent malformed account data: {0}")]
    BadAccountData(&'static str),
}
