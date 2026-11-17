//! Windows Hello and Touch ID as ways to unlock the vault.
//!
//! The logic (what is stored, in which order methods are tried, the
//! fallback to the passphrase) lives in `easyssh_vault::GatedKeyProvider` and
//! is tested there with a fake gate. This crate only supplies the thin,
//! platform-specific `PresenceGate`s:
//!
//! * Windows: `UserConsentVerifier` (Windows Hello: face, fingerprint or
//!   PIN). The device-key copy sits in Credential Manager, which encrypts it
//!   with the user's DPAPI key.
//! * macOS: `LAContext` with the biometrics policy (Touch ID). The copy sits
//!   in the login keychain.
//! * Linux: no provider. The passphrase is the only way to unlock.
//!
//! Neither gate can run headless, so CI only compiles them; the manual smoke
//! test in `RELEASING.md` covers the prompts.

use easyssh_crypto::KeyStore;
use easyssh_vault::{UnlockProvider, Vault};
use std::sync::Arc;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows_hello;

/// The unlock providers this operating system can offer, storing their key
/// copies in `keystore`. Empty on Linux.
#[allow(unused_variables)]
pub fn os_providers(keystore: Arc<dyn KeyStore>) -> Vec<Arc<dyn UnlockProvider>> {
    #[cfg(windows)]
    return vec![windows_hello::provider(keystore)];
    #[cfg(target_os = "macos")]
    return vec![macos::provider(keystore)];
    #[cfg(not(any(windows, target_os = "macos")))]
    return Vec::new();
}

/// Registers [`os_providers`] over the operating system keychain.
pub fn attach_os_providers(vault: &Vault) {
    let keystore: Arc<dyn KeyStore> = Arc::new(easyssh_crypto::OsKeyStore::new());
    for provider in os_providers(keystore) {
        vault.add_unlock_provider(provider);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_crypto::MemoryKeyStore;

    #[test]
    fn the_providers_match_the_platform() {
        let providers = os_providers(Arc::new(MemoryKeyStore::new()));
        let names: Vec<_> = providers.iter().map(|p| p.name()).collect();
        if cfg!(windows) {
            assert_eq!(names, ["windows_hello"]);
        } else if cfg!(target_os = "macos") {
            assert_eq!(names, ["touch_id"]);
        } else {
            assert!(names.is_empty());
        }
        // Nothing is turned on until the user does it.
        assert!(providers.iter().all(|p| !p.enrolled()));
    }
}
