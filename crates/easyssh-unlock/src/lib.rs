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
//! * FIDO2 security keys (`easyssh_vault::SecurityKeyProvider`): the
//!   `hmac-secret` extension over USB HID on Linux and macOS (`fido_hid.rs`),
//!   through the Windows WebAuthn API on Windows (`fido_windows.rs`).
//! * Linux: no OS gate; security keys and the passphrase unlock.
//!
//! Neither gate can run headless, so CI only compiles them; the manual smoke
//! test in `RELEASING.md` covers the prompts.

use easyssh_crypto::KeyStore;
use easyssh_vault::{HmacAuthenticator, SecurityKeyProvider, UnlockProvider, Vault};
use std::sync::Arc;

mod fido_common;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod fido_hid;
#[cfg(windows)]
mod fido_windows;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows_hello;

/// The FIDO2 `hmac-secret` backend of this platform: USB HID on Linux and
/// macOS, the Windows WebAuthn API on Windows, none elsewhere (the method is
/// then simply not offered).
pub fn default_authenticator() -> Option<Arc<dyn HmacAuthenticator>> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    return Some(Arc::new(fido_hid::HidAuthenticator));
    #[cfg(windows)]
    return Some(Arc::new(fido_windows::WebAuthnAuthenticator));
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    return None;
}

/// The unlock providers this operating system can offer, storing their key
/// copies in `keystore`: Windows Hello or Touch ID, and FIDO2 security keys.
#[allow(unused_variables, unused_mut)]
pub fn os_providers(keystore: Arc<dyn KeyStore>) -> Vec<Arc<dyn UnlockProvider>> {
    let mut providers: Vec<Arc<dyn UnlockProvider>> = Vec::new();
    #[cfg(windows)]
    providers.push(windows_hello::provider(keystore.clone()));
    #[cfg(target_os = "macos")]
    providers.push(macos::provider(keystore.clone()));
    if let Some(authenticator) = default_authenticator() {
        providers.push(Arc::new(SecurityKeyProvider::new(authenticator, keystore)));
    }
    providers
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
            assert_eq!(names, ["windows_hello", "security_key"]);
        } else if cfg!(target_os = "macos") {
            assert_eq!(names, ["touch_id", "security_key"]);
        } else {
            assert_eq!(names, ["security_key"]);
        }
        // Nothing is turned on until the user does it.
        assert!(providers.iter().all(|p| !p.enrolled()));
    }
}
