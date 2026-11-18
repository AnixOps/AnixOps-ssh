//! The vault on Android and iOS.
//!
//! The vault itself (`easyssh-vault`) does not change: it asks a
//! [`KeyStore`](easyssh_crypto::KeyStore) for the device key and has
//! [`UnlockProvider`](easyssh_vault::UnlockProvider)s that can open a
//! passphrase-protected vault without the passphrase. This crate supplies the
//! mobile pieces over a [`NativeBridge`]:
//!
//! * [`BridgeKeyStore`]: the device key (or its passphrase-wrapped form) in
//!   the Android Keystore or the iOS Keychain;
//! * [`BridgeBoundStore`] behind a [`BoundKeyProvider`]: a copy of the device
//!   key that only a biometric match can release, the third unlock backend
//!   after Windows Hello and Touch ID;
//! * [`mobile_vault`]: both of them wired into a `Vault`;
//! * [`status_json`]: what the touch UI shows.
//!
//! The Kotlin and Swift implementations are in
//! `src-tauri/plugins/device-key`; this crate has no Tauri in it so the
//! logic is tested here with a fake bridge. See `docs/mobile-prototype.md`.

#![forbid(unsafe_code)]

mod bridge;
mod stores;

pub use bridge::{code, command, BridgeError, NativeBridge};
pub use easyssh_vault::{BoundError, BoundKeyProvider, BoundStore};
pub use stores::{BridgeBoundStore, BridgeKeyStore};

use easyssh_crypto::KdfParams;
use easyssh_store::Store;
use easyssh_vault::{Vault, VaultError};
use serde_json::{json, Value};
use std::sync::Arc;

/// The unlock method's name on the wire.
pub const BIOMETRIC: &str = "biometric";
/// Where the sensor-bound copy of the device key is kept.
pub const BIOMETRIC_ACCOUNT: &str = "device-key-biometric/v1";
/// Settings key: the first-run choice (passphrase or device only) was made.
pub const SETUP_SETTING: &str = "mobile_vault_setup";

/// The vault of this phone: the device key in the Keystore or Keychain, and
/// biometric unlock as an optional way in.
pub fn mobile_vault(bridge: Arc<dyn NativeBridge>) -> Arc<Vault> {
    mobile_vault_with(bridge, KdfParams::DESKTOP_V1)
}

/// Like [`mobile_vault`] with explicit Argon2id parameters (tests use weak ones).
pub fn mobile_vault_with(bridge: Arc<dyn NativeBridge>, kdf: KdfParams) -> Arc<Vault> {
    let vault = Arc::new(Vault::with_kdf(
        Arc::new(BridgeKeyStore::new(bridge.clone())),
        kdf,
    ));
    vault.add_unlock_provider(Arc::new(
        BoundKeyProvider::new(
            BIOMETRIC,
            BIOMETRIC_ACCOUNT,
            Arc::new(BridgeBoundStore::new(bridge)),
        )
        // Shown by the system prompt, so it cannot follow the app's language.
        .with_reasons("Turn on biometric unlock", "Unlock AnixOps SSH"),
    ));
    vault
}

/// Whether the person already chose how to protect the vault.
pub fn setup_done(store: &Store) -> bool {
    store
        .get_setting_as::<bool>(SETUP_SETTING)
        .ok()
        .flatten()
        .unwrap_or(false)
}

pub fn mark_setup_done(store: &Store) -> Result<(), VaultError> {
    store
        .set_setting_as(SETUP_SETTING, &true)
        .map_err(|error| VaultError::Store(error.to_string()))
}

/// The vault's state as the touch UI reads it (`VaultStatus` in
/// `src/lib/previewVault.ts`). It holds flags and numbers only.
pub fn status_json(vault: &Vault, setup_done: bool) -> Value {
    let status = vault.status();
    json!({
        "passphraseSet": status.passphrase_set,
        "unlocked": status.unlocked,
        "autoLockMinutes": status.auto_lock_minutes,
        "backgroundLockSeconds": vault.background_lock_seconds(),
        "keychainAvailable": status.keychain_available,
        "keychainError": status.keychain_error,
        "setupDone": setup_done,
        "unlockMethods": status.unlock_methods.iter().map(|method| json!({
            "name": method.name,
            "available": method.available,
            "enabled": method.enabled,
        })).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests;
