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

    /// The login password, e.g. `"pwuser@example.com's password"`.
    async fn password(&self, prompt: &str) -> Option<String>;

    /// One keyboard-interactive round. Return one answer per prompt.
    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[KbdPrompt],
    ) -> Option<Vec<String>>;

    /// The passphrase of an encrypted identity file.
    async fn passphrase(&self, key_path: &Path) -> Option<String>;
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
