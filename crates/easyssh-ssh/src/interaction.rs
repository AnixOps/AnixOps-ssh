//! The questions the engine asks the user (or the daemon acting for them).

use crate::known_hosts::KnownHostStatus;
use async_trait::async_trait;
use std::path::Path;

/// What to do with a host key the engine does not already trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Do not connect.
    Reject,
    /// Connect now, remember nothing (only for an unknown host).
    AcceptOnce,
    /// Connect and record the key (only for an unknown host).
    AcceptAndSave,
    /// The recorded key was changed on purpose: replace it and connect. This
    /// is the only way to accept a key that does not match what is
    /// recorded; every other answer to a changed key is a rejection.
    ReplaceAndSave,
}

/// Everything the engine knows about a host key it has to ask about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyDetails {
    pub host: String,
    pub port: u16,
    /// The key's algorithm, e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// `SHA256:...`
    pub fingerprint: String,
    /// `Unknown` or `Changed` (with the recorded fingerprints).
    pub status: KnownHostStatus,
}

/// One prompt of a keyboard-interactive round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KbdPrompt {
    pub text: String,
    /// Whether the answer may be shown while typing.
    pub echo: bool,
}

/// Implemented by the UI or the daemon. Every method is asked at most when
/// needed, and `None` means "the user cancelled", which skips that method.
#[async_trait]
pub trait Interaction: Send + Sync {
    /// Asked for an unknown host (`known` is `Unknown`) and for a host whose
    /// recorded key differs (`known` is `Changed`). Not asked when the key
    /// is already trusted. `key_fingerprint_sha256` looks like `SHA256:...`.
    async fn verify_host_key(
        &self,
        host: &str,
        port: u16,
        key_fingerprint_sha256: &str,
        known: KnownHostStatus,
    ) -> Decision;

    /// Like [`Interaction::verify_host_key`] with the key's algorithm and,
    /// for a changed key, the recorded fingerprints. The engine calls this
    /// one; the default forwards to `verify_host_key`, so implementations
    /// that do not need the details stay as they are.
    async fn verify_host_key_details(&self, details: &HostKeyDetails) -> Decision {
        self.verify_host_key(
            &details.host,
            details.port,
            &details.fingerprint,
            details.status.clone(),
        )
        .await
    }

    /// The login password, e.g. `"pwuser@example.com's password"`.
    async fn password(&self, prompt: &str) -> Option<String>;

    /// A saved login password for `prompt` (the same text as
    /// [`Interaction::password`] gets), used to answer a keyboard-interactive
    /// round that only asks for the password. `None`, the default, means
    /// nothing is saved (or it was used already), and the round goes to
    /// [`Interaction::keyboard_interactive`]. Must not ask the user.
    async fn saved_password(&self, _prompt: &str) -> Option<String> {
        None
    }

    /// One keyboard-interactive round. Return one answer per prompt.
    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[KbdPrompt],
    ) -> Option<Vec<String>>;

    /// The passphrase of an encrypted identity file.
    async fn passphrase(&self, key_path: &Path) -> Option<String>;

    /// The security key `key` (a label, never secret) is waiting to be
    /// touched. Shown until [`Interaction::security_key_touch_done`]. The
    /// default shows nothing.
    async fn security_key_touch(&self, _key: &str) {}

    /// The touch is over: the key signed, or the attempt failed.
    async fn security_key_touch_done(&self, _key: &str) {}

    /// The PIN of the security key `key`. `retries` is how many tries the
    /// device has left when it reported that. `None` cancels the key; the
    /// default cancels. The PIN is never logged or stored by the engine.
    async fn security_key_pin(&self, _key: &str, _retries: Option<u32>) -> Option<String> {
        None
    }
}

/// An [`Interaction`] that rejects unknown hosts and answers nothing. Useful
/// for non-interactive callers and as a base for tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAll;

#[async_trait]
impl Interaction for DenyAll {
    async fn verify_host_key(&self, _: &str, _: u16, _: &str, _: KnownHostStatus) -> Decision {
        Decision::Reject
    }
    async fn password(&self, _: &str) -> Option<String> {
        None
    }
    async fn keyboard_interactive(&self, _: &str, _: &str, _: &[KbdPrompt]) -> Option<Vec<String>> {
        None
    }
    async fn passphrase(&self, _: &Path) -> Option<String> {
        None
    }
}
