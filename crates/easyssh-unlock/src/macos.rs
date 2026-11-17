//! Touch ID through `LAContext`, in front of the login keychain.
//!
//! The biometrics-only policy is used on purpose: the "use password" button
//! of the other policy would be a second passphrase, and the vault already
//! has one.

#![allow(unsafe_code)]

use block2::RcBlock;
use easyssh_crypto::KeyStore;
use easyssh_vault::{GatedKeyProvider, PresenceGate, UnlockProvider, VaultError};
use objc2::runtime::Bool;
use objc2_foundation::{NSError, NSString};
use objc2_local_authentication::{LAContext, LAPolicy};
use std::sync::{mpsc, Arc};

const POLICY: LAPolicy = LAPolicy::DeviceOwnerAuthenticationWithBiometrics;

struct TouchId;

impl PresenceGate for TouchId {
    fn available(&self) -> bool {
        // SAFETY: a fresh context used on this thread only.
        unsafe {
            let context = LAContext::new();
            context.canEvaluatePolicy_error(POLICY).is_ok()
        }
    }

    fn verify(&self, reason: &str) -> Result<bool, VaultError> {
        let (sender, receiver) = mpsc::channel::<bool>();
        let reply = RcBlock::new(move |success: Bool, _error: *mut NSError| {
            let _ = sender.send(success.as_bool());
        });
        let reason = NSString::from_str(reason);
        // SAFETY: the reply block only owns a channel sender, which is Send,
        // and it is called at most once by the framework.
        unsafe {
            let context = LAContext::new();
            context.evaluatePolicy_localizedReason_reply(POLICY, &reason, &reply);
            // The context must outlive the evaluation; it does, because we
            // wait for the answer before it is released.
            receiver
                .recv()
                .map_err(|_| VaultError::UnlockFailed("Touch ID gave no answer".into()))
        }
    }
}

pub fn provider(keystore: Arc<dyn KeyStore>) -> Arc<dyn UnlockProvider> {
    Arc::new(
        GatedKeyProvider::new(
            "touch_id",
            "device-key-touch-id/v1",
            Arc::new(TouchId),
            keystore,
        )
        .with_reasons(
            "turn on Touch ID for the EasySSH vault",
            "unlock the EasySSH vault",
        ),
    )
}
