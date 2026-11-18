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
use std::time::{Duration, Instant, SystemTime};
use zeroize::Zeroizing;

/// Auto-lock after this many idle minutes unless the user chose otherwise.
pub const DEFAULT_AUTO_LOCK_MINUTES: u32 = 15;
/// Longest auto-lock the UI offers (a day); 0 means never.
pub const MAX_AUTO_LOCK_MINUTES: u32 = 24 * 60;
/// Settings key (in the app database) of the auto-lock minutes.
pub const AUTO_LOCK_SETTING: &str = "vault_auto_lock_minutes";
/// Settings key (in the app database) of the seconds a phone app may stay in
/// the background before the vault locks. 0 locks the moment it leaves.
pub const BACKGROUND_LOCK_SETTING: &str = "vault_background_lock_seconds";
/// Longest background grace period the UI offers (an hour).
pub const MAX_BACKGROUND_LOCK_SECONDS: u32 = 60 * 60;

#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("the vault is locked")]
    Locked,
    #[error("wrong vault passphrase")]
    WrongPassphrase,
    #[error("{0}")]
    Passphrase(&'static str),
    #[error("the unlock was cancelled")]
    UnlockDeclined,
    #[error("the system unlock failed: {0}")]
    UnlockFailed(String),
    #[error("the operating system keychain is not available: {0}")]
    KeychainUnavailable(String),
    #[error("keychain error: {0}")]
    KeyStore(String),
    #[error("{0}")]
    Input(String),
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
            VaultError::UnlockDeclined => "unlock_cancelled",
            VaultError::UnlockFailed(_) => "unlock_failed",
            VaultError::KeychainUnavailable(_) => "keychain_unavailable",
            VaultError::KeyStore(_) => "keychain_error",
            VaultError::Input(_) => "invalid_command",
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

/// A way to open a passphrase-protected vault without typing the passphrase:
/// Windows Hello, Touch ID, later FIDO2 keys.
///
/// A provider keeps its own copy of the device key behind something the
/// passphrase does not replace (a user-presence check in front of the OS
/// keychain). The copy is made by [`Vault::enable_unlock`], which needs the
/// passphrase once, and removed by [`Vault::disable_unlock`] or when the
/// passphrase is removed. The passphrase always stays a way in.
///
/// `try_unlock` may block while the user answers a system prompt; the vault
/// never calls it with its internal lock held.
pub trait UnlockProvider: Send + Sync {
    /// A stable identifier on the wire, for example `windows_hello`.
    fn name(&self) -> &'static str;
    /// Whether the operating system can offer this method right now
    /// (hardware present and set up). Unavailable providers are skipped and
    /// not shown in the UI.
    fn available(&self) -> bool {
        true
    }
    /// Whether the user turned this method on, that is, whether a copy of
    /// the device key is waiting behind it.
    fn enrolled(&self) -> bool {
        true
    }
    /// Stores the device key behind this method. Providers ask the user to
    /// prove presence first, so a method that does not work is never turned
    /// on.
    fn enroll(&self, _key: &DeviceKey) -> Result<(), VaultError> {
        Err(VaultError::Input(
            "this unlock method cannot be turned on".into(),
        ))
    }
    /// Removes the stored copy. Not being enrolled is not an error.
    fn unenroll(&self) -> Result<(), VaultError> {
        Ok(())
    }
    /// `Ok(None)` when the user declined or this provider could not unlock
    /// (the caller falls back to the next provider and then the passphrase).
    fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError>;

    /// The separately removable entries behind this method (the security
    /// keys of the FIDO2 method). Empty for methods that hold a single copy.
    fn items(&self) -> Vec<UnlockItem> {
        Vec::new()
    }
    /// Adds one more entry (another security key) holding the device key.
    /// Methods with a single copy do not support it.
    fn add_item(&self, _key: &DeviceKey, _label: &str) -> Result<UnlockItem, VaultError> {
        Err(VaultError::Input(
            "this unlock method holds a single entry".into(),
        ))
    }
    /// Removes one entry. An id that is not there is not an error.
    fn remove_item(&self, _id: &str) -> Result<(), VaultError> {
        Err(VaultError::Input(
            "this unlock method holds a single entry".into(),
        ))
    }
    /// Where the provider shows touch and PIN prompts that the system does
    /// not show itself. Called by [`Vault::set_unlock_prompt`].
    fn set_prompt(&self, _prompt: Arc<dyn UnlockPrompt>) {}
}

/// One removable entry of an [`UnlockProvider`], for example one security key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockItem {
    /// Stable identifier, never secret.
    pub id: String,
    /// The name the user gave it.
    pub label: String,
}

/// How a provider talks to the person while it waits for hardware: the
/// daemon shows the same touch and PIN dialogs as for FIDO2 SSH keys.
pub trait UnlockPrompt: Send + Sync {
    /// "Touch your security key." The notice stays up until the returned
    /// guard is dropped.
    fn touch(&self, label: &str) -> Box<dyn Send>;
    /// The PIN of the key `label`; `retries` is what the device says is left.
    /// `None` when the user cancelled.
    fn pin(&self, label: &str, retries: Option<u32>) -> Option<String>;
}

/// One entry of [`VaultStatus::unlock_methods`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnlockMethod {
    pub name: &'static str,
    /// The OS offers it; only available methods are shown.
    pub available: bool,
    /// The user turned it on.
    pub enabled: bool,
    /// The removable entries behind it (security keys); empty for the rest.
    pub items: Vec<UnlockItem>,
}

/// After a provider declined (or failed) on its own, it is not asked again
/// for this long, so a background secret use does not raise a prompt storm.
/// Explicit requests ([`Vault::unlock_with`]) ignore it.
pub const PROVIDER_COOLDOWN: Duration = Duration::from_secs(30);

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
    /// Biometric and OS unlock methods this vault knows about.
    pub unlock_methods: Vec<UnlockMethod>,
}

struct Inner {
    last_used: Instant,
    auto_lock: Option<Duration>,
    /// How long the app may be in the background before it locks (zero:
    /// at once).
    background_lock: Duration,
    /// When the app left the screen, on the wall clock: the monotonic clock
    /// stops while a phone sleeps, which is exactly when this matters.
    backgrounded_at: Option<SystemTime>,
    providers: Vec<Arc<dyn UnlockProvider>>,
    prompt: Option<Arc<dyn UnlockPrompt>>,
    /// When a provider last declined an automatic attempt.
    declined: Option<Instant>,
}

pub struct Vault {
    keystore: Arc<dyn KeyStore>,
    cipher: Arc<DeviceKeyCipher>,
    kdf: KdfParams,
    inner: Mutex<Inner>,
    /// Held while providers are asked, so two secret uses at once raise one
    /// prompt, not two.
    asking: Mutex<()>,
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
                background_lock: Duration::ZERO,
                backgrounded_at: None,
                providers: Vec::new(),
                prompt: None,
                declined: None,
            }),
            asking: Mutex::new(()),
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
        let prompt = self.inner().prompt.clone();
        if let Some(prompt) = prompt {
            provider.set_prompt(prompt);
        }
        self.inner().providers.push(provider);
    }

    /// Where providers show their touch and PIN prompts (also for providers
    /// added later).
    pub fn set_unlock_prompt(&self, prompt: Arc<dyn UnlockPrompt>) {
        let providers = {
            let mut inner = self.inner();
            inner.prompt = Some(prompt.clone());
            inner.providers.clone()
        };
        for provider in providers {
            provider.set_prompt(prompt.clone());
        }
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

    /// Loads the saved auto-lock and background-lock times from the app
    /// database.
    pub fn load_settings(&self, store: &Store) {
        if let Ok(Some(value)) = store.get_setting_as::<u32>(AUTO_LOCK_SETTING) {
            let _ = self.set_auto_lock_minutes(value);
        }
        if let Ok(Some(value)) = store.get_setting_as::<u32>(BACKGROUND_LOCK_SETTING) {
            let _ = self.set_background_lock_seconds(value);
        }
    }

    /// Seconds in the background before the vault locks; 0 is at once.
    pub fn background_lock_seconds(&self) -> u32 {
        self.inner().background_lock.as_secs() as u32
    }

    pub fn set_background_lock_seconds(&self, value: u32) -> Result<(), VaultError> {
        if value > MAX_BACKGROUND_LOCK_SECONDS {
            return Err(VaultError::Passphrase(
                "the background lock is limited to one hour",
            ));
        }
        self.inner().background_lock = Duration::from_secs(u64::from(value));
        Ok(())
    }

    /// Changes the background lock and saves it to the app database.
    pub fn save_background_lock(&self, store: &Store, value: u32) -> Result<(), VaultError> {
        self.set_background_lock_seconds(value)?;
        store.set_setting_as(BACKGROUND_LOCK_SETTING, &value)?;
        Ok(())
    }

    /// The app left the screen at `now`. With a background lock of zero the
    /// key is dropped now; otherwise the time is noted for
    /// [`Vault::returned_to_foreground`]. Returns whether it locked.
    ///
    /// While a system prompt is up (an unlock method is being asked) some
    /// phones report the app as inactive; locking then would cancel the
    /// user's own unlock, so nothing happens.
    pub fn entered_background(&self, now: SystemTime) -> bool {
        let Ok(_asking) = self.asking.try_lock() else {
            return false;
        };
        let mut inner = self.inner();
        inner.backgrounded_at.get_or_insert(now);
        if inner.background_lock.is_zero() && self.cipher.is_unlocked() {
            self.cipher.lock();
            return true;
        }
        false
    }

    /// The app is back at `now`. Locks when it was away for at least the
    /// background lock time, or when the clock went backwards (a changed
    /// clock must not keep a vault open). Returns whether it locked.
    pub fn returned_to_foreground(&self, now: SystemTime) -> bool {
        let mut inner = self.inner();
        let Some(since) = inner.backgrounded_at.take() else {
            return false;
        };
        let expired = match now.duration_since(since) {
            Ok(away) => away >= inner.background_lock,
            Err(_) => true,
        };
        if expired && self.cipher.is_unlocked() {
            self.cipher.lock();
            return true;
        }
        false
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
            unlock_methods: self.unlock_methods(),
        }
    }

    /// The registered unlock providers and their state.
    pub fn unlock_methods(&self) -> Vec<UnlockMethod> {
        let providers = self.inner().providers.clone();
        providers
            .iter()
            .map(|p| {
                let available = p.available();
                UnlockMethod {
                    name: p.name(),
                    available,
                    enabled: available && p.enrolled(),
                    items: if available { p.items() } else { Vec::new() },
                }
            })
            .collect()
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
    ///
    /// A passphrase vault asks its enabled unlock providers in order; when
    /// none opens it the answer is [`VaultError::Locked`] and the caller
    /// asks for the passphrase.
    pub fn ensure_unlocked(&self) -> Result<(), VaultError> {
        self.lock_if_idle(Instant::now());
        if !self.cipher.is_unlocked() {
            match protect::protection(&*self.keystore)? {
                Protection::DeviceOnly => {
                    let key = protect::open_device_key(&*self.keystore, None)?;
                    self.cipher.unlock(&key)?;
                }
                Protection::Passphrase => self.unlock_from_providers(None)?,
            }
        }
        self.inner().last_used = Instant::now();
        Ok(())
    }

    /// Asks the providers (only `only` when given) for the key. Without
    /// `only`, providers are not asked again for [`PROVIDER_COOLDOWN`] after
    /// one declined, and every failure becomes [`VaultError::Locked`].
    fn unlock_from_providers(&self, only: Option<&str>) -> Result<(), VaultError> {
        let _asking = self.asking.lock().unwrap_or_else(|e| e.into_inner());
        if self.cipher.is_unlocked() {
            return Ok(());
        }
        let providers = {
            let inner = self.inner();
            let cooling = inner
                .declined
                .is_some_and(|at| at.elapsed() < PROVIDER_COOLDOWN);
            if only.is_none() && cooling {
                return Err(VaultError::Locked);
            }
            inner.providers.clone()
        };
        let mut failure = None;
        let mut asked = false;
        for provider in providers
            .iter()
            .filter(|p| only.is_none_or(|name| p.name() == name))
            .filter(|p| p.available() && p.enrolled())
        {
            asked = true;
            match provider.try_unlock() {
                Ok(Some(key)) => {
                    self.cipher.unlock(&key)?;
                    self.inner().declined = None;
                    return Ok(());
                }
                Ok(None) => {}
                Err(error) => failure = Some(error),
            }
        }
        if asked {
            self.inner().declined = Some(Instant::now());
        }
        match only {
            None => Err(VaultError::Locked),
            Some(_) => Err(failure.unwrap_or(VaultError::UnlockDeclined)),
        }
    }

    /// Opens the vault with one unlock method, as the user asked by pressing
    /// its button. Raises the system prompt even when an earlier automatic
    /// attempt was declined.
    pub fn unlock_with(&self, method: &str) -> Result<(), VaultError> {
        self.provider(method)?;
        match protect::protection(&*self.keystore)? {
            Protection::DeviceOnly => self.ensure_unlocked(),
            Protection::Passphrase => {
                self.unlock_from_providers(Some(method))?;
                self.inner().last_used = Instant::now();
                Ok(())
            }
        }
    }

    fn provider(&self, method: &str) -> Result<Arc<dyn UnlockProvider>, VaultError> {
        self.inner()
            .providers
            .iter()
            .find(|p| p.name() == method)
            .cloned()
            .ok_or_else(|| VaultError::Input(format!("unknown unlock method {method:?}")))
    }

    /// Turns an unlock method on. Needs the vault passphrase once, to read
    /// the device key; the provider then checks the user's presence and
    /// stores its copy.
    pub fn enable_unlock(&self, method: &str, passphrase: &str) -> Result<(), VaultError> {
        let provider = self.provider(method)?;
        if !provider.available() {
            return Err(VaultError::Input(
                "this unlock method is not available on this device".into(),
            ));
        }
        if protect::protection(&*self.keystore)? != Protection::Passphrase {
            return Err(VaultError::Passphrase(
                "set a vault passphrase before turning on another unlock method",
            ));
        }
        let key = protect::open_device_key(&*self.keystore, Some(passphrase.as_bytes()))?;
        provider.enroll(&key)?;
        self.cipher.unlock(&key)?;
        let mut inner = self.inner();
        inner.declined = None;
        inner.last_used = Instant::now();
        Ok(())
    }

    /// Adds one entry to an unlock method that holds several (another
    /// security key). Needs the vault passphrase once, like
    /// [`Vault::enable_unlock`].
    pub fn add_unlock_item(
        &self,
        method: &str,
        passphrase: &str,
        label: &str,
    ) -> Result<UnlockItem, VaultError> {
        let provider = self.provider(method)?;
        if !provider.available() {
            return Err(VaultError::Input(
                "this unlock method is not available on this device".into(),
            ));
        }
        if protect::protection(&*self.keystore)? != Protection::Passphrase {
            return Err(VaultError::Passphrase(
                "set a vault passphrase before turning on another unlock method",
            ));
        }
        let key = protect::open_device_key(&*self.keystore, Some(passphrase.as_bytes()))?;
        let item = provider.add_item(&key, label)?;
        self.cipher.unlock(&key)?;
        let mut inner = self.inner();
        inner.declined = None;
        inner.last_used = Instant::now();
        Ok(item)
    }

    /// Removes one entry (one security key) of an unlock method.
    pub fn remove_unlock_item(&self, method: &str, id: &str) -> Result<(), VaultError> {
        self.provider(method)?.remove_item(id)
    }

    /// Turns an unlock method off and deletes its stored copy of the key.
    pub fn disable_unlock(&self, method: &str) -> Result<(), VaultError> {
        self.provider(method)?.unenroll()
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
        // Without a passphrase the keychain item opens the vault by itself;
        // the copies behind the other methods are stale now.
        let providers = self.inner().providers.clone();
        for provider in providers {
            let _ = provider.unenroll();
        }
        self.cipher.unlock(&key)?;
        self.inner().last_used = Instant::now();
        Ok(())
    }

    // ---- derived keys and single secret values ---------------------------

    /// A purpose-bound subkey of the device key (HKDF-SHA256 with `salt`).
    /// Opens the vault when it can do so without asking.
    pub fn derive_key(&self, info: &[u8], salt: &[u8]) -> Result<Zeroizing<[u8; 32]>, VaultError> {
        self.ensure_unlocked()?;
        Ok(self.cipher.derive_key(info, salt)?)
    }

    /// Encrypts one secret value (for example an API key). `aad` names where
    /// it is kept, so the blob cannot be moved to another slot.
    pub fn seal_value(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
        self.ensure_unlocked()?;
        Ok(self.cipher.encrypt(plaintext, aad)?)
    }

    /// Inverse of [`Vault::seal_value`].
    pub fn open_value(&self, aad: &[u8], blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        self.ensure_unlocked()?;
        Ok(Zeroizing::new(self.cipher.decrypt(blob, aad)?))
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
