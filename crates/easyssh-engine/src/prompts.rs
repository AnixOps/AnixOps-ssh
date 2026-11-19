//! The engine's questions to the person holding the phone: login passwords,
//! key passphrases, keyboard-interactive rounds and host keys.
//!
//! A question is sent as an event (`askpass.prompt`, `hostkey.prompt`, the same
//! payloads the desktop daemon sends, so the React dialogs are shared) and
//! answered by a command (`askpass_reply`, `hostkey_reply`). Nothing is stored
//! or logged. An unanswered question is a cancellation after [`ANSWER_TIMEOUT`];
//! an unanswered host key is a rejection, and so is a host key nobody was
//! attached to see.
//!
//! [`HostKeyPrompts`] is shared with the desktop daemon; the secret questions
//! of the daemon go through its own askpass hub (which also serves OpenSSH).

use crate::events::Events;
use crate::login::SavedPassword;
use async_trait::async_trait;
use easyssh_ssh::known_hosts::KnownHostStatus;
use easyssh_ssh::{Decision, HostKeyDetails, Interaction, KbdPrompt};
use serde_json::json;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::oneshot;

pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Open host key questions.
pub struct HostKeyPrompts {
    events: Events,
    pending: Mutex<HashMap<u64, oneshot::Sender<Decision>>>,
    timeout: Duration,
}

/// Parses the `decision` of `hostkey.reply` / `hostkey_reply`. Anything unknown
/// is a rejection.
pub fn parse_decision(name: &str) -> Decision {
    match name {
        "accept_once" => Decision::AcceptOnce,
        "accept_and_save" => Decision::AcceptAndSave,
        "replace_and_save" => Decision::ReplaceAndSave,
        _ => Decision::Reject,
    }
}

impl HostKeyPrompts {
    pub fn new(events: Events) -> Arc<Self> {
        Self::with_timeout(events, ANSWER_TIMEOUT)
    }

    pub fn with_timeout(events: Events, timeout: Duration) -> Arc<Self> {
        Arc::new(HostKeyPrompts {
            events,
            pending: Mutex::new(HashMap::new()),
            timeout,
        })
    }

    /// Asks the clients (the first answer wins). With nobody attached the
    /// key is rejected at once.
    pub async fn ask(&self, id: u64, details: &HostKeyDetails) -> Decision {
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(id, sender);
        let (status, expected) = match &details.status {
            KnownHostStatus::Changed { expected, .. } => ("changed", Some(expected.clone())),
            _ => ("unknown", None),
        };
        let mut payload = json!({
            "id": id,
            "host": details.host,
            "port": details.port,
            "algorithm": details.algorithm,
            "fingerprint": details.fingerprint,
            "status": status,
        });
        if let Some(expected) = expected {
            payload["expected"] = json!(expected);
        }
        if self.events.emit("hostkey.prompt", payload) == 0 {
            lock(&self.pending).remove(&id);
            return Decision::Reject;
        }
        let decision = tokio::time::timeout(self.timeout, receiver)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(Decision::Reject);
        lock(&self.pending).remove(&id);
        self.events.emit("hostkey.resolved", json!({"id": id}));
        decision
    }

    /// `hostkey.reply`. Unknown or already answered ids are ignored.
    pub fn reply(&self, id: u64, decision: Decision) {
        if let Some(sender) = lock(&self.pending).remove(&id) {
            let _ = sender.send(decision);
        }
    }
}

pub struct Prompts {
    events: Events,
    next: AtomicU64,
    secrets: Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>,
    hostkeys: Arc<HostKeyPrompts>,
    timeout: Duration,
}

impl Prompts {
    pub fn new(events: Events) -> Arc<Self> {
        Self::with_timeout(events, ANSWER_TIMEOUT)
    }

    pub fn with_timeout(events: Events, timeout: Duration) -> Arc<Self> {
        Arc::new(Prompts {
            hostkeys: HostKeyPrompts::with_timeout(events.clone(), timeout),
            events,
            next: AtomicU64::new(1),
            secrets: Mutex::new(HashMap::new()),
            timeout,
        })
    }

    fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Asks for a password, passphrase or keyboard-interactive answer. `kind`
    /// is `password`, `passphrase` or `keyboard_interactive`.
    pub async fn ask_secret(
        &self,
        host: &str,
        prompt: &str,
        kind: &str,
        echo: bool,
    ) -> Option<String> {
        let id = self.next_id();
        let (sender, receiver) = oneshot::channel();
        lock(&self.secrets).insert(id, sender);
        let attached = self.events.emit(
            "askpass.prompt",
            json!({
                "id": id,
                "host": host,
                "prompt": prompt,
                "confirm": false,
                "kind": kind,
                "echo": echo,
                "rememberable": false,
            }),
        );
        let answer = if attached == 0 {
            None
        } else {
            tokio::time::timeout(self.timeout, receiver)
                .await
                .ok()
                .and_then(Result::ok)
                .flatten()
        };
        lock(&self.secrets).remove(&id);
        self.events.emit("askpass.resolved", json!({ "id": id }));
        answer
    }

    /// `askpass_reply`. `None` cancels. Unknown or answered ids are ignored.
    pub fn reply_secret(&self, id: u64, answer: Option<String>) {
        if let Some(sender) = lock(&self.secrets).remove(&id) {
            let _ = sender.send(answer);
        }
    }

    pub async fn ask_hostkey(&self, details: &HostKeyDetails) -> Decision {
        self.hostkeys.ask(self.next_id(), details).await
    }

    /// `hostkey_reply`.
    pub fn reply_hostkey(&self, id: u64, decision: &str) {
        self.hostkeys.reply(id, parse_decision(decision));
    }
}

/// The [`Interaction`] for one connection attempt; `label` names the host in
/// the dialogs.
pub struct PromptInteraction {
    pub prompts: Arc<Prompts>,
    pub label: String,
    /// The saved passwords of the host and of its jump hosts, each offered
    /// once, and only at the prompt of its own account.
    pub saved_passwords: Vec<Arc<SavedPassword>>,
}

#[async_trait]
impl Interaction for PromptInteraction {
    async fn verify_host_key(
        &self,
        host: &str,
        port: u16,
        key_fingerprint_sha256: &str,
        known: KnownHostStatus,
    ) -> Decision {
        self.verify_host_key_details(&HostKeyDetails {
            host: host.to_owned(),
            port,
            algorithm: String::new(),
            fingerprint: key_fingerprint_sha256.to_owned(),
            status: known,
        })
        .await
    }

    async fn verify_host_key_details(&self, details: &HostKeyDetails) -> Decision {
        self.prompts.ask_hostkey(details).await
    }

    async fn password(&self, prompt: &str) -> Option<String> {
        if let Some(saved) = self.saved_passwords.iter().find_map(|s| s.take(prompt)) {
            return Some(saved);
        }
        self.prompts
            .ask_secret(&self.label, prompt, "password", false)
            .await
    }

    async fn saved_password(&self, prompt: &str) -> Option<String> {
        // The same once-only answer as `password`: whichever of the two the
        // server asks through first uses it up.
        self.saved_passwords.iter().find_map(|s| s.take(prompt))
    }

    async fn keyboard_interactive(
        &self,
        name: &str,
        instructions: &str,
        prompts: &[KbdPrompt],
    ) -> Option<Vec<String>> {
        let mut answers = Vec::with_capacity(prompts.len());
        for (index, prompt) in prompts.iter().enumerate() {
            let mut text = prompt.text.clone();
            if index == 0 {
                let header: Vec<&str> = [name, instructions]
                    .into_iter()
                    .filter(|part| !part.trim().is_empty())
                    .collect();
                if !header.is_empty() {
                    text = format!("{}\n{text}", header.join("\n"));
                }
            }
            answers.push(
                self.prompts
                    .ask_secret(&self.label, &text, "keyboard_interactive", prompt.echo)
                    .await?,
            );
        }
        Some(answers)
    }

    async fn passphrase(&self, key_path: &Path) -> Option<String> {
        let prompt = format!("Enter passphrase for key '{}'", key_path.display());
        self.prompts
            .ask_secret(&self.label, &prompt, "passphrase", false)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    fn recording() -> (Events, Seen) {
        let seen: Seen = Arc::default();
        let sink = seen.clone();
        let emit: Events = Arc::new(move |name: &str, payload: Value| {
            lock(&sink).push((name.to_owned(), payload));
            1usize
        });
        (emit, seen)
    }

    async fn first_event(seen: &Seen) -> (String, Value) {
        loop {
            if let Some(event) = lock(seen).first().cloned() {
                return event;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_password_question_is_an_askpass_event_and_the_reply_answers_it() {
        let (emit, seen) = recording();
        let prompts = Prompts::new(emit);
        let asking = prompts.clone();
        let task = tokio::spawn(async move {
            asking
                .ask_secret("web", "me@h's password: ", "password", false)
                .await
        });
        let (name, payload) = first_event(&seen).await;
        assert_eq!(name, "askpass.prompt");
        assert_eq!(payload["host"], "web");
        assert_eq!(payload["kind"], "password");
        assert_eq!(payload["confirm"], false);
        prompts.reply_secret(payload["id"].as_u64().unwrap(), Some("pw".into()));
        assert_eq!(task.await.unwrap().as_deref(), Some("pw"));
        assert_eq!(lock(&seen)[1].0, "askpass.resolved");
    }

    #[tokio::test]
    async fn each_jump_hop_and_the_host_answer_their_own_password_prompt_from_the_vault() {
        use easyssh_crypto::SecretText;
        use easyssh_store::{SecretFlags, StoredSecrets};
        use easyssh_vault::{SecretSource, VaultError};

        struct Vault;
        impl SecretSource for Vault {
            fn host_secrets(&self, id: &str) -> Result<StoredSecrets, VaultError> {
                Ok(StoredSecrets {
                    password: Some(SecretText::new(format!("pw-of-{id}"))),
                    private_key: None,
                    passphrase: None,
                })
            }
            fn secret_flags(&self, _: &str) -> Result<SecretFlags, VaultError> {
                Ok(SecretFlags {
                    password: true,
                    private_key: false,
                    passphrase: false,
                })
            }
        }
        let vault: Arc<dyn easyssh_vault::SecretSource> = Arc::new(Vault);
        let saved = |account: &str, id: &str| {
            Arc::new(SavedPassword::new(account.into(), vault.clone(), id.into()))
        };
        let (emit, seen) = recording();
        let interaction = PromptInteraction {
            prompts: Prompts::new(emit),
            label: "inner".into(),
            saved_passwords: vec![saved("ops@bastion", "bastion"), saved("me@inner", "inner")],
        };
        // The jump host's prompt gets the jump host's password, the target's
        // prompt the target's; nobody is asked.
        assert_eq!(
            interaction
                .password("ops@bastion's password: ")
                .await
                .as_deref(),
            Some("pw-of-bastion")
        );
        assert_eq!(
            interaction
                .password("me@inner's password: ")
                .await
                .as_deref(),
            Some("pw-of-inner")
        );
        assert!(lock(&seen).is_empty());
    }

    #[tokio::test]
    async fn the_saved_password_answers_a_keyboard_interactive_password_round_once() {
        use easyssh_crypto::SecretText;
        use easyssh_store::{SecretFlags, StoredSecrets};
        use easyssh_vault::{SecretSource, VaultError};

        struct Vault;
        impl SecretSource for Vault {
            fn host_secrets(&self, _: &str) -> Result<StoredSecrets, VaultError> {
                Ok(StoredSecrets {
                    password: Some(SecretText::new("pw")),
                    private_key: None,
                    passphrase: None,
                })
            }
            fn secret_flags(&self, _: &str) -> Result<SecretFlags, VaultError> {
                Ok(SecretFlags {
                    password: true,
                    private_key: false,
                    passphrase: false,
                })
            }
        }
        let (emit, seen) = recording();
        let interaction = PromptInteraction {
            prompts: Prompts::with_timeout(emit, Duration::from_millis(20)),
            label: "inner".into(),
            saved_passwords: vec![Arc::new(SavedPassword::new(
                "me@inner".into(),
                Arc::new(Vault),
                "inner".into(),
            ))],
        };
        assert_eq!(
            interaction.saved_password("ops@bastion's password").await,
            None
        );
        assert_eq!(
            interaction
                .saved_password("me@inner's password")
                .await
                .as_deref(),
            Some("pw")
        );
        // Used once: the password method asks the user now.
        assert_eq!(
            interaction.saved_password("me@inner's password").await,
            None
        );
        assert_eq!(interaction.password("me@inner's password").await, None);
        assert_eq!(lock(&seen)[0].0, "askpass.prompt");
    }

    #[tokio::test]
    async fn an_unanswered_question_is_a_cancellation_and_an_unanswered_host_key_a_rejection() {
        let (emit, _) = recording();
        let prompts = Prompts::with_timeout(emit, Duration::from_millis(20));
        assert_eq!(prompts.ask_secret("h", "p", "password", false).await, None);
        let details = HostKeyDetails {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            status: KnownHostStatus::Unknown,
        };
        assert_eq!(prompts.ask_hostkey(&details).await, Decision::Reject);
    }

    #[tokio::test]
    async fn a_changed_key_reports_what_was_recorded_and_only_a_replacement_accepts_it() {
        let (emit, seen) = recording();
        let prompts = Prompts::new(emit);
        let asking = prompts.clone();
        let task = tokio::spawn(async move {
            asking
                .ask_hostkey(&HostKeyDetails {
                    host: "h".into(),
                    port: 2222,
                    algorithm: "ssh-ed25519".into(),
                    fingerprint: "SHA256:new".into(),
                    status: KnownHostStatus::Changed {
                        expected: vec!["SHA256:old".into()],
                        line: 3,
                    },
                })
                .await
        });
        let (name, payload) = first_event(&seen).await;
        assert_eq!(name, "hostkey.prompt");
        assert_eq!(payload["status"], "changed");
        assert_eq!(payload["expected"][0], "SHA256:old");
        let id = payload["id"].as_u64().unwrap();
        prompts.reply_hostkey(id, "replace_and_save");
        prompts.reply_hostkey(id, "reject"); // too late, ignored
        assert_eq!(task.await.unwrap(), Decision::ReplaceAndSave);
    }

    #[test]
    fn unknown_decisions_are_rejections() {
        assert_eq!(parse_decision("accept_once"), Decision::AcceptOnce);
        assert_eq!(parse_decision("accept_and_save"), Decision::AcceptAndSave);
        assert_eq!(parse_decision("whatever"), Decision::Reject);
    }

    fn details(status: KnownHostStatus) -> HostKeyDetails {
        HostKeyDetails {
            host: "h".into(),
            port: 22,
            algorithm: "ssh-ed25519".into(),
            fingerprint: "SHA256:abc".into(),
            status,
        }
    }

    #[tokio::test]
    async fn a_prompt_with_nobody_attached_is_rejected() {
        let prompts = HostKeyPrompts::new(Arc::new(|_: &str, _: Value| 0usize));
        assert_eq!(
            prompts.ask(1, &details(KnownHostStatus::Unknown)).await,
            Decision::Reject
        );
        // The same for a password: nobody can answer it.
        let secrets = Prompts::new(Arc::new(|_: &str, _: Value| 0usize));
        assert_eq!(secrets.ask_secret("h", "p", "password", false).await, None);
    }

    #[tokio::test]
    async fn the_first_reply_wins_and_every_client_hears_the_end() {
        let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let seen = events.clone();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = count.clone();
        let prompts = HostKeyPrompts::new(Arc::new(move |kind: &str, payload: Value| {
            counter.fetch_add(1, Ordering::SeqCst);
            lock(&seen).push((kind.to_owned(), payload));
            1usize
        }));
        let asking = prompts.clone();
        let task = tokio::spawn(async move {
            asking
                .ask(
                    7,
                    &details(KnownHostStatus::Changed {
                        expected: vec!["SHA256:old".into()],
                        line: 1,
                    }),
                )
                .await
        });
        while count.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        prompts.reply(7, parse_decision("replace_and_save"));
        prompts.reply(7, Decision::Reject); // too late, ignored
        assert_eq!(task.await.unwrap(), Decision::ReplaceAndSave);
        let events = lock(&events);
        assert_eq!(events[0].0, "hostkey.prompt");
        assert_eq!(events[0].1["status"], "changed");
        assert_eq!(events[0].1["expected"][0], "SHA256:old");
        assert_eq!(events[1], ("hostkey.resolved".to_owned(), json!({"id": 7})));
    }
}
