//! The vault's lock state: where the device key comes from, the optional
//! passphrase, the idle timer.
//!
//! Two protection levels (see `easyssh_crypto::protect`):
//!
//! * no passphrase: the device key lives in the OS keychain only. The vault
//!   "unlocks" itself on the first secret use of a run by reading it;
//! * a passphrase: the keychain holds only a wrapped copy. The first secret
//!   use fails with [`VaultError::Locked`] until [`Vault::unlock`] is given
//!   the passphrase (or an [`UnlockProvider`] opens it).
//!
//! In both modes [`Vault::lock`] and the idle timer drop the key from memory.

use easyssh_crypto::protect::{self, ProtectError, Protection};
use easyssh_crypto::{
    CryptoError, DeviceKey, DeviceKeyCipher, FieldCipher, KdfParams, KeyStore, KeyStoreError,
};
use easyssh_store::{SecretFlags, SecretsUpdate, Store, StoreError, StoredSecrets};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Auto-lock after this many idle minutes unless the user chose otherwise.
pub const DEFAULT_AUTO_LOCK_MINUTES: u32 = 15;
/// Longest auto-lock the UI offers (a day); 0 means never.
pub const MAX_AUTO_LOCK_MINUTES: u32 = 24 * 60;
/// Settings key (in the app database) of the auto-lock minutes.
pub const AUTO_LOCK_SETTING: &str = "vault_auto_lock_minutes";

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("the vault is locked")]
    Locked,
    #[error("wrong vault passphrase")]
    WrongPassphrase,
    #[error("{0}")]
    Passphrase(&'static str),
    #[error("the operating system keychain is not available: {0}")]
    KeychainUnavailable(String),
    #[error("keychain error: {0}")]
    KeyStore(String),
    #[error("database error: {0}")]
    Store(String),
    #[error("encryption error: {0}")]
    Crypto(String),
}

impl VaultError {
    /// A short stable code for the wire protocol.
    pub fn code(&self) -> &'static str {
        match self {
            VaultError::Locked => "vault_locked",
            VaultError::WrongPassphrase => "wrong_passphrase",
            VaultError::Passphrase(_) => "invalid_passphrase",
            VaultError::KeychainUnavailable(_) => "keychain_unavailable",
            VaultError::KeyStore(_) => "keychain_error",
            VaultError::Store(_) => "store_error",
            VaultError::Crypto(_) => "crypto_error",
        }
    }
}

impl From<KeyStoreError> for VaultError {
    fn from(error: KeyStoreError) -> Self {
        match error {
            KeyStoreError::Unavailable(why) => VaultError::KeychainUnavailable(why),
            other => VaultError::KeyStore(other.to_string()),
        }
    }
}

impl From<CryptoError> for VaultError {
    fn from(error: CryptoError) -> Self {
        match error {
            CryptoError::Locked => VaultError::Locked,
            other => VaultError::Crypto(other.to_string()),
        }
    }
}

impl From<ProtectError> for VaultError {
    fn from(error: ProtectError) -> Self {
        match error {
            ProtectError::NeedsPassphrase => VaultError::Locked,
            ProtectError::WrongPassphrase => VaultError::WrongPassphrase,
            ProtectError::AlreadySet => VaultError::Passphrase("a vault passphrase is already set"),
            ProtectError::NotSet => VaultError::Passphrase("no vault passphrase is set"),
            ProtectError::EmptyPassphrase => {
                VaultError::Passphrase("the vault passphrase must not be empty")
            }
            ProtectError::KeyStore(error) => error.into(),
            ProtectError::Crypto(error) => error.into(),
        }
    }
}

impl From<StoreError> for VaultError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Cipher(error) => error.into(),
            other => VaultError::Store(other.to_string()),
        }
    }
}

/// A way to open a passphrase-protected vault without typing the passphrase.
///
/// TODO(phase 4, later): Windows Hello, Touch ID and FIDO2 keys implement
/// this. They must hand back the device key only after the user proved
/// presence, keeping it in hardware-backed storage that the passphrase does
/// not replace. Not implemented in this lane; the hook exists so the unlock
/// path does not have to change.
pub trait UnlockProvider: Send + Sync {
    fn name(&self) -> &'static str;
    /// `Ok(None)` when this provider cannot or did not unlock (the caller
    /// falls back to asking for the passphrase).
    fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError>;
}

/// What the UI shows in Settings → Security.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultStatus {
    /// A vault passphrase wraps the device key.
    pub passphrase_set: bool,
    /// The key is loaded right now.
    pub unlocked: bool,
    /// 0 = never.
    pub auto_lock_minutes: u32,
    /// False when there is no usable OS keychain (nothing can be stored).
    pub keychain_available: bool,
    pub keychain_error: Option<String>,
}

struct Inner {
    last_used: Instant,
    auto_lock: Option<Duration>,
    providers: Vec<Arc<dyn UnlockProvider>>,
}

pub struct Vault {
    keystore: Arc<dyn KeyStore>,
    cipher: Arc<DeviceKeyCipher>,
    kdf: KdfParams,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault")
            .field("cipher", &self.cipher)
            .finish_non_exhaustive()
    }
}

fn minutes(minutes: u32) -> Option<Duration> {
    (minutes > 0).then(|| Duration::from_secs(u64::from(minutes) * 60))
}

impl Vault {
    /// A vault over `keystore` with the production Argon2id parameters.
    pub fn new(keystore: Arc<dyn KeyStore>) -> Self {
        Self::with_kdf(keystore, KdfParams::DESKTOP_V1)
    }

    /// The vault of this device, kept in the operating system keychain.
    #[cfg(feature = "os-keyring")]
    pub fn os() -> Self {
        Self::new(Arc::new(easyssh_crypto::OsKeyStore::new()))
    }

    /// Like [`Vault::new`] with explicit Argon2id parameters (tests use weak ones).
    pub fn with_kdf(keystore: Arc<dyn KeyStore>, kdf: KdfParams) -> Self {
        Self {
            keystore,
            cipher: Arc::new(DeviceKeyCipher::locked()),
            kdf,
            inner: Mutex::new(Inner {
                last_used: Instant::now(),
                auto_lock: minutes(DEFAULT_AUTO_LOCK_MINUTES),
                providers: Vec::new(),
            }),
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The cipher to open the app database with. It is locked until the
    /// vault unlocks and locks again with it.
    pub fn cipher(&self) -> Arc<dyn FieldCipher> {
        self.cipher.clone()
    }

    pub fn add_unlock_provider(&self, provider: Arc<dyn UnlockProvider>) {
        self.inner().providers.push(provider);
    }

    pub fn is_unlocked(&self) -> bool {
        self.cipher.is_unlocked()
    }

    pub fn auto_lock_minutes(&self) -> u32 {
        self.inner()
            .auto_lock
            .map_or(0, |d| (d.as_secs() / 60) as u32)
    }

    /// 0 turns auto-lock off.
    pub fn set_auto_lock_minutes(&self, value: u32) -> Result<(), VaultError> {
        if value > MAX_AUTO_LOCK_MINUTES {
            return Err(VaultError::Passphrase("auto-lock is limited to 24 hours"));
        }
        let mut inner = self.inner();
        inner.auto_lock = minutes(value);
        inner.last_used = Instant::now();
        Ok(())
    }

    /// Loads the saved auto-lock minutes from the app database.
    pub fn load_settings(&self, store: &Store) {
        if let Ok(Some(value)) = store.get_setting_as::<u32>(AUTO_LOCK_SETTING) {
            let _ = self.set_auto_lock_minutes(value);
        }
    }

    /// Changes the auto-lock and saves it to the app database.
    pub fn save_auto_lock(&self, store: &Store, value: u32) -> Result<(), VaultError> {
        self.set_auto_lock_minutes(value)?;
        store.set_setting_as(AUTO_LOCK_SETTING, &value)?;
        Ok(())
    }

    pub fn status(&self) -> VaultStatus {
        let (passphrase_set, keychain_available, keychain_error) =
            match protect::protection(&*self.keystore) {
                Ok(p) => (p == Protection::Passphrase, true, None),
                Err(error) => (false, false, Some(VaultError::from(error).to_string())),
            };
        VaultStatus {
            passphrase_set,
            unlocked: self.is_unlocked(),
            auto_lock_minutes: self.auto_lock_minutes(),
            keychain_available,
            keychain_error,
        }
    }

    /// Locks if the vault has been idle for the auto-lock time as of `now`.
    /// Returns whether it locked just now.
    pub fn lock_if_idle(&self, now: Instant) -> bool {
        let inner = self.inner();
        let idle = now.saturating_duration_since(inner.last_used);
        match inner.auto_lock {
            Some(limit) if idle >= limit && self.cipher.is_unlocked() => {
                self.cipher.lock();
                true
            }
            _ => false,
        }
    }

    /// For a timer: call it every so often.
    pub fn tick(&self) -> bool {
        self.lock_if_idle(Instant::now())
    }

    /// Drops the key from memory now.
    pub fn lock(&self) {
        self.cipher.lock();
    }

    /// Makes sure the key is loaded, or says why it cannot be. Counts as
    /// activity, so the idle timer restarts.
    pub fn ensure_unlocked(&self) -> Result<(), VaultError> {
        self.lock_if_idle(Instant::now());
        let mut inner = self.inner();
        if !self.cipher.is_unlocked() {
            match protect::protection(&*self.keystore)? {
                Protection::DeviceOnly => {
                    let key = protect::open_device_key(&*self.keystore, None)?;
                    self.cipher.unlock(&key)?;
                }
                Protection::Passphrase => {
                    let mut opened = false;
                    for provider in &inner.providers {
                        if let Some(key) = provider.try_unlock()? {
                            self.cipher.unlock(&key)?;
                            opened = true;
                            break;
                        }
                    }
                    if !opened {
                        return Err(VaultError::Locked);
                    }
                }
            }
        }
        inner.last_used = Instant::now();
        Ok(())
    }

    /// Unlocks a passphrase-protected vault. A vault without a passphrase
    /// just loads its key.
    pub fn unlock(&self, passphrase: &str) -> Result<(), VaultError> {
        let key = match protect::protection(&*self.keystore)? {
            Protection::DeviceOnly => protect::open_device_key(&*self.keystore, None)?,
            Protection::Passphrase => {
                protect::open_device_key(&*self.keystore, Some(passphrase.as_bytes()))?
            }
        };
        self.cipher.unlock(&key)?;
        self.inner().last_used = Instant::now();
        Ok(())
    }

    /// Protects the device key with a passphrase; the vault stays unlocked.
    pub fn set_passphrase(&self, passphrase: &str) -> Result<(), VaultError> {
        if protect::protection(&*self.keystore)? == Protection::Passphrase {
            return Err(ProtectError::AlreadySet.into());
        }
        let key = protect::open_device_key(&*self.keystore, None)?;
        protect::set_passphrase(&*self.keystore, passphrase.as_bytes(), self.kdf)?;
        self.cipher.unlock(&key)?;
        self.inner().last_used = Instant::now();
        Ok(())
    }

    pub fn change_passphrase(&self, old: &str, new: &str) -> Result<(), VaultError> {
        protect::change_passphrase(&*self.keystore, old.as_bytes(), new.as_bytes(), self.kdf)?;
        self.inner().last_used = Instant::now();
        Ok(())
    }

    /// Back to keychain-only protection; `passphrase` must be right. The
    /// vault is unlocked afterwards.
    pub fn remove_passphrase(&self, passphrase: &str) -> Result<(), VaultError> {
        let key = protect::open_device_key(&*self.keystore, Some(passphrase.as_bytes()))?;
        protect::remove_passphrase(&*self.keystore, passphrase.as_bytes())?;
        self.cipher.unlock(&key)?;
        self.inner().last_used = Instant::now();
        Ok(())
    }

    // ---- secrets of hosts ------------------------------------------------

    /// What `host_id` has saved, without opening the vault.
    pub fn flags(&self, store: &Store, host_id: &str) -> Result<SecretFlags, VaultError> {
        Ok(store.host_secret_flags(host_id)?)
    }

    /// The saved secrets of a host. Opens the vault when it can do so
    /// without asking (see [`Vault::ensure_unlocked`]).
    pub fn host_secrets(&self, store: &Store, host_id: &str) -> Result<StoredSecrets, VaultError> {
        if !store.host_secret_flags(host_id)?.any() {
            return Ok(StoredSecrets::default());
        }
        self.ensure_unlocked()?;
        Ok(store.host_secrets(host_id)?)
    }

    pub fn set_host_secrets(
        &self,
        store: &Store,
        host_id: &str,
        update: &SecretsUpdate,
    ) -> Result<(), VaultError> {
        if update.is_empty() {
            return Ok(());
        }
        self.ensure_unlocked()?;
        Ok(store.set_host_secrets(host_id, update)?)
    }

    /// Deletes every saved secret. Needs no key, so it works while locked.
    pub fn forget_all(&self, store: &Store) -> Result<usize, VaultError> {
        Ok(store.forget_all_secrets()?)
    }
}
