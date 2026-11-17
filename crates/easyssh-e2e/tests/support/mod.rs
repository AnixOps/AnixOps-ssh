//! Shared by the daemon-level end-to-end tests: which SSH engine a scenario
//! runs on, a client that answers the daemon's prompts like a user would,
//! and `both_engines!`, which turns one scenario into an `openssh` and a
//! `russh` test.
//!
//! The owner's rule: the built-in engine is the default only because every
//! scenario below passes on it as well as on OpenSSH. A scenario that is
//! engine-neutral goes through `both_engines!`; a scenario about something
//! only one engine does (a key file for OpenSSH, one shared connection for
//! russh) stays a plain test and says so.
#![cfg(unix)]
#![allow(dead_code)]

use easyssh_core::SshEngine;
use easysshd::link::{AskpassPrompt, HostKeyPrompt, Link, LinkHandler};
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The engine a scenario runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    Openssh,
    Russh,
}

impl EngineKind {
    pub const BOTH: [EngineKind; 2] = [EngineKind::Openssh, EngineKind::Russh];

    pub fn name(self) -> &'static str {
        match self {
            EngineKind::Openssh => "openssh",
            EngineKind::Russh => "russh",
        }
    }

    pub fn setting(self) -> SshEngine {
        match self {
            EngineKind::Openssh => SshEngine::Openssh,
            EngineKind::Russh => SshEngine::Russh,
        }
    }
}

/// Sets the global `ssh_engine` the way the app does: a row in the settings
/// of the app database that the running daemon reads on every open. Always
/// set explicitly, so a scenario never depends on which engine is the
/// default.
pub fn set_engine(config_path: &Path, engine: EngineKind) {
    let opened = easyssh_store::open_app_store(config_path).expect("open the app database");
    opened
        .store
        .set_setting_as("ssh_engine", &engine.setting())
        .expect("save ssh_engine");
}

/// Answers prompts the way a user would and remembers what was asked.
/// Host keys are accepted and saved unless told otherwise, so a scenario
/// that is not about host keys behaves the same on both engines (OpenSSH is
/// configured with `StrictHostKeyChecking accept-new` in the test config).
pub struct Client {
    link: Mutex<Option<Link>>,
    host_key_answer: Mutex<String>,
    pub password: Mutex<Option<String>>,
    pub host_keys: Mutex<Vec<HostKeyPrompt>>,
    pub asks: Mutex<Vec<AskpassPrompt>>,
}

impl Client {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            link: Mutex::new(None),
            host_key_answer: Mutex::new("accept_and_save".into()),
            password: Mutex::new(None),
            host_keys: Mutex::new(Vec::new()),
            asks: Mutex::new(Vec::new()),
        })
    }

    /// Connects to the daemon in `runtime` with this client as the handler.
    pub async fn connect(runtime: &Path) -> (Arc<Self>, Link) {
        let client = Client::new();
        let link = Link::connect(runtime, client.clone())
            .await
            .expect("connect");
        client.attach(&link);
        (client, link)
    }

    /// Lets the client answer prompts. The link and its handler then hold
    /// each other, so call [`Client::detach`] before dropping the link.
    pub fn attach(&self, link: &Link) {
        *self.link.lock().unwrap() = Some(link.clone());
    }

    pub fn detach(&self) {
        *self.link.lock().unwrap() = None;
    }

    pub fn link(&self) -> Link {
        self.link.lock().unwrap().clone().expect("link attached")
    }

    pub fn answer_host_keys(&self, decision: &str) {
        *self.host_key_answer.lock().unwrap() = decision.to_owned();
    }

    pub fn set_password(&self, password: Option<&str>) {
        *self.password.lock().unwrap() = password.map(str::to_owned);
    }
}

impl LinkHandler for Client {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        let id = prompt.id;
        self.asks.lock().unwrap().push(prompt);
        let answer = self.password.lock().unwrap().clone();
        if let Some(link) = self.link.lock().unwrap().clone() {
            let _ = link.askpass_reply(id, answer);
        }
    }

    fn hostkey_prompt(&self, prompt: HostKeyPrompt) {
        let id = prompt.id;
        self.host_keys.lock().unwrap().push(prompt);
        let decision = self.host_key_answer.lock().unwrap().clone();
        if let Some(link) = self.link.lock().unwrap().clone() {
            let _ = link.hostkey_reply(id, &decision);
        }
    }
}

/// `both_engines!(scenario)` expects `async fn scenario(engine: EngineKind)`
/// and adds the tests `scenario::openssh` and `scenario::russh`.
macro_rules! both_engines {
    ($name:ident) => {
        mod $name {
            #[tokio::test]
            #[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
            async fn openssh() {
                super::$name($crate::support::EngineKind::Openssh).await
            }

            #[tokio::test]
            #[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
            async fn russh() {
                super::$name($crate::support::EngineKind::Russh).await
            }
        }
    };
}
