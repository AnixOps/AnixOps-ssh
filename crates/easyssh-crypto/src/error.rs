use thiserror::Error;

pub type Result<T, E = CryptoError> = std::result::Result<T, E>;

/// Deliberately coarse: decryption failures never say which part mismatched.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("operating system random number generator failed")]
    Rng,
    #[error("unsupported format version {0:#04x}")]
    UnsupportedVersion(u8),
    #[error("malformed input: {0}")]
    Malformed(&'static str),
    #[error("authentication failed")]
    Authentication,
    #[error("invalid key derivation parameters: {0}")]
    InvalidKdfParams(&'static str),
    #[error("key derivation failed")]
    Kdf,
    #[error("invalid signature")]
    BadSignature,
    #[error("device certificate is invalid or does not belong to this account")]
    BadCertificate,
    #[error("envelope signer does not match the device certificate")]
    SignerMismatch,
    #[error("sealed key could not be opened (wrong recipient or corrupted)")]
    SealOpen,
    /// The cipher exists but its key is not loaded (a locked vault).
    #[error("the vault is locked")]
    Locked,
    /// No cipher was set up; secrets are refused rather than stored in the clear.
    #[error("no field cipher is configured; refusing to store or read a secret")]
    NotConfigured,
}
