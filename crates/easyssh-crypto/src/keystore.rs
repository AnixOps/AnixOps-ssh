//! Storage for small secrets (the device key, the device signing key seed).

use crate::{CryptoError, DeviceKey};
use std::collections::HashMap;
use std::sync::Mutex;
use thiserror::Error;
use zeroize::Zeroizing;

/// Service name used for EasySSH entries in the OS keychain.
pub const SERVICE: &str = "easyssh";
/// Account name of the local-database device key.
pub const DEVICE_KEY_ACCOUNT: &str = "device-key/v1";
/// Account name of the Ed25519 device signing key seed.
pub const DEVICE_SIGNING_ACCOUNT: &str = "device-signing-key/v1";
/// Account name of the device key wrapped by the vault passphrase.
pub const WRAPPED_DEVICE_KEY_ACCOUNT: &str = "device-key-wrapped/v1";

#[derive(Debug, Error)]
pub enum KeyStoreError {
    /// No usable secret service (e.g. headless Linux without a Secret Service).
    #[error("key store unavailable: {0}")]
    Unavailable(String),
    #[error("key store error: {0}")]
    Backend(String),
    #[error(transparent)]
    Crypto(#[from] CryptoError),
}

/// A `(service, account) -> secret bytes` store. Implementations must be
/// safe to share between threads.
pub trait KeyStore: Send + Sync {
    fn get(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError>;
    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeyStoreError>;
    /// Deleting a missing entry is not an error.
    fn delete(&self, service: &str, account: &str) -> Result<(), KeyStoreError>;
}

type Entries = HashMap<(String, String), Zeroizing<Vec<u8>>>;

/// In-process store for tests and ephemeral sessions.
#[derive(Default)]
pub struct MemoryKeyStore {
    map: Mutex<Entries>,
}

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        let map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        Ok(map
            .get(&(service.to_owned(), account.to_owned()))
            .map(|v| Zeroizing::new(v.to_vec())))
    }

    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeyStoreError> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.insert(
            (service.to_owned(), account.to_owned()),
            Zeroizing::new(secret.to_vec()),
        );
        Ok(())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), KeyStoreError> {
        let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
        map.remove(&(service.to_owned(), account.to_owned()));
        Ok(())
    }
}

/// Get the device key from `store`, creating and saving a fresh one on first use.
pub fn load_or_create_device_key(store: &dyn KeyStore) -> Result<DeviceKey, KeyStoreError> {
    if let Some(bytes) = store.get(SERVICE, DEVICE_KEY_ACCOUNT)? {
        return Ok(DeviceKey::from_slice(&bytes)?);
    }
    let key = DeviceKey::generate()?;
    store.set(SERVICE, DEVICE_KEY_ACCOUNT, key.expose())?;
    Ok(key)
}

/// The operating system keychain: Windows Credential Manager, macOS
/// Keychain or the Linux Secret Service. Construction never talks to the
/// service; errors surface on use.
#[cfg(feature = "os-keyring")]
#[derive(Default)]
pub struct OsKeyStore;

#[cfg(feature = "os-keyring")]
impl OsKeyStore {
    pub fn new() -> Self {
        Self
    }

    fn entry(service: &str, account: &str) -> Result<keyring::Entry, KeyStoreError> {
        keyring::Entry::new(service, account).map_err(map_err)
    }
}

#[cfg(feature = "os-keyring")]
fn map_err(e: keyring::Error) -> KeyStoreError {
    match e {
        keyring::Error::NoDefaultStore | keyring::Error::NoStorageAccess(_) => {
            KeyStoreError::Unavailable(e.to_string())
        }
        other => KeyStoreError::Backend(other.to_string()),
    }
}

#[cfg(feature = "os-keyring")]
impl KeyStore for OsKeyStore {
    fn get(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        match Self::entry(service, account)?.get_secret() {
            Ok(v) => Ok(Some(Zeroizing::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeyStoreError> {
        Self::entry(service, account)?
            .set_secret(secret)
            .map_err(map_err)
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), KeyStoreError> {
        match Self::entry(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip() {
        let s = MemoryKeyStore::new();
        assert!(s.get("svc", "a").unwrap().is_none());
        s.set("svc", "a", b"secret").unwrap();
        assert_eq!(&**s.get("svc", "a").unwrap().unwrap(), b"secret");
        assert!(s.get("svc", "b").unwrap().is_none());
        assert!(s.get("other", "a").unwrap().is_none());
        s.set("svc", "a", b"new").unwrap();
        assert_eq!(&**s.get("svc", "a").unwrap().unwrap(), b"new");
        s.delete("svc", "a").unwrap();
        s.delete("svc", "a").unwrap();
        assert!(s.get("svc", "a").unwrap().is_none());
    }

    #[test]
    fn device_key_is_created_once() {
        let s = MemoryKeyStore::new();
        let a = load_or_create_device_key(&s).unwrap();
        let b = load_or_create_device_key(&s).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn corrupt_device_key_is_an_error() {
        let s = MemoryKeyStore::new();
        s.set(SERVICE, DEVICE_KEY_ACCOUNT, b"short").unwrap();
        assert!(load_or_create_device_key(&s).is_err());
    }

    /// Needs a running Secret Service / Keychain / Credential Manager.
    #[cfg(feature = "os-keyring")]
    #[test]
    #[ignore = "requires a real OS keychain"]
    fn os_store_round_trip() {
        let s = OsKeyStore::new();
        let account = "easyssh-crypto-test";
        s.set("easyssh-test", account, b"hello").unwrap();
        assert_eq!(
            &**s.get("easyssh-test", account).unwrap().unwrap(),
            b"hello"
        );
        s.delete("easyssh-test", account).unwrap();
        assert!(s.get("easyssh-test", account).unwrap().is_none());
    }
}
