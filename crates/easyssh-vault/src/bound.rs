//! An [`UnlockProvider`] whose copy of the device key cannot be released
//! without the sensor.
//!
//! [`GatedKeyProvider`](crate::GatedKeyProvider) puts a presence check in
//! front of an ordinary key store item: the item itself is readable by any
//! code that runs as the user. On a phone the operating system can do better.
//! The copy is wrapped by a key that lives in the secure element (an Android
//! Keystore AES key created with user authentication required, an iOS Keychain
//! item with a `biometryCurrentSet` access control), and the element refuses to
//! use that key until the sensor has matched. Skipping the app's UI gets an
//! attacker nothing: there is no plaintext to read.
//!
//! A [`BoundStore`] is that platform piece. Everything else (what is stored,
//! when a prompt is raised, what a cancelled or invalidated key means, the
//! fallback to the passphrase) is here and is tested with a fake store.
//!
//! Enrolling a new fingerprint or face invalidates the platform key, which
//! destroys the copy. That is why this provider only ever holds a *copy* of
//! the device key: the passphrase-wrapped original stays in the key store, and
//! the passphrase keeps working.

use crate::{UnlockProvider, VaultError};
use easyssh_crypto::DeviceKey;
use std::sync::Arc;
use zeroize::Zeroizing;

/// Why a sensor-bound operation did not finish.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundError {
    /// The user dismissed the prompt or chose the passphrase.
    Cancelled,
    /// The platform key is gone for good (a fingerprint or face was added,
    /// the lock screen was removed). The stored copy cannot be opened again.
    Invalidated,
    /// No sensor, none enrolled, or locked out for now.
    Unavailable(String),
    /// Anything else. The text is the platform's, never a secret.
    Failed(String),
}

impl std::fmt::Display for BoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BoundError::Cancelled => f.write_str("cancelled"),
            BoundError::Invalidated => f.write_str("the biometric key was invalidated"),
            BoundError::Unavailable(why) => write!(f, "unavailable: {why}"),
            BoundError::Failed(why) => f.write_str(why),
        }
    }
}

/// Secrets that only the sensor can release, kept by the platform.
pub trait BoundStore: Send + Sync {
    /// A sensor is present and at least one fingerprint or face is enrolled.
    fn available(&self) -> bool;
    /// An item is stored under `account`. Never raises a prompt.
    fn contains(&self, account: &str) -> bool;
    /// Stores `secret` under `account`, behind the sensor. Raises the system
    /// prompt (`reason` is its text) so a method that does not work is never
    /// turned on.
    fn seal(&self, account: &str, secret: &[u8], reason: &str) -> Result<(), BoundError>;
    /// Raises the system prompt and, when the sensor matches, returns the
    /// secret. `Ok(None)`: nothing is stored.
    fn open(&self, account: &str, reason: &str) -> Result<Option<Zeroizing<Vec<u8>>>, BoundError>;
    /// Deletes the item and its platform key. Nothing stored is not an error.
    fn remove(&self, account: &str) -> Result<(), BoundError>;
}

/// The device key behind the sensor, as an unlock method of the vault.
pub struct BoundKeyProvider {
    name: &'static str,
    account: &'static str,
    reason_enable: &'static str,
    reason_unlock: &'static str,
    store: Arc<dyn BoundStore>,
}

impl BoundKeyProvider {
    pub fn new(name: &'static str, account: &'static str, store: Arc<dyn BoundStore>) -> Self {
        Self {
            name,
            account,
            reason_enable: "Turn on biometric unlock for the vault",
            reason_unlock: "Unlock the vault",
            store,
        }
    }

    /// The texts of the system prompt. They are shown by the operating
    /// system, so the app cannot translate them at the time of the call.
    pub fn with_reasons(mut self, enable: &'static str, unlock: &'static str) -> Self {
        self.reason_enable = enable;
        self.reason_unlock = unlock;
        self
    }
}

impl UnlockProvider for BoundKeyProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn available(&self) -> bool {
        self.store.available()
    }

    fn enrolled(&self) -> bool {
        self.store.contains(self.account)
    }

    fn enroll(&self, key: &DeviceKey) -> Result<(), VaultError> {
        match self
            .store
            .seal(self.account, key.expose(), self.reason_enable)
        {
            Ok(()) => Ok(()),
            Err(BoundError::Cancelled) => Err(VaultError::UnlockDeclined),
            Err(other) => Err(VaultError::UnlockFailed(other.to_string())),
        }
    }

    fn unenroll(&self) -> Result<(), VaultError> {
        self.store
            .remove(self.account)
            .map_err(|error| VaultError::UnlockFailed(error.to_string()))
    }

    fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError> {
        // Look first: nothing stored, no prompt.
        if !self.store.contains(self.account) {
            return Ok(None);
        }
        match self.store.open(self.account, self.reason_unlock) {
            // A damaged copy is not something the user can fix by trying
            // again; the passphrase still works.
            Ok(Some(bytes)) => Ok(DeviceKey::from_slice(&bytes).ok()),
            // It was there a moment ago and the platform no longer has it
            // (the Keychain drops an item when the biometric set changes).
            Ok(None) => {
                let _ = self.store.remove(self.account);
                Ok(None)
            }
            Err(BoundError::Cancelled | BoundError::Unavailable(_)) => Ok(None),
            Err(BoundError::Invalidated) => {
                // The copy can never be opened again; forget it so the
                // settings show the method as off and it can be turned on
                // again with the new fingerprint.
                let _ = self.store.remove(self.account);
                Ok(None)
            }
            Err(BoundError::Failed(why)) => Err(VaultError::UnlockFailed(why)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Fake {
        item: Mutex<Option<Vec<u8>>>,
        next: Mutex<Option<BoundError>>,
        prompts: Mutex<Vec<String>>,
    }

    impl Fake {
        fn new() -> Arc<Self> {
            Arc::new(Self::default())
        }

        fn fail_next(&self, error: BoundError) {
            *self.next.lock().unwrap() = Some(error);
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }
    }

    impl BoundStore for Fake {
        fn available(&self) -> bool {
            true
        }

        fn contains(&self, _: &str) -> bool {
            self.item.lock().unwrap().is_some()
        }

        fn seal(&self, _: &str, secret: &[u8], reason: &str) -> Result<(), BoundError> {
            self.prompts.lock().unwrap().push(reason.to_owned());
            if let Some(error) = self.next.lock().unwrap().take() {
                return Err(error);
            }
            *self.item.lock().unwrap() = Some(secret.to_vec());
            Ok(())
        }

        fn open(&self, _: &str, reason: &str) -> Result<Option<Zeroizing<Vec<u8>>>, BoundError> {
            self.prompts.lock().unwrap().push(reason.to_owned());
            if let Some(error) = self.next.lock().unwrap().take() {
                return Err(error);
            }
            Ok(self.item.lock().unwrap().clone().map(Zeroizing::new))
        }

        fn remove(&self, _: &str) -> Result<(), BoundError> {
            *self.item.lock().unwrap() = None;
            Ok(())
        }
    }

    fn provider(fake: &Arc<Fake>) -> BoundKeyProvider {
        BoundKeyProvider::new("biometric", "bio/v1", fake.clone())
            .with_reasons("enable it", "unlock it")
    }

    #[test]
    fn enrolling_stores_the_key_behind_the_sensor_and_unlocking_returns_it() {
        let fake = Fake::new();
        let provider = provider(&fake);
        let key = DeviceKey::generate().unwrap();
        assert!(provider.available());
        assert!(!provider.enrolled());
        provider.enroll(&key).unwrap();
        assert!(provider.enrolled());
        assert_eq!(provider.try_unlock().unwrap().unwrap(), key);
        assert_eq!(fake.prompts(), ["enable it", "unlock it"]);
    }

    #[test]
    fn nothing_enrolled_means_no_prompt() {
        let fake = Fake::new();
        assert!(provider(&fake).try_unlock().unwrap().is_none());
        assert!(fake.prompts().is_empty());
    }

    #[test]
    fn a_cancelled_enrolment_turns_nothing_on() {
        let fake = Fake::new();
        fake.fail_next(BoundError::Cancelled);
        let provider = provider(&fake);
        let error = provider
            .enroll(&DeviceKey::generate().unwrap())
            .unwrap_err();
        assert!(matches!(error, VaultError::UnlockDeclined));
        assert!(!provider.enrolled());
    }

    #[test]
    fn a_cancelled_unlock_falls_back_without_an_error() {
        let fake = Fake::new();
        let provider = provider(&fake);
        provider.enroll(&DeviceKey::generate().unwrap()).unwrap();
        fake.fail_next(BoundError::Cancelled);
        assert!(provider.try_unlock().unwrap().is_none());
        // Still enrolled: cancelling once does not turn the method off.
        assert!(provider.enrolled());
    }

    #[test]
    fn an_invalidated_key_is_forgotten_and_the_passphrase_remains() {
        let fake = Fake::new();
        let provider = provider(&fake);
        provider.enroll(&DeviceKey::generate().unwrap()).unwrap();
        fake.fail_next(BoundError::Invalidated);
        assert!(provider.try_unlock().unwrap().is_none());
        assert!(!provider.enrolled());
    }

    #[test]
    fn lockout_falls_back_but_real_failures_are_reported() {
        let fake = Fake::new();
        let provider = provider(&fake);
        provider.enroll(&DeviceKey::generate().unwrap()).unwrap();
        fake.fail_next(BoundError::Unavailable("too many attempts".into()));
        assert!(provider.try_unlock().unwrap().is_none());
        fake.fail_next(BoundError::Failed("hardware error".into()));
        let error = provider.try_unlock().unwrap_err();
        assert!(matches!(error, VaultError::UnlockFailed(why) if why == "hardware error"));
    }

    #[test]
    fn a_damaged_copy_is_not_a_key() {
        let fake = Fake::new();
        *fake.item.lock().unwrap() = Some(b"short".to_vec());
        assert!(provider(&fake).try_unlock().unwrap().is_none());
    }

    #[test]
    fn a_copy_the_platform_dropped_is_forgotten() {
        /// Says it has an item, then finds none (the Keychain does this after
        /// the set of fingerprints changed).
        struct Ghost(Mutex<bool>);
        impl BoundStore for Ghost {
            fn available(&self) -> bool {
                true
            }
            fn contains(&self, _: &str) -> bool {
                *self.0.lock().unwrap()
            }
            fn seal(&self, _: &str, _: &[u8], _: &str) -> Result<(), BoundError> {
                Ok(())
            }
            fn open(&self, _: &str, _: &str) -> Result<Option<Zeroizing<Vec<u8>>>, BoundError> {
                Ok(None)
            }
            fn remove(&self, _: &str) -> Result<(), BoundError> {
                *self.0.lock().unwrap() = false;
                Ok(())
            }
        }
        let ghost = Arc::new(Ghost(Mutex::new(true)));
        let provider = BoundKeyProvider::new("biometric", "bio/v1", ghost.clone());
        assert!(provider.enrolled());
        assert!(provider.try_unlock().unwrap().is_none());
        assert!(!provider.enrolled());
    }

    #[test]
    fn unenrolling_removes_the_copy_and_is_harmless_twice() {
        let fake = Fake::new();
        let provider = provider(&fake);
        provider.enroll(&DeviceKey::generate().unwrap()).unwrap();
        provider.unenroll().unwrap();
        provider.unenroll().unwrap();
        assert!(!provider.enrolled());
    }
}
