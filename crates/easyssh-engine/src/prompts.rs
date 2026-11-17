//! The engine's questions to the person holding the phone: login passwords,
//! key passphrases, keyboard-interactive rounds and host keys.
//!
//! A question is sent as an event (`askpass`, `hostkey`, the same payloads the
//! desktop daemon sends, so the React dialogs are shared) and answered by a
//! command (`askpass_reply`, `hostkey_reply`). Nothing is stored or logged. An
//! unanswered question is a cancellation after [`ANSWER_TIMEOUT`]; an unanswered
//! host key is a rejection.

use async_trait::async_trait;
use easyssh_ssh::known_hosts::KnownHostStatus;
use easyssh_ssh::{Decision, HostKeyDetails, Interaction, KbdPrompt};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::oneshot;

pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

/// Sends an event to the UI (`emit(name, payload)`).
pub type Emit = Arc<dyn Fn(&str, Value) + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

pub struct Prompts {
    emit: Emit,
    next: AtomicU64,
    secrets: Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>,
    hostkeys: Mutex<HashMap<u64, oneshot::Sender<Decision>>>,
    timeout: Duration,
}

/// Parses the `decision` of `hostkey_reply`. Anything unknown is a rejection.
pub fn parse_decision(name: &str) -> Decision {
    match name {
        "accept_once" => Decision::AcceptOnce,
        "accept_and_save" => Decision::AcceptAndSave,
        "replace_and_save" => Decision::ReplaceAndSave,
        _ => Decision::Reject,
    }
}

impl Prompts {
    pub fn new(emit: Emit) -> Arc<Self> {
        Self::with_timeout(emit, ANSWER_TIMEOUT)
    }

    pub fn with_timeout(emit: Emit, timeout: Duration) -> Arc<Self> {
        Arc::new(Prompts {
            emit,
            next: AtomicU64::new(1),
            secrets: Mutex::new(HashMap::new()),
            hostkeys: Mutex::new(HashMap::new()),
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
        (self.emit)(
            "askpass",
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
        let answer = tokio::time::timeout(self.timeout, receiver)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
        lock(&self.secrets).remove(&id);
        (self.emit)("askpass-resolved", json!({ "id": id }));
        answer
    }

    /// `askpass_reply`. `None` cancels. Unknown or answered ids are ignored.
    pub fn reply_secret(&self, id: u64, answer: Option<String>) {
        if let Some(sender) = lock(&self.secrets).remove(&id) {
            let _ = sender.send(answer);
        }
    }

    pub async fn ask_hostkey(&self, details: &HostKeyDetails) -> Decision {
        let id = self.next_id();
        let (sender, receiver) = oneshot::channel();
        lock(&self.hostkeys).insert(id, sender);
        let (status, expected) = match &details.status {
            KnownHostStatus::Changed { expected, .. } => ("changed", expected.clone()),
            _ => ("unknown", Vec::new()),
        };
        (self.emit)(
            "hostkey",
            json!({
                "id": id,
                "host": details.host,
                "port": details.port,
                "algorithm": details.algorithm,
                "fingerprint": details.fingerprint,
                "status": status,
                "expected": expected,
            }),
        );
        let decision = tokio::time::timeout(self.timeout, receiver)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(Decision::Reject);
        lock(&self.hostkeys).remove(&id);
        (self.emit)("hostkey-resolved", json!({ "id": id }));
        decision
    }

    /// `hostkey_reply`.
    pub fn reply_hostkey(&self, id: u64, decision: &str) {
        if let Some(sender) = lock(&self.hostkeys).remove(&id) {
            let _ = sender.send(parse_decision(decision));
        }
    }
}

/// The [`Interaction`] for one connection attempt; `label` names the host in
/// the dialogs.
pub struct PromptInteraction {
    pub prompts: Arc<Prompts>,
    pub label: String,
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
        self.prompts
            .ask_secret(&self.label, prompt, "password", false)
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

    type Seen = Arc<Mutex<Vec<(String, Value)>>>;

    fn recording() -> (Emit, Seen) {
        let seen: Seen = Arc::default();
        let sink = seen.clone();
        let emit: Emit = Arc::new(move |name, payload| {
            lock(&sink).push((name.to_owned(), payload));
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
        assert_eq!(name, "askpass");
        assert_eq!(payload["host"], "web");
        assert_eq!(payload["kind"], "password");
        assert_eq!(payload["confirm"], false);
        prompts.reply_secret(payload["id"].as_u64().unwrap(), Some("pw".into()));
        assert_eq!(task.await.unwrap().as_deref(), Some("pw"));
        assert_eq!(lock(&seen)[1].0, "askpass-resolved");
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
        assert_eq!(name, "hostkey");
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
}
