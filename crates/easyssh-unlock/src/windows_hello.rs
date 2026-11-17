//! Windows Hello through `UserConsentVerifier`, in front of Credential
//! Manager.

use easyssh_crypto::KeyStore;
use easyssh_vault::{GatedKeyProvider, PresenceGate, UnlockProvider, VaultError};
use std::sync::Arc;
use windows::core::HSTRING;
use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};

struct Hello;

impl PresenceGate for Hello {
    fn available(&self) -> bool {
        UserConsentVerifier::CheckAvailabilityAsync()
            .and_then(|operation| operation.join())
            .is_ok_and(|availability| availability == UserConsentVerifierAvailability::Available)
    }

    fn verify(&self, reason: &str) -> Result<bool, VaultError> {
        let result = UserConsentVerifier::RequestVerificationAsync(&HSTRING::from(reason))
            .and_then(|operation| operation.join())
            .map_err(|error| VaultError::UnlockFailed(error.message()))?;
        // Cancelled, retries exhausted, device busy: all mean "not verified";
        // the passphrase remains.
        Ok(result == UserConsentVerificationResult::Verified)
    }
}

pub fn provider(keystore: Arc<dyn KeyStore>) -> Arc<dyn UnlockProvider> {
    Arc::new(
        GatedKeyProvider::new(
            "windows_hello",
            "device-key-windows-hello/v1",
            Arc::new(Hello),
            keystore,
        )
        .with_reasons(
            "Turn on Windows Hello for the EasySSH vault",
            "Unlock the EasySSH vault",
        ),
    )
}
