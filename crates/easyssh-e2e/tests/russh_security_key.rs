#![cfg(unix)]

//! `sk-*` keys against the real OpenSSH container. A real security key needs
//! hardware, but sshd verifies the signatures itself, so a key from the
//! software authenticator logs in the same way (the container authorizes the
//! public keys in `e2e/id_e2e_sk.pub`).

use async_trait::async_trait;
use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::sk::{e2e_security_keys, e2e_webauthn_ecdsa_key};
use easyssh_e2e::E2e;
use easyssh_ssh::{
    AuthMethod, Decision, Error, Interaction, KbdPrompt, KnownHostStatus, MemoryIdentity, Session,
};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// Trusts the host and records the touch prompts.
struct Touches {
    inner: Scripted,
    events: Mutex<Vec<String>>,
}

#[async_trait]
impl Interaction for Touches {
    async fn verify_host_key(
        &self,
        host: &str,
        port: u16,
        fingerprint: &str,
        known: KnownHostStatus,
    ) -> Decision {
        self.inner
            .verify_host_key(host, port, fingerprint, known)
            .await
    }
    async fn password(&self, prompt: &str) -> Option<String> {
        self.inner.password(prompt).await
    }
    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[KbdPrompt],
    ) -> Option<Vec<String>> {
        self.inner
            .keyboard_interactive(name, instructions, prompts)
            .await
    }
    async fn passphrase(&self, key_path: &Path) -> Option<String> {
        self.inner.passphrase(key_path).await
    }
    async fn security_key_touch(&self, key: &str) {
        self.events.lock().unwrap().push(format!("touch {key}"));
    }
    async fn security_key_touch_done(&self, key: &str) {
        self.events.lock().unwrap().push(format!("done {key}"));
    }
}

async fn login_with(label: &str, text: &str) -> (Session, Vec<String>) {
    let (device, _, _) = e2e_security_keys();
    login_on(device, label, text).await
}

async fn login_on(
    device: easyssh_ssh::security_key_software::SoftwareSecurityKey,
    label: &str,
    text: &str,
) -> (Session, Vec<String>) {
    let e2e = E2e::new();
    let mut params = e2e.russh_target().security_key(Arc::new(device));
    // Only the sk key: the regular test key must not be what gets in.
    params.identity_files.clear();
    params = params.memory_identity(MemoryIdentity::new(label, text.to_owned(), None));
    let interaction = Touches {
        inner: Scripted::trusting(),
        events: Mutex::new(Vec::new()),
    };
    let session = within("connect", Session::connect(params, &interaction))
        .await
        .expect("connect with the sk key");
    let events = interaction.events.lock().unwrap().clone();
    assert_eq!(session.auth_method(), &AuthMethod::StoredKey(label.into()));
    let output = within("exec", session.exec("id -un")).await.unwrap();
    assert_eq!(output.stdout_text(), "keyuser\n");
    (session, events)
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn ed25519_sk_key_from_the_software_authenticator_logs_in() {
    let (_, ed25519, _) = e2e_security_keys();
    let (_session, events) = login_with("e2e sk ed25519", &ed25519.private_text).await;
    assert_eq!(events, ["touch e2e sk ed25519", "done e2e sk ed25519"]);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn ecdsa_sk_key_from_the_software_authenticator_logs_in() {
    let (_, _, ecdsa) = e2e_security_keys();
    let (_session, events) = login_with("e2e sk ecdsa", &ecdsa.private_text).await;
    assert_eq!(events, ["touch e2e sk ecdsa", "done e2e sk ecdsa"]);
}

/// What the Windows backend sends: the `webauthn-sk-ecdsa-sha2-nistp256`
/// algorithm and signature, which sshd verifies against the same sk key.
#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn ecdsa_sk_key_with_webauthn_signatures_logs_in() {
    let (device, ecdsa) = e2e_webauthn_ecdsa_key();
    let (_session, events) = login_on(device, "e2e sk webauthn", &ecdsa.private_text).await;
    assert_eq!(events, ["touch e2e sk webauthn", "done e2e sk webauthn"]);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn an_sk_key_the_server_does_not_know_is_refused_without_a_touch() {
    let e2e = E2e::new();
    let (device, _, _) = e2e_security_keys();
    // A credential on the same device that the container never authorized.
    let stranger = device.create(
        easyssh_ssh::SkAlgorithm::Ed25519,
        "ssh:",
        easyssh_ssh::FLAG_USER_PRESENCE,
    );
    let mut params = e2e.russh_target().security_key(Arc::new(device));
    params.identity_files.clear();
    params = params.memory_identity(MemoryIdentity::new("stranger", stranger.private_text, None));
    let interaction = Touches {
        inner: Scripted::trusting(),
        events: Mutex::new(Vec::new()),
    };
    let error = within("connect", Session::connect(params, &interaction))
        .await
        .expect_err("the server does not know this key");
    assert!(matches!(error, Error::AuthFailed { .. }), "{error:?}");
    assert!(interaction.events.lock().unwrap().is_empty());
}
