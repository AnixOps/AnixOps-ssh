//! [`KeyStore`] and [`BoundStore`] over a [`NativeBridge`].

use crate::bridge::{code, command, BridgeError, NativeBridge};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use easyssh_crypto::{KeyStore, KeyStoreError};
use easyssh_vault::{BoundError, BoundStore};
use serde_json::{json, Value};
use std::sync::Arc;
use zeroize::Zeroizing;

fn encode(secret: &[u8]) -> Zeroizing<String> {
    Zeroizing::new(STANDARD.encode(secret))
}

/// The `value` of a native answer: `None` for `null` or a missing field.
fn decode(answer: &Value) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    match answer.get("value") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => STANDARD
            .decode(text)
            .map(|bytes| Some(Zeroizing::new(bytes)))
            .map_err(|_| "the secure store returned a value that is not base64".to_owned()),
        Some(_) => Err("the secure store returned a value of the wrong type".to_owned()),
    }
}

/// The device key store of a phone: Android Keystore or iOS Keychain.
///
/// * Android: every item is encrypted with an AES-256-GCM key that lives in
///   the Keystore (hardware-backed, StrongBox when the phone has it) and
///   never leaves it; only the ciphertext is written to app storage.
/// * iOS: a Keychain generic password item with
///   `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, so it is not in backups
///   and not readable while the phone is locked.
///
/// The secret itself does pass through this process (the vault needs the
/// device key to encrypt records), as base64 inside the bridge call; it is
/// held in [`Zeroizing`] buffers on this side.
pub struct BridgeKeyStore {
    bridge: Arc<dyn NativeBridge>,
}

impl BridgeKeyStore {
    pub fn new(bridge: Arc<dyn NativeBridge>) -> Self {
        Self { bridge }
    }

    fn call(&self, name: &str, args: Value) -> Result<Value, KeyStoreError> {
        self.bridge.call(name, args).map_err(|error| {
            if error.code == code::UNAVAILABLE {
                KeyStoreError::Unavailable(error.to_string())
            } else {
                KeyStoreError::Backend(error.to_string())
            }
        })
    }
}

impl std::fmt::Debug for BridgeKeyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeKeyStore").finish_non_exhaustive()
    }
}

impl KeyStore for BridgeKeyStore {
    fn get(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, KeyStoreError> {
        let answer = self.call(
            command::SECURE_GET,
            json!({"service": service, "account": account}),
        )?;
        decode(&answer).map_err(KeyStoreError::Backend)
    }

    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeyStoreError> {
        let value = encode(secret);
        self.call(
            command::SECURE_SET,
            json!({"service": service, "account": account, "value": value.as_str()}),
        )
        .map(|_| ())
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), KeyStoreError> {
        self.call(
            command::SECURE_DELETE,
            json!({"service": service, "account": account}),
        )
        .map(|_| ())
    }
}

fn bound_error(error: BridgeError) -> BoundError {
    match error.code.as_str() {
        code::CANCELLED => BoundError::Cancelled,
        code::INVALIDATED => BoundError::Invalidated,
        code::UNAVAILABLE => BoundError::Unavailable(error.to_string()),
        _ => BoundError::Failed(error.to_string()),
    }
}

/// Secrets only the sensor can release: an Android Keystore key created with
/// user authentication required (every use needs a fresh biometric match,
/// through a `BiometricPrompt.CryptoObject`), or an iOS Keychain item with a
/// `biometryCurrentSet` access control.
pub struct BridgeBoundStore {
    bridge: Arc<dyn NativeBridge>,
}

impl BridgeBoundStore {
    pub fn new(bridge: Arc<dyn NativeBridge>) -> Self {
        Self { bridge }
    }
}

impl std::fmt::Debug for BridgeBoundStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeBoundStore").finish_non_exhaustive()
    }
}

impl BoundStore for BridgeBoundStore {
    fn available(&self) -> bool {
        self.bridge
            .call(command::BOUND_STATUS, json!({}))
            .ok()
            .and_then(|answer| answer["available"].as_bool())
            .unwrap_or(false)
    }

    fn contains(&self, account: &str) -> bool {
        self.bridge
            .call(command::BOUND_CONTAINS, json!({"account": account}))
            .ok()
            .and_then(|answer| answer["present"].as_bool())
            .unwrap_or(false)
    }

    fn seal(&self, account: &str, secret: &[u8], reason: &str) -> Result<(), BoundError> {
        let value = encode(secret);
        self.bridge
            .call(
                command::BOUND_SEAL,
                json!({"account": account, "value": value.as_str(), "reason": reason}),
            )
            .map(|_| ())
            .map_err(bound_error)
    }

    fn open(&self, account: &str, reason: &str) -> Result<Option<Zeroizing<Vec<u8>>>, BoundError> {
        let answer = self
            .bridge
            .call(
                command::BOUND_OPEN,
                json!({"account": account, "reason": reason}),
            )
            .map_err(bound_error)?;
        decode(&answer).map_err(BoundError::Failed)
    }

    fn remove(&self, account: &str) -> Result<(), BoundError> {
        self.bridge
            .call(command::BOUND_REMOVE, json!({"account": account}))
            .map(|_| ())
            .map_err(bound_error)
    }
}
