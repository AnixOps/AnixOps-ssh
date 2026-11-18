//! The mobile supervisor against a real OpenSSH server with tmux: a session
//! that opted in to "attach to tmux" survives a closed connection, and a
//! reconnect lands in the same screen.
#![cfg(unix)]

use easyssh_e2e::{PW_PASSWORD, PW_USER, TARGET_PORT};
use easyssh_engine::prompts::Prompts;
use easyssh_engine::supervisor::{
    Attach, ReconnectPolicy, SessionOptions, Status, SupervisedSink, Supervisor,
};
use easyssh_engine::{Engine, ShellSink, Target};
use easyssh_ssh::MemoryKnownHosts;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug)]
enum Event {
    Data(String),
    Exit(Option<u32>),
    Reconnecting,
    Resumed,
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

impl SupervisedSink for Channelled {
    fn status(&self, status: Status) {
        let _ = self.0.send(match status {
            Status::Reconnecting { .. } => Event::Reconnecting,
            Status::Resumed => Event::Resumed,
        });
    }
}

fn engine() -> Arc<Engine> {
    let cell: Arc<OnceLock<Weak<Prompts>>> = Arc::default();
    let reply = cell.clone();
    let prompts = Prompts::new(Arc::new(move |name, payload| {
        let Some(prompts) = reply.get().and_then(Weak::upgrade) else {
            return;
        };
        let Some(id) = payload["id"].as_u64() else {
            return;
        };
        match name {
            "hostkey" => prompts.reply_hostkey(id, "accept_once"),
            "askpass" => prompts.reply_secret(id, Some(PW_PASSWORD.to_owned())),
            _ => {}
        }
    }));
    cell.set(Arc::downgrade(&prompts)).unwrap();
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

fn options() -> SessionOptions {
    SessionOptions {
        attach: Attach::Tmux,
        reconnect: true,
        policy: ReconnectPolicy::default(),
    }
}

async fn next(receiver: &mut mpsc::UnboundedReceiver<Event>, what: &str) -> Event {
    tokio::time::timeout(Duration::from_secs(30), receiver.recv())
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .expect("the sink closed")
}

/// Reads events until `wanted` shows up in the output.
async fn wait_for(
    receiver: &mut mpsc::UnboundedReceiver<Event>,
    output: &mut String,
    wanted: &str,
) {
    while !output.contains(wanted) {
        match next(receiver, wanted).await {
            Event::Data(text) => output.push_str(&text),
            Event::Exit(code) => {
                panic!("the session ended ({code:?}) before {wanted:?}: {output:?}")
            }
            Event::Reconnecting | Event::Resumed => {}
        }
    }
}

async fn wait_for_event(
    receiver: &mut mpsc::UnboundedReceiver<Event>,
    output: &mut String,
    matches: impl Fn(&Event) -> bool,
) {
    loop {
        let event = next(receiver, "a status event").await;
        if matches(&event) {
            return;
        }
        if let Event::Data(text) = event {
            output.push_str(&text);
        }
    }
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn work_in_a_tmux_session_survives_a_closed_connection_and_a_forced_reconnect() {
    let supervisor = Supervisor::new(engine());

    // First connection: attach, run something that leaves a mark on screen.
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let id = supervisor
        .open(target(), 100, 30, Arc::new(Channelled(sender)), options())
        .await
        .expect("open the attached shell");
    let mut output = String::new();
    // tmux is up once its status line (the session name) is drawn.
    wait_for(&mut receiver, &mut output, "anixops").await;
    supervisor.write(&id, b"echo mark-$((20+22))\r").unwrap();
    wait_for(&mut receiver, &mut output, "mark-42").await;

    // The phone goes to the background and the app closes the connection.
    supervisor.close(&id);
    wait_for_event(&mut receiver, &mut output, |event| {
        matches!(event, Event::Exit(_))
    })
    .await;
    assert!(supervisor.open_ids().is_empty());

    // A new connection lands in the same tmux session: the screen comes back.
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let id = supervisor
        .open(target(), 100, 30, Arc::new(Channelled(sender)), options())
        .await
        .expect("reattach");
    let mut output = String::new();
    wait_for(&mut receiver, &mut output, "mark-42").await;

    // A forced reconnect (the app is back in front) keeps the id and the screen.
    supervisor.reconnect(&id).expect("reconnect");
    wait_for_event(&mut receiver, &mut output, |event| {
        matches!(event, Event::Resumed)
    })
    .await;
    supervisor.write(&id, b"echo after-$((1+1))\r").unwrap();
    wait_for(&mut receiver, &mut output, "after-2").await;

    // Clean up so a second run starts from nothing.
    supervisor
        .write(&id, b"tmux kill-session -t anixops\r")
        .unwrap();
    wait_for_event(&mut receiver, &mut output, |event| {
        matches!(event, Event::Exit(_))
    })
    .await;
    assert!(supervisor.open_ids().is_empty());
}
