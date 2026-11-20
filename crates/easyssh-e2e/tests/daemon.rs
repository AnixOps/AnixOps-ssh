#![cfg(unix)]
//! The daemon against real servers, on both engines. Every scenario runs
//! once with `ssh_engine = openssh` and once with `ssh_engine = russh`
//! (see `support::both_engines!`), with the same assertions.

#[macro_use]
mod support;

use easyssh_core::{ConfigStore, Connection};
use easyssh_e2e::E2e;
use easysshd::link::{ExitReason, SessionMsg, SessionSink};
use easysshd::{Daemon, DaemonConfig, Server};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use support::{set_engine, Client, EngineKind};

type Seen = Arc<Mutex<Vec<SessionMsg>>>;

fn collector() -> (Seen, SessionSink) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    (
        seen,
        Box::new(move |message| {
            sink.lock().unwrap().push(message);
            true
        }),
    )
}

fn text(seen: &Seen) -> String {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|message| match message {
            SessionMsg::Data { data, .. } => Some(data.as_str()),
            SessionMsg::Exit { .. } => None,
        })
        .collect()
}

async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Running {
    runtime: std::path::PathBuf,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<()>,
    daemon: Arc<Daemon>,
}

async fn start(e2e: &E2e, engine: EngineKind) -> Running {
    let config_path = e2e.dir.path().join("connections.json");
    let config = DaemonConfig {
        config_store: ConfigStore::at(config_path.clone()),
        local_shell: None,
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let daemon = Daemon::new(config).await.expect("daemon");
    set_engine(&config_path, engine);
    let runtime = e2e.dir.path().join("run");
    let server = Server::start_service(&runtime, daemon.clone()).expect("start server");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        server
            .serve(async {
                let _ = stopped.await;
            })
            .await
            .expect("serve")
    });
    Running {
        runtime,
        stop,
        serving,
        daemon,
    }
}

impl Running {
    async fn finish(self) {
        let _ = self.stop.send(());
        self.serving.await.expect("server task");
        self.daemon.shutdown();
    }
}

/// The phase 1 promise against a real server: the daemon runs the host's
/// session (the system `ssh` on a PTY, or the built-in engine), the first
/// client goes away, and a second one reattaches to the same live session
/// and sees its scrollback.
async fn daemon_session_survives_a_client_and_replays(engine: EngineKind) {
    let e2e = E2e::new();
    let running = start(&e2e, engine).await;
    let runtime = running.runtime.clone();

    // First "window": save the host through the daemon, open it, run a command.
    let (first_client, first_link) = Client::connect(&runtime).await;
    let connection = Connection::alias("E2E target", "e2e-target");
    first_link
        .request(
            "hosts.save",
            json!({"connection": serde_json::to_value(&connection).unwrap()}),
        )
        .await
        .expect("hosts.save");
    let (first, sink) = collector();
    let id = first_link
        .open_session(Some(&connection.id), 100, 30, sink)
        .await
        .expect("session.open");
    // The echo shows `$((1+1))`; only the remote shell's evaluation prints `-2`.
    first_link
        .write_session(&id, "echo e2e-daemon-$((1+1))\r")
        .unwrap();
    eventually("the command's output", || {
        text(&first).contains("e2e-daemon-2")
    })
    .await;

    // The window goes away; the session lives on in the daemon.
    first_client.detach();
    drop(first_link);
    drop(first);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let (second_client, second_link) = Client::connect(&runtime).await;
    let live = second_link.list_sessions().await.expect("session.list");
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].session_id, id);
    assert!(live[0].running);
    assert_eq!(live[0].host_id.as_deref(), Some(connection.id.as_str()));
    assert_eq!(
        serde_json::to_value(&live[0]).unwrap()["engine"],
        engine.name()
    );

    let (second, sink) = collector();
    let attached = second_link
        .attach_session(&id, None, sink)
        .await
        .expect("session.attach");
    assert!(attached.running);
    assert!(!attached.truncated);
    eventually("the replayed scrollback", || {
        text(&second).contains("e2e-daemon-2")
    })
    .await;

    // Still interactive after reattaching; `exit` ends it with status 0.
    second_link
        .write_session(&id, "echo e2e-again-$((2+3))\r")
        .unwrap();
    eventually("output after reattach", || {
        text(&second).contains("e2e-again-5")
    })
    .await;
    second_link.write_session(&id, "exit\r").unwrap();
    eventually("the exit status", || {
        second.lock().unwrap().contains(&SessionMsg::Exit {
            code: Some(0),
            reason: ExitReason::Exit,
        })
    })
    .await;
    second_link.close_session(&id);

    second_client.detach();
    drop(second_link);
    running.finish().await;
}

both_engines!(daemon_session_survives_a_client_and_replays);

/// A host behind a bastion (`ProxyJump` in the ssh config): a terminal opens
/// through it and runs a command on the target.
async fn terminal_through_a_jump_host(engine: EngineKind) {
    let e2e = E2e::new();
    let running = start(&e2e, engine).await;
    let (client, link) = Client::connect(&running.runtime).await;
    let connection = Connection::alias("E2E via bastion", "e2e-target-via-bastion");
    link.request(
        "hosts.save",
        json!({"connection": serde_json::to_value(&connection).unwrap()}),
    )
    .await
    .expect("hosts.save");
    let (seen, sink) = collector();
    let id = link
        .open_session(Some(&connection.id), 100, 30, sink)
        .await
        .expect("session.open");
    link.write_session(&id, "echo e2e-jump-$((40+2))\r")
        .unwrap();
    eventually("the command's output through the bastion", || {
        text(&seen).contains("e2e-jump-42")
    })
    .await;
    link.write_session(&id, "exit\r").unwrap();
    eventually("the exit status", || {
        seen.lock().unwrap().contains(&SessionMsg::Exit {
            code: Some(0),
            reason: ExitReason::Exit,
        })
    })
    .await;
    link.close_session(&id);
    client.detach();
    drop(link);
    running.finish().await;
}

both_engines!(terminal_through_a_jump_host);
