//! Local cryptographic building blocks for EasySSH v2.
//!
//! No custom primitives: XChaCha20-Poly1305, Argon2id, HKDF-SHA256, X25519
//! and Ed25519 from RustCrypto and dalek, composed as documented in the
//! crate README. Formats are versioned and covered by committed test vectors
//! (`tests/vectors/*.json`).

#![forbid(unsafe_code)]

mod error;
pub mod field;
pub mod kdf;
pub mod keys;
pub mod keystore;
pub mod protect;
pub mod record;
pub mod secret;
pub mod sign;
pub mod version_vector;

pub use error::{CryptoError, Result};
pub use field::{DeviceFieldCipher, DeviceKeyCipher, FieldCipher, NotConfigured};
pub use kdf::{derive_account_secrets, AccountSecrets, KdfHeader, KdfParams};
pub use keys::{
    open_vault_key, seal_vault_key, unwrap_account_keys, wrap_account_keys, AccountKeyPair,
    AccountSealingKey, AccountSigningKey, SealPublicKey,
};
#[cfg(feature = "os-keyring")]
pub use keystore::OsKeyStore;
pub use keystore::{load_or_create_device_key, KeyStore, KeyStoreError, MemoryKeyStore};
pub use record::{decrypt_record, encrypt_record, Aad};
pub use secret::{AccountKey, AuthSecret, DeviceKey, SecretText, VaultKey};
pub use sign::{
    certify_device, sign_envelope, verify_envelope, DeviceCert, DeviceSigningKey, Envelope,
    EnvelopeFields,
};
pub use version_vector::{Ordering, VersionVector};

/// Fill `buf` from the operating system CSPRNG.
pub(crate) fn fill_random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|_| CryptoError::Rng)
}

pub(crate) fn random_array<const N: usize>() -> Result<[u8; N]> {
    let mut a = [0u8; N];
    fill_random(&mut a)?;
    Ok(a)
}

/// Append a `u32` big-endian length followed by `bytes`.
pub(crate) fn put_lp(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}
