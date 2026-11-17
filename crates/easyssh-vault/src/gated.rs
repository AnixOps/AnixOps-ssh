//! An [`UnlockProvider`] built from two small parts: a check that the user is
//! present (Windows Hello, Touch ID) and a key store that holds the copy of
//! the device key.
//!
//! The platform crate (`easyssh-unlock`) only supplies the [`PresenceGate`];
//! everything else, including how the copy is stored, is here and is tested
//! with a fake gate.
//!
//! The gate stands in front of the keychain read. It is not a hardware
//! boundary: code running as the same user could read the keychain item
//! without asking. The vault passphrase still protects the primary copy of
//! the key; this method trades some of that protection for convenience, which
//! is why it is opt-in.

use crate::{UnlockProvider, VaultError};
use easyssh_crypto::keystore::SERVICE;
use easyssh_crypto::{DeviceKey, KeyStore};
use std::sync::Arc;

/// Asks the operating system whether the person at the keyboard is the user.
pub trait PresenceGate: Send + Sync {
    /// The OS supports it and the user has set it up (a fingerprint or face
    /// is enrolled, a PIN exists).
    fn available(&self) -> bool;
    /// Shows the system prompt and blocks until the user answers. `Ok(false)`
    /// when the user cancelled or failed the check.
    fn verify(&self, reason: &str) -> Result<bool, VaultError>;
}

/// The device key kept in `keystore` under `account`, released only after
/// `gate` confirms the user.
pub struct GatedKeyProvider {
    name: &'static str,
    account: &'static str,
    reason_enable: &'static str,
    reason_unlock: &'static str,
    gate: Arc<dyn PresenceGate>,
    keystore: Arc<dyn KeyStore>,
}

impl GatedKeyProvider {
    pub fn new(
        name: &'static str,
        account: &'static str,
        gate: Arc<dyn PresenceGate>,
        keystore: Arc<dyn KeyStore>,
    ) -> Self {
        Self {
            name,
            account,
            reason_enable: "Turn on biometric unlock for the EasySSH vault",
            reason_unlock: "Unlock the EasySSH vault",
            gate,
            keystore,
        }
    }

    /// The texts of the system prompt (shown by the OS, so they cannot be
    /// translated by the app at the time of the call).
    pub fn with_reasons(mut self, enable: &'static str, unlock: &'static str) -> Self {
        self.reason_enable = enable;
        self.reason_unlock = unlock;
        self
    }
}

impl UnlockProvider for GatedKeyProvider {
    fn name(&self) -> &'static str {
        self.name
    }

    fn available(&self) -> bool {
        self.gate.available()
    }

    fn enrolled(&self) -> bool {
        matches!(self.keystore.get(SERVICE, self.account), Ok(Some(_)))
    }

    fn enroll(&self, key: &DeviceKey) -> Result<(), VaultError> {
        if !self.gate.verify(self.reason_enable)? {
            return Err(VaultError::UnlockDeclined);
        }
        self.keystore.set(SERVICE, self.account, key.expose())?;
        Ok(())
    }

    fn unenroll(&self) -> Result<(), VaultError> {
        Ok(self.keystore.delete(SERVICE, self.account)?)
    }

    fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError> {
        // Look first: no copy, no prompt.
        let Some(item) = self.keystore.get(SERVICE, self.account)? else {
            return Ok(None);
        };
        if !self.gate.verify(self.reason_unlock)? {
            return Ok(None);
        }
        // A damaged copy is not an error the user can fix by trying again;
        // the passphrase still works.
        Ok(DeviceKey::from_slice(&item).ok())
    }
}
