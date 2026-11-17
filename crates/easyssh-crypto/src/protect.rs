//! How the device key rests in the key store, with or without a vault
//! passphrase.
//!
//! * **Device only** (default): the raw device key sits in the OS keychain
//!   under [`DEVICE_KEY_ACCOUNT`]. Whoever can read that keychain item can
//!   decrypt the local database.
//! * **Passphrase**: the raw item is deleted and replaced by a wrapped copy
//!   under [`WRAPPED_DEVICE_KEY_ACCOUNT`]. The wrapping key comes from the
//!   passphrase (Argon2id, then HKDF), so a stolen keychain item alone is not
//!   enough.
//!
//! Wrapped item: `0x01 || kdf_header(29) || blob(73)`, where `blob` is the
//! usual `0x01 || nonce(24) || ct(32) || tag(16)` under
//! `HKDF-SHA256(account_key, "easyssh/v1/device-key-wrap")` with AAD
//! `"easyssh/v1/device-key-wrap" || kdf_header`, so the header (and with it
//! the Argon2 parameters) is authenticated too.

use crate::kdf::{derive_account_secrets, KdfHeader, KdfParams, HEADER_LEN};
use crate::keystore::{
    KeyStore, KeyStoreError, DEVICE_KEY_ACCOUNT, SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT,
};
use crate::record::{open_blob, seal_blob, NONCE_LEN};
use crate::{fill_random, CryptoError, DeviceKey};
use hkdf::Hkdf;
use sha2::Sha256;
use thiserror::Error;
use zeroize::Zeroizing;

const WRAP_VERSION: u8 = 1;
const WRAP_INFO: &[u8] = b"easyssh/v1/device-key-wrap";

/// How the device key is protected right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    /// Raw key in the key store (also the state before any key exists).
    DeviceOnly,
    /// Wrapped by a passphrase.
    Passphrase,
}

#[derive(Debug, Error)]
pub enum ProtectError {
    /// The key is passphrase-protected and none was given.
    #[error("the vault passphrase is required")]
    NeedsPassphrase,
    /// Wrong passphrase, or the stored item was tampered with.
    #[error("wrong vault passphrase")]
    WrongPassphrase,
    #[error("a vault passphrase is already set")]
    AlreadySet,
    #[error("no vault passphrase is set")]
    NotSet,
    #[error("the vault passphrase must not be empty")]
    EmptyPassphrase,
    #[error(transparent)]
    KeyStore(#[from] KeyStoreError),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// Whether a vault passphrase is set. Reads only the key store.
pub fn protection(store: &dyn KeyStore) -> Result<Protection, ProtectError> {
    Ok(if store.get(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT)?.is_some() {
        Protection::Passphrase
    } else {
        Protection::DeviceOnly
    })
}

/// The device key. Without a passphrase set it is read from the key store
/// (and created on first use); with one, `passphrase` is required.
pub fn open_device_key(
    store: &dyn KeyStore,
    passphrase: Option<&[u8]>,
) -> Result<DeviceKey, ProtectError> {
    match store.get(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT)? {
        Some(item) => {
            let passphrase = passphrase.ok_or(ProtectError::NeedsPassphrase)?;
            let key = unwrap_device_key(&item, passphrase)?;
            // A crash between writing the wrapped item and deleting the raw
            // one leaves a stale raw key behind; remove it now.
            store.delete(SERVICE, DEVICE_KEY_ACCOUNT)?;
            Ok(key)
        }
        None => Ok(crate::load_or_create_device_key(store)?),
    }
}

/// Protect the device key with `passphrase`. The wrapped item is written
/// before the raw one is deleted, so a failure never loses the key.
pub fn set_passphrase(
    store: &dyn KeyStore,
    passphrase: &[u8],
    params: KdfParams,
) -> Result<(), ProtectError> {
    if passphrase.is_empty() {
        return Err(ProtectError::EmptyPassphrase);
    }
    if protection(store)? == Protection::Passphrase {
        return Err(ProtectError::AlreadySet);
    }
    let key = crate::load_or_create_device_key(store)?;
    let item = wrap_device_key(&key, passphrase, params)?;
    store.set(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT, &item)?;
    store.delete(SERVICE, DEVICE_KEY_ACCOUNT)?;
    Ok(())
}

/// Change the passphrase; `old` must be right.
pub fn change_passphrase(
    store: &dyn KeyStore,
    old: &[u8],
    new: &[u8],
    params: KdfParams,
) -> Result<(), ProtectError> {
    if new.is_empty() {
        return Err(ProtectError::EmptyPassphrase);
    }
    let item = store
        .get(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT)?
        .ok_or(ProtectError::NotSet)?;
    let key = unwrap_device_key(&item, old)?;
    let item = wrap_device_key(&key, new, params)?;
    store.set(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT, &item)?;
    Ok(())
}

/// Go back to keychain-only protection; `passphrase` must be right.
pub fn remove_passphrase(store: &dyn KeyStore, passphrase: &[u8]) -> Result<(), ProtectError> {
    let item = store
        .get(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT)?
        .ok_or(ProtectError::NotSet)?;
    let key = unwrap_device_key(&item, passphrase)?;
    store.set(SERVICE, DEVICE_KEY_ACCOUNT, key.expose())?;
    store.delete(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT)?;
    Ok(())
}

/// Delete every key of this device from the key store. Everything encrypted
/// with them becomes unreadable for good.
pub fn destroy_device_key(store: &dyn KeyStore) -> Result<(), ProtectError> {
    store.delete(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT)?;
    store.delete(SERVICE, DEVICE_KEY_ACCOUNT)?;
    Ok(())
}

fn wrap_key(
    passphrase: &[u8],
    header: &KdfHeader,
) -> Result<Zeroizing<[u8; 32]>, CryptoError> {
    let secrets = derive_account_secrets(passphrase, header)?;
    let hk = Hkdf::<Sha256>::new(None, secrets.account_key.expose());
    let mut key = Zeroizing::new([0u8; 32]);
    hk.expand(WRAP_INFO, &mut *key)
        .map_err(|_| CryptoError::Kdf)?;
    Ok(key)
}

fn wrap_aad(header: &[u8]) -> Vec<u8> {
    let mut aad = WRAP_INFO.to_vec();
    aad.extend_from_slice(header);
    aad
}

fn wrap_device_key(
    key: &DeviceKey,
    passphrase: &[u8],
    params: KdfParams,
) -> Result<Vec<u8>, CryptoError> {
    let header = KdfHeader::generate(params)?;
    let header_bytes = header.to_bytes();
    let wrapping = wrap_key(passphrase, &header)?;
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    let blob = seal_blob(
        WRAP_VERSION,
        &wrapping,
        &nonce,
        &wrap_aad(&header_bytes),
        key.expose(),
    )?;
    let mut item = Vec::with_capacity(1 + HEADER_LEN + blob.len());
    item.push(WRAP_VERSION);
    item.extend_from_slice(&header_bytes);
    item.extend_from_slice(&blob);
    Ok(item)
}

fn unwrap_device_key(item: &[u8], passphrase: &[u8]) -> Result<DeviceKey, ProtectError> {
    if item.len() < 1 + HEADER_LEN || item[0] != WRAP_VERSION {
        return Err(CryptoError::Malformed("wrapped device key").into());
    }
    let header_bytes = &item[1..1 + HEADER_LEN];
    let header = KdfHeader::from_bytes(header_bytes)?;
    let wrapping = wrap_key(passphrase, &header)?;
    let plain = Zeroizing::new(
        open_blob(
            WRAP_VERSION,
            &wrapping,
            &wrap_aad(header_bytes),
            &item[1 + HEADER_LEN..],
        )
        .map_err(|_| ProtectError::WrongPassphrase)?,
    );
    Ok(DeviceKey::from_slice(&plain)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryKeyStore;

    const FAST: KdfParams = KdfParams::insecure_for_tests();

    #[test]
    fn default_is_device_only_and_key_is_stable() {
        let s = MemoryKeyStore::new();
        assert_eq!(protection(&s).unwrap(), Protection::DeviceOnly);
        let a = open_device_key(&s, None).unwrap();
        let b = open_device_key(&s, Some(b"ignored")).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn wrap_unwrap_and_raw_key_is_gone() {
        let s = MemoryKeyStore::new();
        let before = open_device_key(&s, None).unwrap();
        set_passphrase(&s, b"correct horse", FAST).unwrap();
        assert_eq!(protection(&s).unwrap(), Protection::Passphrase);
        // The raw key is no longer in the key store.
        assert!(s.get(SERVICE, DEVICE_KEY_ACCOUNT).unwrap().is_none());
        assert!(matches!(
            open_device_key(&s, None),
            Err(ProtectError::NeedsPassphrase)
        ));
        assert!(matches!(
            open_device_key(&s, Some(b"wrong")),
            Err(ProtectError::WrongPassphrase)
        ));
        let after = open_device_key(&s, Some(b"correct horse")).unwrap();
        assert_eq!(before, after);
        // A stored item alone does not contain the key bytes.
        let item = s.get(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT).unwrap().unwrap();
        assert!(!item.windows(32).any(|w| w == before.expose()));
    }

    #[test]
    fn set_twice_is_refused_change_and_remove_work() {
        let s = MemoryKeyStore::new();
        let key = open_device_key(&s, None).unwrap();
        assert!(matches!(
            set_passphrase(&s, b"", FAST),
            Err(ProtectError::EmptyPassphrase)
        ));
        set_passphrase(&s, b"one", FAST).unwrap();
        assert!(matches!(
            set_passphrase(&s, b"two", FAST),
            Err(ProtectError::AlreadySet)
        ));
        assert!(matches!(
            change_passphrase(&s, b"bad", b"two", FAST),
            Err(ProtectError::WrongPassphrase)
        ));
        change_passphrase(&s, b"one", b"two", FAST).unwrap();
        assert!(open_device_key(&s, Some(b"one")).is_err());
        assert_eq!(open_device_key(&s, Some(b"two")).unwrap(), key);
        assert!(matches!(
            remove_passphrase(&s, b"nope"),
            Err(ProtectError::WrongPassphrase)
        ));
        remove_passphrase(&s, b"two").unwrap();
        assert_eq!(protection(&s).unwrap(), Protection::DeviceOnly);
        assert_eq!(open_device_key(&s, None).unwrap(), key);
        assert!(matches!(
            remove_passphrase(&s, b"two"),
            Err(ProtectError::NotSet)
        ));
    }

    #[test]
    fn tampering_with_the_item_or_header_fails() {
        let s = MemoryKeyStore::new();
        set_passphrase(&s, b"pw", FAST).unwrap();
        let item = s.get(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT).unwrap().unwrap();
        for i in [0usize, 5, 20, 40, item.len() - 1] {
            let mut bad = item.to_vec();
            bad[i] ^= 1;
            assert!(unwrap_device_key(&bad, b"pw").is_err(), "byte {i}");
        }
        assert!(unwrap_device_key(&item[..item.len() - 1], b"pw").is_err());
    }

    #[test]
    fn stale_raw_key_is_cleaned_up_on_unlock() {
        let s = MemoryKeyStore::new();
        let key = open_device_key(&s, None).unwrap();
        set_passphrase(&s, b"pw", FAST).unwrap();
        // Simulate a crash that left the raw key behind.
        s.set(SERVICE, DEVICE_KEY_ACCOUNT, key.expose()).unwrap();
        open_device_key(&s, Some(b"pw")).unwrap();
        assert!(s.get(SERVICE, DEVICE_KEY_ACCOUNT).unwrap().is_none());
    }

    #[test]
    fn destroy_removes_everything() {
        let s = MemoryKeyStore::new();
        set_passphrase(&s, b"pw", FAST).unwrap();
        destroy_device_key(&s).unwrap();
        assert!(s.get(SERVICE, WRAPPED_DEVICE_KEY_ACCOUNT).unwrap().is_none());
        assert!(s.get(SERVICE, DEVICE_KEY_ACCOUNT).unwrap().is_none());
    }
}
