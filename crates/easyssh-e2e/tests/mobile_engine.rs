//! The in-process engine of the mobile app (`easyssh-engine`) against a real
//! OpenSSH server: the prompts are answered through the same events and
//! replies the web view uses, a shell runs a command, and the file browser and
//! a port forward share the terminal's one connection.
#![cfg(unix)]

use easyssh_core::{Tunnel, TunnelKind};
use easyssh_e2e::russh_support::{echo_roundtrip, wait_for_port_open, wait_for_port_shut, within};
use easyssh_e2e::{
    sample_bytes, unique_name, E2e, BASTION_PORT, ECHO_PORT, PW_PASSWORD, PW_USER, TARGET_PORT,
};
use easyssh_engine::prompts::Prompts;
use easyssh_engine::{Engine, ShellSink, Target};
use easyssh_ssh::{MemoryIdentity, MemoryKnownHosts};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::net::TcpStream;
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
/// host key once and types the right password. `events` also receives what
/// the engine reports besides questions (transfers, tunnel status).
fn engine_with_events(
    password: &'static str,
    asked: Arc<Mutex<Vec<String>>>,
    events: Arc<Mutex<Vec<(String, Value)>>>,
) -> Arc<Engine> {
    let prompts_cell: Arc<OnceLock<Weak<Prompts>>> = Arc::default();
    let cell = prompts_cell.clone();
    let prompts = Prompts::new(Arc::new(move |name: &str, payload: Value| {
        asked.lock().unwrap().push(name.to_owned());
        let Some(prompts) = cell.get().and_then(Weak::upgrade) else {
            return 1;
        };
        let Some(id) = payload["id"].as_u64() else {
            return 1;
        };
        match name {
            "hostkey.prompt" => prompts.reply_hostkey(id, "accept_once"),
            "askpass.prompt" => prompts.reply_secret(id, Some(password.to_owned())),
            _ => {}
        }
        1usize
    }));
    prompts_cell.set(Arc::downgrade(&prompts)).unwrap();
    let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
        events.lock().unwrap().push((name.to_owned(), payload));
        1usize
    });
    Engine::new(prompts, Arc::new(MemoryKnownHosts::new()), sink)
}

fn engine(password: &'static str, asked: Arc<Mutex<Vec<String>>>) -> Arc<Engine> {
    engine_with_events(password, asked, Arc::default())
}

fn target() -> Target {
    Target {
        host_id: "e2e".into(),
        label: "e2e".into(),
        host: "127.0.0.1".into(),
        port: TARGET_PORT,
        user: PW_USER.into(),
        identity: None,
        jump: Vec::new(),
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
    assert!(
        asked.iter().any(|name| name == "hostkey.prompt"),
        "{asked:?}"
    );
    assert!(
        asked.iter().any(|name| name == "askpass.prompt"),
        "{asked:?}"
    );
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

/// What a transfer reports to the sink it was started with.
struct TransferEnd {
    status: String,
    error: Option<String>,
    progress: u64,
}

async fn run_transfer(
    engine: &Engine,
    entry: Arc<easyssh_engine::files::SftpEntry>,
    upload: bool,
    local: std::path::PathBuf,
    remote: String,
) -> TransferEnd {
    let (sender, mut receiver) = mpsc::unbounded_channel::<(String, Value)>();
    let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
        let _ = sender.send((name.to_owned(), payload));
        1usize
    });
    let id = engine
        .files()
        .start_transfer(entry, upload, local, remote, false, sink);
    let mut progress = 0;
    within("transfer", async {
        loop {
            let (name, payload) = receiver.recv().await.expect("transfer events");
            assert_eq!(payload["transfer_id"], id.as_str());
            match name.as_str() {
                "transfer.progress" => progress = payload["bytes"].as_u64().unwrap(),
                "transfer.done" => {
                    return TransferEnd {
                        status: payload["status"].as_str().unwrap().to_owned(),
                        error: payload["error"].as_str().map(str::to_owned),
                        progress,
                    }
                }
                other => panic!("unexpected event {other}"),
            }
        }
    })
    .await
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn the_terminal_the_files_and_a_forward_share_one_connection() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let engine = engine_with_events(PW_PASSWORD, asked.clone(), events.clone());
    engine.set_linger(Duration::from_millis(300));
    let target = target();

    // The terminal logs in: one host key question and one password question.
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let shell = engine
        .open_shell(target.clone(), 80, 24, Arc::new(Channelled(sender)))
        .await
        .expect("open the shell");

    // The file browser on the same host asks nothing more.
    let (sftp_id, home) = within("sftp", engine.open_files(&target))
        .await
        .expect("open the files");
    assert!(home.starts_with("/home/"), "{home}");
    let entry = engine.files().get(&sftp_id).expect("the file session");

    let dir = format!("{home}/{}", unique_name("e2e-mobile"));
    entry.mkdir(&dir).await.expect("mkdir");

    // Upload from a local file the app staged, list, rename, download.
    let scratch = tempfile::tempdir().unwrap();
    let data = sample_bytes(300_000);
    let local = scratch.path().join("report.bin");
    std::fs::write(&local, &data).unwrap();
    let remote = format!("{dir}/report.bin");
    let end = run_transfer(&engine, entry.clone(), true, local.clone(), remote.clone()).await;
    assert_eq!(end.status, "completed", "{:?}", end.error);
    assert_eq!(end.progress, data.len() as u64);

    let listing = entry.list(&dir).await.expect("list");
    assert_eq!(listing["entries"][0]["name"], "report.bin");
    assert_eq!(listing["entries"][0]["kind"], "file");
    assert_eq!(listing["entries"][0]["size"], data.len() as u64);

    let renamed = format!("{dir}/renamed.bin");
    entry.rename(&remote, &renamed).await.expect("rename");
    let downloaded = scratch.path().join("back.bin");
    let end = run_transfer(
        &engine,
        entry.clone(),
        false,
        downloaded.clone(),
        renamed.clone(),
    )
    .await;
    assert_eq!(end.status, "completed", "{:?}", end.error);
    assert_eq!(std::fs::read(&downloaded).unwrap(), data);

    // A download of something that is not there fails and leaves no file.
    let missing = scratch.path().join("missing.bin");
    let end = run_transfer(
        &engine,
        entry.clone(),
        false,
        missing.clone(),
        format!("{dir}/nope.bin"),
    )
    .await;
    assert_eq!(end.status, "failed");
    assert!(end.error.is_some());
    assert!(!missing.exists());

    // A local forward to the echo service on the server's loopback.
    let tunnel = Tunnel {
        id: "e2e-forward".into(),
        name: "echo".into(),
        host_id: target.host_id.clone(),
        kind: TunnelKind::Local,
        bind_address: None,
        bind_port: 0,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(ECHO_PORT),
    };
    let port = within("forward", engine.start_tunnel(&tunnel, &target))
        .await
        .expect("start the forward");
    assert_ne!(port, 0);
    wait_for_port_open(port, Duration::from_secs(10)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    echo_roundtrip(&mut stream, "through-the-phone").await;
    drop(stream);
    assert_eq!(
        engine
            .start_tunnel(&tunnel, &target)
            .await
            .unwrap_err()
            .code,
        "already_running"
    );
    let states: Vec<String> = events
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == "tunnel.status")
        .map(|(_, payload)| payload["state"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(states, ["starting", "running"]);

    // One connection carried the shell, the file session, four transfers'
    // worth of channels and the forward, and the user was asked once.
    let report = engine.connections().await;
    assert_eq!(report["opened"], 1, "{report}");
    assert_eq!(report["live"], 1, "{report}");
    assert!(report["connections"][0]["leases"].as_u64().unwrap() >= 3);
    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked.iter().filter(|n| *n == "hostkey.prompt").count(),
        1,
        "{asked:?}"
    );
    assert_eq!(
        asked.iter().filter(|n| *n == "askpass.prompt").count(),
        1,
        "{asked:?}"
    );

    // Clean up: stop the forward, remove the files, end the shell.
    engine.stop_tunnel("e2e-forward").await;
    wait_for_port_shut(port, Duration::from_secs(10)).await;
    entry.remove(&renamed, false).await.expect("rm");
    entry.remove(&dir, true).await.expect("rmdir");
    assert!(entry.list(&dir).await.is_err());
    engine.files().close(&sftp_id).await;
    engine.close(&shell);
    loop {
        if let Event::Exit(_) = next(&mut receiver).await {
            break;
        }
    }

    // With nobody left the connection lingers briefly, then closes.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let report = engine.connections().await;
    assert_eq!(report["live"], 0, "{report}");
    // Asking again connects anew (a new connection means new questions, but
    // the answers above are scripted the same way).
    let again = engine.open_files(&target).await.expect("reconnect");
    assert_eq!(engine.connections().await["opened"], 2);
    engine.files().close(&again.0).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_cancelled_transfer_is_reported_and_leaves_no_partial_file() {
    let engine = engine(PW_PASSWORD, Arc::default());
    let target = target();
    let (sftp_id, home) = within("sftp", engine.open_files(&target))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    // Big enough that the cancel arrives while it is still running.
    let remote = format!("{home}/{}.bin", unique_name("e2e-mobile-big"));
    let local = scratch.path().join("big.bin");
    std::fs::write(&local, sample_bytes(8 * 1024 * 1024)).unwrap();
    let (sender, mut receiver) = mpsc::unbounded_channel::<(String, Value)>();
    let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
        let _ = sender.send((name.to_owned(), payload));
        1usize
    });
    let id = engine
        .files()
        .start_transfer(entry.clone(), true, local, remote.clone(), false, sink);
    engine.files().cancel_transfer(&id);
    let status = within("cancel", async {
        loop {
            let (name, payload) = receiver.recv().await.expect("events");
            if name == "transfer.done" {
                return payload["status"].as_str().unwrap().to_owned();
            }
        }
    })
    .await;
    assert_eq!(status, "cancelled");
    // Cancelling twice is harmless.
    engine.files().cancel_transfer(&id);
    let _ = entry.remove(&remote, false).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_host_behind_a_jump_host_opens_through_it_with_its_own_questions() {
    let e2e = E2e::new();
    let key = std::fs::read_to_string(&e2e.key).expect("the test key");
    let identity = |label: &str| Some(MemoryIdentity::new(label, key.clone(), None));
    let bastion = Target {
        host_id: "bastion".into(),
        label: "bastion".into(),
        host: "127.0.0.1".into(),
        port: BASTION_PORT,
        user: "keyuser".into(),
        identity: identity("bastion key"),
        jump: Vec::new(),
    };
    let behind = Target {
        host_id: "inner".into(),
        label: "inner".into(),
        host: "target".into(),
        port: 22,
        user: "keyuser".into(),
        identity: identity("inner key"),
        jump: vec![bastion],
    };
    let asked = Arc::new(Mutex::new(Vec::new()));
    let engine = engine(PW_PASSWORD, asked.clone());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let id = engine
        .open_shell(behind.clone(), 80, 24, Arc::new(Channelled(sender)))
        .await
        .expect("open through the jump host");
    engine.write(&id, b"hostname; exit\r").unwrap();
    let mut output = String::new();
    loop {
        match next(&mut receiver).await {
            Event::Data(text) => output.push_str(&text),
            Event::Exit(_) => break,
        }
    }
    assert!(output.contains("target"), "{output:?}");
    // Each hop asked about its own host key, and the keys logged both in.
    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked.iter().filter(|n| *n == "hostkey.prompt").count(),
        2,
        "{asked:?}"
    );
    assert!(!asked.iter().any(|n| n == "askpass.prompt"), "{asked:?}");
    // Files go through the same chain on the same connection.
    let (sftp_id, home) = engine.open_files(&behind).await.expect("files");
    assert_eq!(home, "/home/keyuser");
    engine.files().close(&sftp_id).await;
    engine.shutdown().await;
}
