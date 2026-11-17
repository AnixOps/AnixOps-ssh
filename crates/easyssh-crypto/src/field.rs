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
use std::fmt::Debug;
use std::sync::RwLock;
use zeroize::Zeroizing;

pub const FIELD_VERSION: u8 = 1;
pub const FIELD_INFO: &[u8] = b"easyssh/v1/field-cipher";
const FIELD_AAD: &[u8] = b"easyssh/v1/field";

/// Encrypts individual database fields. `aad` should name the field's
/// location (for example `b"hosts/<id>/password"`) so values cannot be
/// swapped between rows or columns.
///
/// This is the one `FieldCipher` in the workspace: `easyssh-store` uses it
/// for every secret column.
pub trait FieldCipher: Send + Sync + Debug {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>>;
    fn decrypt(&self, blob: &[u8], aad: &[u8]) -> Result<Vec<u8>>;
}

/// [`FieldCipher`] backed by the device key.
pub struct DeviceFieldCipher {
    key: Zeroizing<[u8; 32]>,
    /// Kept (zeroized on drop) for [`DeviceFieldCipher::derive_key`] only.
    device: DeviceKey,
}

impl DeviceFieldCipher {
    pub fn new(device_key: &DeviceKey) -> Result<Self> {
        let hk = Hkdf::<Sha256>::new(None, device_key.expose());
        let mut key = Zeroizing::new([0u8; 32]);
        hk.expand(FIELD_INFO, &mut *key)
            .map_err(|_| CryptoError::Kdf)?;
        Ok(Self {
            key,
            device: DeviceKey::from_bytes(*device_key.expose()),
        })
    }

    /// A 32-byte subkey for one purpose: `HKDF-SHA256(salt, device_key, info)`.
    /// `info` names the purpose (for example `easyssh/v1/session-log`) and
    /// `salt` is random per use, so two uses never share a key.
    pub fn derive_key(&self, info: &[u8], salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
        let hk = Hkdf::<Sha256>::new(Some(salt), self.device.expose());
        let mut out = Zeroizing::new([0u8; 32]);
        hk.expand(info, &mut *out).map_err(|_| CryptoError::Kdf)?;
        Ok(out)
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

/// Refuses to handle any secret. For stores opened by code that has no
/// vault, so nothing sensitive can be written in plaintext by accident.
#[derive(Debug, Default, Clone, Copy)]
pub struct NotConfigured;

impl FieldCipher for NotConfigured {
    fn encrypt(&self, _plaintext: &[u8], _aad: &[u8]) -> Result<Vec<u8>> {
        Err(CryptoError::NotConfigured)
    }
    fn decrypt(&self, _blob: &[u8], _aad: &[u8]) -> Result<Vec<u8>> {
        Err(CryptoError::NotConfigured)
    }
}

/// The local database's field cipher with a lock: it starts locked and only
/// encrypts or decrypts while a device key is loaded into it. Locking drops
/// (and zeroizes) the derived key. `Arc`-share one of these between the
/// store and whatever unlocks it.
#[derive(Default)]
pub struct DeviceKeyCipher {
    inner: RwLock<Option<DeviceFieldCipher>>,
}

impl DeviceKeyCipher {
    /// A cipher with no key loaded; every call fails with [`CryptoError::Locked`].
    pub fn locked() -> Self {
        Self::default()
    }

    /// A cipher that is already unlocked with `key`.
    pub fn unlocked(key: &DeviceKey) -> Result<Self> {
        let cipher = Self::locked();
        cipher.unlock(key)?;
        Ok(cipher)
    }

    pub fn unlock(&self, key: &DeviceKey) -> Result<()> {
        let field = DeviceFieldCipher::new(key)?;
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = Some(field);
        Ok(())
    }

    pub fn lock(&self) {
        *self.inner.write().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub fn is_unlocked(&self) -> bool {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// A purpose-bound subkey of the device key, see
    /// [`DeviceFieldCipher::derive_key`]. Fails with [`CryptoError::Locked`]
    /// while locked.
    pub fn derive_key(&self, info: &[u8], salt: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
        match &*self.inner.read().unwrap_or_else(|e| e.into_inner()) {
            Some(cipher) => cipher.derive_key(info, salt),
            None => Err(CryptoError::Locked),
        }
    }
}

impl Debug for DeviceKeyCipher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let state = if self.is_unlocked() {
            "unlocked"
        } else {
            "locked"
        };
        write!(f, "DeviceKeyCipher({state})")
    }
}

impl FieldCipher for DeviceKeyCipher {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        match &*self.inner.read().unwrap_or_else(|e| e.into_inner()) {
            Some(cipher) => cipher.encrypt(plaintext, aad),
            None => Err(CryptoError::Locked),
        }
    }

    fn decrypt(&self, blob: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
        match &*self.inner.read().unwrap_or_else(|e| e.into_inner()) {
            Some(cipher) => cipher.decrypt(blob, aad),
            None => Err(CryptoError::Locked),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_cipher_refuses_until_unlocked() {
        let key = DeviceKey::from_bytes([3; 32]);
        let c = DeviceKeyCipher::locked();
        assert_eq!(c.encrypt(b"x", b"a"), Err(CryptoError::Locked));
        c.unlock(&key).unwrap();
        let blob = c.encrypt(b"x", b"a").unwrap();
        assert_eq!(c.decrypt(&blob, b"a").unwrap(), b"x");
        assert_eq!(format!("{c:?}"), "DeviceKeyCipher(unlocked)");
        c.lock();
        assert_eq!(c.decrypt(&blob, b"a"), Err(CryptoError::Locked));
        c.unlock(&key).unwrap();
        assert_eq!(c.decrypt(&blob, b"a").unwrap(), b"x");
    }

    #[test]
    fn derived_keys_depend_on_info_salt_and_device_key() {
        let c = DeviceKeyCipher::unlocked(&DeviceKey::from_bytes([5; 32])).unwrap();
        let a = c.derive_key(b"x", b"salt-1").unwrap();
        assert_eq!(*a, *c.derive_key(b"x", b"salt-1").unwrap());
        assert_ne!(*a, *c.derive_key(b"y", b"salt-1").unwrap());
        assert_ne!(*a, *c.derive_key(b"x", b"salt-2").unwrap());
        let other = DeviceKeyCipher::unlocked(&DeviceKey::from_bytes([6; 32])).unwrap();
        assert_ne!(*a, *other.derive_key(b"x", b"salt-1").unwrap());
        c.lock();
        assert_eq!(c.derive_key(b"x", b"s"), Err(CryptoError::Locked));
    }

    #[test]
    fn not_configured_refuses() {
        assert_eq!(
            NotConfigured.encrypt(b"x", b"a"),
            Err(CryptoError::NotConfigured)
        );
        assert_eq!(
            NotConfigured.decrypt(b"x", b"a"),
            Err(CryptoError::NotConfigured)
        );
    }

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
