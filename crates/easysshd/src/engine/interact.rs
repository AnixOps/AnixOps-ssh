//! The built-in engine's questions, asked through the attached clients:
//! passwords, passphrases and keyboard-interactive prompts reuse the
//! `askpass.prompt` dialog (with a `kind`), host keys use `hostkey.prompt`.
//!
//! Answers are held in memory only until they are handed to the engine, and
//! are never stored or logged. When nobody is attached, or nobody answers
//! within [`ANSWER_TIMEOUT`], the answer is "cancelled" (and a host key is
//! rejected).

use crate::askpass::{Hub, Notice, ANSWER_TIMEOUT};
use async_trait::async_trait;
use easyssh_ssh::known_hosts::KnownHostStatus;
use easyssh_ssh::{Decision, HostKeyDetails, Interaction, KbdPrompt};
use easyssh_vault::SecretSource;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::oneshot;

/// Sends an event to every attached client; returns how many took it.
pub type Broadcast = Arc<dyn Fn(&str, Value) -> usize + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Open host key questions.
pub struct HostKeyPrompts {
    broadcast: Broadcast,
    pending: Mutex<HashMap<u64, oneshot::Sender<Decision>>>,
    timeout: std::time::Duration,
}

impl HostKeyPrompts {
    pub fn new(broadcast: Broadcast) -> Arc<Self> {
        Arc::new(HostKeyPrompts {
            broadcast,
            pending: Mutex::new(HashMap::new()),
            timeout: ANSWER_TIMEOUT,
        })
    }

    /// Asks the clients (the first answer wins).
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
        if (self.broadcast)("hostkey.prompt", payload) == 0 {
            lock(&self.pending).remove(&id);
            return Decision::Reject;
        }
        let decision = tokio::time::timeout(self.timeout, receiver)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(Decision::Reject);
        lock(&self.pending).remove(&id);
        (self.broadcast)("hostkey.resolved", json!({"id": id}));
        decision
    }

    /// `hostkey.reply`. Unknown or already answered ids are ignored.
    pub fn reply(&self, id: u64, decision: Decision) {
        if let Some(sender) = lock(&self.pending).remove(&id) {
            let _ = sender.send(decision);
        }
    }
}

/// Parses the `decision` of `hostkey.reply`.
pub fn parse_decision(name: &str) -> Decision {
    match name {
        "accept_once" => Decision::AcceptOnce,
        "accept_and_save" => Decision::AcceptAndSave,
        "replace_and_save" => Decision::ReplaceAndSave,
        _ => Decision::Reject,
    }
}

/// The [`Interaction`] for one connection attempt; `label` names the host in
/// the dialogs.
pub struct DaemonInteraction {
    pub hub: Arc<Hub>,
    pub hostkeys: Arc<HostKeyPrompts>,
    pub label: String,
    /// The saved password of the target host, offered once when the server
    /// asks for exactly that user's password (never for a jump host, and
    /// never again after the server refused it).
    pub saved_password: Option<SavedPassword>,
    /// The "touch your security key" notice while one is shown.
    pub touch: Mutex<Option<Notice>>,
}

/// A saved password and the prompt it belongs to.
pub struct SavedPassword {
    /// `user@host`, which the engine's password prompt starts with.
    pub account: String,
    pub secrets: Arc<dyn SecretSource>,
    pub host_id: String,
    pub used: AtomicBool,
}

impl SavedPassword {
    fn take(&self, prompt: &str) -> Option<String> {
        if !prompt.starts_with(&format!("{}'s password", self.account)) {
            return None;
        }
        if self.used.swap(true, Ordering::SeqCst) {
            return None;
        }
        // A locked vault or a missing entry means: ask the user.
        let secrets = self.secrets.host_secrets(&self.host_id).ok()?;
        secrets
            .password
            .map(|password| password.expose().to_owned())
    }
}

#[async_trait]
impl Interaction for DaemonInteraction {
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
        self.hostkeys.ask(self.hub.next_id(), details).await
    }

    async fn password(&self, prompt: &str) -> Option<String> {
        if let Some(saved) = self.saved_password.as_ref().and_then(|s| s.take(prompt)) {
            return Some(saved);
        }
        self.hub
            .prompt(&self.label, prompt, "password", false)
            .await
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
                self.hub
                    .prompt(&self.label, &text, "keyboard_interactive", prompt.echo)
                    .await?,
            );
        }
        Some(answers)
    }

    async fn passphrase(&self, key_path: &Path) -> Option<String> {
        let prompt = format!("Enter passphrase for key '{}'", key_path.display());
        self.hub
            .prompt(&self.label, &prompt, "passphrase", false)
            .await
    }

    async fn security_key_touch(&self, key: &str) {
        let text = format!("Touch your security key ({key})");
        let notice = self.hub.notice(&self.label, &text, "security_key_touch");
        *lock(&self.touch) = notice;
    }

    async fn security_key_touch_done(&self, _key: &str) {
        // Dropping the notice closes the dialog on every client.
        lock(&self.touch).take();
    }

    async fn security_key_pin(&self, key: &str, retries: Option<u32>) -> Option<String> {
        let mut prompt = format!("Enter the PIN of your security key ({key})");
        if let Some(retries) = retries {
            prompt.push_str(&format!(" - {retries} tries left"));
        }
        self.hub
            .prompt(&self.label, &prompt, "security_key_pin", false)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixed(Option<&'static str>);

    impl SecretSource for Fixed {
        fn host_secrets(
            &self,
            _: &str,
        ) -> Result<easyssh_store::StoredSecrets, easyssh_vault::VaultError> {
            Ok(easyssh_store::StoredSecrets {
                password: self.0.map(easyssh_crypto::SecretText::new),
                ..Default::default()
            })
        }

        fn secret_flags(
            &self,
            _: &str,
        ) -> Result<easyssh_store::SecretFlags, easyssh_vault::VaultError> {
            Ok(easyssh_store::SecretFlags {
                password: self.0.is_some(),
                ..Default::default()
            })
        }
    }

    fn saved_with(password: Option<&'static str>) -> SavedPassword {
        SavedPassword {
            account: "me@example.com".into(),
            secrets: Arc::new(Fixed(password)),
            host_id: "h".into(),
            used: AtomicBool::new(false),
        }
    }

    #[test]
    fn a_saved_password_answers_only_its_own_prompt_and_only_once() {
        let saved = saved_with(Some("pw"));
        // Another user (or a jump host) is never answered from the vault.
        assert_eq!(saved.take("you@example.com's password"), None);
        assert_eq!(saved.take("me@other.com's password"), None);
        assert_eq!(
            saved.take("me@example.com's password").as_deref(),
            Some("pw")
        );
        // The server refused it: the next prompt goes to the user.
        assert_eq!(saved.take("me@example.com's password"), None);
        assert_eq!(saved_with(None).take("me@example.com's password"), None);
    }

    type Shown = Arc<Mutex<Vec<(u64, String, String, Option<&'static str>)>>>;

    /// An interaction whose hub records what it shows and what it closes.
    async fn recording() -> (Arc<DaemonInteraction>, Shown, Arc<Mutex<Vec<u64>>>) {
        let shown: Shown = Arc::default();
        let closed: Arc<Mutex<Vec<u64>>> = Arc::default();
        let (seen, ended) = (shown.clone(), closed.clone());
        let hub = Hub::bind(
            Arc::new(move |prompt| {
                lock(&seen).push((prompt.id, prompt.host, prompt.prompt, prompt.kind));
                true
            }),
            Arc::new(move |id| lock(&ended).push(id)),
        )
        .await
        .unwrap();
        let interaction = Arc::new(DaemonInteraction {
            hub,
            hostkeys: HostKeyPrompts::new(Arc::new(|_, _| 1)),
            label: "web".into(),
            saved_password: None,
            touch: Mutex::default(),
        });
        (interaction, shown, closed)
    }

    #[tokio::test]
    async fn touching_the_key_shows_a_notice_until_it_is_done() {
        let (interaction, shown, closed) = recording().await;
        interaction.security_key_touch("saved key of web").await;
        let first = lock(&shown)[0].clone();
        assert_eq!(first.1, "web");
        assert_eq!(first.3, Some("security_key_touch"));
        assert!(first.2.contains("Touch your security key"), "{first:?}");
        assert!(lock(&closed).is_empty());
        interaction
            .security_key_touch_done("saved key of web")
            .await;
        assert_eq!(*lock(&closed), vec![first.0]);
        // A second "done" closes nothing more.
        interaction
            .security_key_touch_done("saved key of web")
            .await;
        assert_eq!(lock(&closed).len(), 1);
    }

    #[tokio::test]
    async fn the_pin_is_asked_as_a_secret_prompt_and_never_shown_back() {
        let (interaction, shown, _) = recording().await;
        let asking = interaction.clone();
        let task = tokio::spawn(async move { asking.security_key_pin("key", Some(2)).await });
        while lock(&shown).is_empty() {
            tokio::task::yield_now().await;
        }
        let (id, host, text, kind) = lock(&shown)[0].clone();
        assert_eq!((host.as_str(), kind), ("web", Some("security_key_pin")));
        assert!(text.contains("2 tries left"), "{text}");
        interaction.hub.reply(id, Some("2468".into()), false);
        assert_eq!(task.await.unwrap().as_deref(), Some("2468"));
        assert!(!text.contains("2468"));
    }

    #[tokio::test]
    async fn a_cancelled_pin_prompt_cancels_the_key() {
        let (interaction, shown, _) = recording().await;
        let asking = interaction.clone();
        let task = tokio::spawn(async move { asking.security_key_pin("key", None).await });
        while lock(&shown).is_empty() {
            tokio::task::yield_now().await;
        }
        let id = lock(&shown)[0].0;
        interaction.hub.reply(id, None, false);
        assert_eq!(task.await.unwrap(), None);
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
        let prompts = HostKeyPrompts::new(Arc::new(|_, _| 0));
        assert_eq!(
            prompts.ask(1, &details(KnownHostStatus::Unknown)).await,
            Decision::Reject
        );
    }

    #[tokio::test]
    async fn the_first_reply_wins_and_every_client_hears_the_end() {
        let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let seen = events.clone();
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let prompts = HostKeyPrompts::new(Arc::new(move |kind, payload| {
            counter.fetch_add(1, Ordering::SeqCst);
            lock(&seen).push((kind.to_owned(), payload));
            1
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

    #[test]
    fn unknown_decisions_are_rejections() {
        assert_eq!(parse_decision("accept_once"), Decision::AcceptOnce);
        assert_eq!(parse_decision("accept_and_save"), Decision::AcceptAndSave);
        assert_eq!(parse_decision("whatever"), Decision::Reject);
    }
}
