//! The in-process engine of the mobile app (`easyssh-engine`) against a real
//! OpenSSH server: the prompts are answered through the same events and
//! replies the web view uses, and a shell runs a command.
#![cfg(unix)]

use easyssh_e2e::{PW_PASSWORD, PW_USER, TARGET_PORT};
use easyssh_engine::prompts::Prompts;
use easyssh_engine::{Engine, ShellSink, Target};
use easyssh_ssh::MemoryKnownHosts;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::mpsc;

enum Event {
    Data(String),
    Exit(Option<u32>),
}

struct Channelled(mpsc::UnboundedSender<Event>);

impl ShellSink for Channelled {
    fn data(&self, text: String) {
        let _ = self.0.send(Event::Data(text));
    }
    fn exit(&self, code: Option<u32>) {
        let _ = self.0.send(Event::Exit(code));
    }
}

/// An engine whose UI answers every question like a person would: trusts the
/// host key once and types the right password.
fn engine(password: &'static str, asked: Arc<Mutex<Vec<String>>>) -> Arc<Engine> {
    let prompts_cell: Arc<OnceLock<Weak<Prompts>>> = Arc::default();
    let cell = prompts_cell.clone();
    let prompts = Prompts::new(Arc::new(move |name, payload| {
        asked.lock().unwrap().push(name.to_owned());
        let Some(prompts) = cell.get().and_then(Weak::upgrade) else {
            return;
        };
        let Some(id) = payload["id"].as_u64() else {
            return;
        };
        match name {
            "hostkey" => prompts.reply_hostkey(id, "accept_once"),
            "askpass" => prompts.reply_secret(id, Some(password.to_owned())),
            _ => {}
        }
    }));
    prompts_cell.set(Arc::downgrade(&prompts)).unwrap();
    Engine::new(prompts, Arc::new(MemoryKnownHosts::new()))
}

fn target() -> Target {
    Target {
        label: "e2e".into(),
        host: "127.0.0.1".into(),
        port: TARGET_PORT,
        user: PW_USER.into(),
        identity: None,
    }
}

async fn next(receiver: &mut mpsc::UnboundedReceiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(30), receiver.recv())
        .await
        .expect("the shell stayed silent")
        .expect("the sink closed")
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_shell_opens_after_the_host_key_and_password_questions_and_runs_a_command() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(PW_PASSWORD, asked.clone());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let id = engine
        .open_shell(target(), 100, 30, Arc::new(Channelled(sender)))
        .await
        .expect("open the shell");
    assert_eq!(engine.open_ids(), vec![id.clone()]);
    engine.resize(&id, 120, 40).unwrap();
    engine
        .write(&id, b"echo mobile-$((20+22)); exit 3\r")
        .unwrap();

    let mut output = String::new();
    let code = loop {
        match next(&mut receiver).await {
            Event::Data(text) => output.push_str(&text),
            Event::Exit(code) => break code,
        }
    };
    assert!(output.contains("mobile-42"), "{output:?}");
    assert_eq!(code, Some(3));
    let asked = asked.lock().unwrap().clone();
    assert!(asked.iter().any(|name| name == "hostkey"), "{asked:?}");
    assert!(asked.iter().any(|name| name == "askpass"), "{asked:?}");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_wrong_password_is_an_auth_error_the_ui_can_show() {
    let engine = engine("definitely-wrong", Arc::default());
    let (sender, _receiver) = mpsc::unbounded_channel();
    let error = engine
        .open_shell(target(), 80, 24, Arc::new(Channelled(sender)))
        .await
        .expect_err("a wrong password must not log in");
    assert_eq!(error.code, "auth_failed", "{error}");
    assert!(engine.open_ids().is_empty());
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn closing_a_session_ends_it_and_the_ui_hears_about_it() {
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let id = engine
        .open_shell(target(), 80, 24, Arc::new(Channelled(sender)))
        .await
        .expect("open the shell");
    engine.close(&id);
    loop {
        if let Event::Exit(_) = next(&mut receiver).await {
            break;
        }
    }
    assert!(engine.write(&id, b"ls\r").is_err());
}
