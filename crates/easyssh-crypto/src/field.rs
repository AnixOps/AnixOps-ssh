//! Field-level encryption for the local database, keyed by the device key.
//!
//! Blob: `0x01 || nonce(24) || ciphertext || tag(16)`; the cipher key is
//! `HKDF-SHA256(device_key, info = "easyssh/v1/field-cipher")` so the raw
//! device key is never used directly, and the AEAD associated data is
//! `"easyssh/v1/field" || lp(caller_aad)`.

use crate::record::{open_blob, seal_blob, NONCE_LEN};
use crate::{fill_random, put_lp, CryptoError, DeviceKey, Result};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

pub const FIELD_VERSION: u8 = 1;
pub const FIELD_INFO: &[u8] = b"easyssh/v1/field-cipher";
const FIELD_AAD: &[u8] = b"easyssh/v1/field";

/// Encrypts individual database fields. `aad` should name the field's
/// location (for example `b"hosts/<id>/password"`) so values cannot be
/// swapped between rows or columns.
pub trait FieldCipher: Send + Sync {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>>;
    fn decrypt(&self, blob: &[u8], aad: &[u8]) -> Result<Vec<u8>>;
}

/// [`FieldCipher`] backed by the device key.
pub struct DeviceFieldCipher {
    key: Zeroizing<[u8; 32]>,
}

impl DeviceFieldCipher {
    pub fn new(device_key: &DeviceKey) -> Result<Self> {
        let hk = Hkdf::<Sha256>::new(None, device_key.expose());
        let mut key = Zeroizing::new([0u8; 32]);
        hk.expand(FIELD_INFO, &mut *key)
            .map_err(|_| CryptoError::Kdf)?;
        Ok(Self { key })
    }

    fn full_aad(aad: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(FIELD_AAD.len() + 4 + aad.len());
        out.extend_from_slice(FIELD_AAD);
        put_lp(&mut out, aad);
        out
    }

    #[doc(hidden)]
    pub fn encrypt_with_nonce(
        &self,
        nonce: &[u8; NONCE_LEN],
        plaintext: &[u8],
        aad: &[u8],
    ) -> Result<Vec<u8>> {
        seal_blob(
            FIELD_VERSION,
            &self.key,
            nonce,
            &Self::full_aad(aad),
            plaintext,
        )
    }
}

impl core::fmt::Debug for DeviceFieldCipher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("DeviceFieldCipher([REDACTED])")
    }
}

impl FieldCipher for DeviceFieldCipher {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        fill_random(&mut nonce)?;
        self.encrypt_with_nonce(&nonce, plaintext, aad)
    }

    fn decrypt(&self, blob: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        open_blob(FIELD_VERSION, &self.key, &Self::full_aad(aad), blob)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher(n: u8) -> DeviceFieldCipher {
        DeviceFieldCipher::new(&DeviceKey::from_bytes([n; 32])).unwrap()
    }

    #[test]
    fn round_trip() {
        let c = cipher(1);
        let blob = c.encrypt(b"hunter2", b"hosts/1/password").unwrap();
        assert_eq!(c.decrypt(&blob, b"hosts/1/password").unwrap(), b"hunter2");
        assert_eq!(blob[0], 1);
    }

    #[test]
    fn aad_key_tamper_and_truncation() {
        let c = cipher(1);
        let blob = c.encrypt(b"hunter2", b"a").unwrap();
        assert!(c.decrypt(&blob, b"b").is_err());
        assert!(cipher(2).decrypt(&blob, b"a").is_err());
        for i in 0..blob.len() {
            let mut t = blob.clone();
            t[i] ^= 1;
            assert!(c.decrypt(&t, b"a").is_err());
        }
        for len in 0..blob.len() {
            assert!(c.decrypt(&blob[..len], b"a").is_err());
        }
    }

    #[test]
    fn works_as_trait_object() {
        let c: Box<dyn FieldCipher> = Box::new(cipher(1));
        let blob = c.encrypt(b"", b"").unwrap();
        assert!(c.decrypt(&blob, b"").unwrap().is_empty());
    }
}
