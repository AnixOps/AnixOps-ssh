//! The in-process engine of the mobile app (`easyssh-engine`) against a real
//! OpenSSH server: the prompts are answered through the same events and
//! replies the web view uses, a shell runs a command, and the file browser and
//! a port forward share the terminal's one connection.
#![cfg(unix)]

use easyssh_core::{Tunnel, TunnelKind};
use easyssh_e2e::russh_support::{
    echo_roundtrip, wait_for_port_open, wait_for_port_shut, within, Scripted,
};
use easyssh_e2e::{
    sample_bytes, unique_name, E2e, BASTION_PORT, ECHO_PORT, PW_PASSWORD, PW_USER, TARGET_PORT,
};
use easyssh_engine::prompts::Prompts;
use easyssh_engine::{Engine, ShellSink, Target};
use easyssh_ssh::{ExitStatus, MemoryIdentity, MemoryKnownHosts, Session};
use easyssh_store::{SecretFlags, StoredSecrets};
use easyssh_vault::{SecretSource, VaultError};
use serde_json::Value;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
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
        saved_password: None,
        proxy: None,
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
    skipped: u64,
}

async fn run_transfer(
    engine: &Engine,
    entry: Arc<easyssh_engine::files::SftpEntry>,
    upload: bool,
    local: std::path::PathBuf,
    remote: String,
) -> TransferEnd {
    run_transfer_with(engine, entry, upload, local, remote, false).await
}

async fn run_transfer_with(
    engine: &Engine,
    entry: Arc<easyssh_engine::files::SftpEntry>,
    upload: bool,
    local: std::path::PathBuf,
    remote: String,
    recursive: bool,
) -> TransferEnd {
    let (sender, mut receiver) = mpsc::unbounded_channel::<(String, Value)>();
    let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
        let _ = sender.send((name.to_owned(), payload));
        1usize
    });
    let id = engine
        .files()
        .start_transfer(entry, upload, local, remote, recursive, sink);
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
                        skipped: payload["skipped"].as_u64().unwrap_or(0),
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
    // The handle itself holds a lease on the connection until it is dropped.
    drop(entry);
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
        saved_password: None,
        proxy: None,
    };
    let behind = Target {
        host_id: "inner".into(),
        label: "inner".into(),
        host: "target".into(),
        port: 22,
        user: "keyuser".into(),
        identity: identity("inner key"),
        jump: vec![bastion],
        saved_password: None,
        proxy: None,
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
    while let Event::Data(text) = next(&mut receiver).await {
        output.push_str(&text);
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

/// A vault that holds only a password, for every host.
struct PasswordVault(&'static str);

impl SecretSource for PasswordVault {
    fn host_secrets(&self, _: &str) -> Result<StoredSecrets, VaultError> {
        Ok(StoredSecrets {
            password: Some(easyssh_crypto::SecretText::new(self.0)),
            private_key: None,
            passphrase: None,
            ..Default::default()
        })
    }
    fn secret_flags(&self, _: &str) -> Result<SecretFlags, VaultError> {
        Ok(SecretFlags {
            password: true,
            private_key: false,
            passphrase: false,
            ..Default::default()
        })
    }
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_jump_host_uses_its_own_saved_password_and_nobody_is_asked() {
    let e2e = E2e::new();
    let key = std::fs::read_to_string(&e2e.key).expect("the test key");
    let vault: Arc<dyn SecretSource> = Arc::new(PasswordVault(PW_PASSWORD));
    // The bastion is a saved host that logs in with a password (pwuser); the
    // host behind it uses a key (keyuser, a bare address with nothing saved).
    // with_saved_login must hand the bastion its saved password.
    let bastion = Target {
        host_id: "bastion".into(),
        label: "bastion".into(),
        host: "127.0.0.1".into(),
        port: BASTION_PORT,
        user: PW_USER.into(),
        identity: None,
        jump: Vec::new(),
        saved_password: None,
        proxy: None,
    };
    let behind = Target {
        host_id: "inner".into(),
        label: "inner".into(),
        host: "target".into(),
        port: 22,
        user: "keyuser".into(),
        identity: None,
        jump: vec![bastion],
        saved_password: None,
        proxy: None,
    };
    let mut behind = easyssh_engine::with_saved_login(&vault, behind).expect("saved login");
    assert!(behind.jump[0].saved_password.is_some());
    // The inner host logs in by key (with_saved_login replaced its identity
    // with the vault's, which has none), and its own vault password is left
    // out so that only the jump hop has one.
    behind.identity = Some(MemoryIdentity::new("inner key", key, None));
    behind.saved_password = None;
    let asked = Arc::new(Mutex::new(Vec::new()));
    // Were a question asked, it would be answered wrongly and the login fail.
    let engine = engine("not-the-password", asked.clone());
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let id = engine
        .open_shell(behind, 80, 24, Arc::new(Channelled(sender)))
        .await
        .expect("open through the jump host with its saved password");
    engine.write(&id, b"hostname; exit\r").unwrap();
    let mut output = String::new();
    while let Event::Data(text) = next(&mut receiver).await {
        output.push_str(&text);
    }
    assert!(output.contains("target"), "{output:?}");
    let asked = asked.lock().unwrap().clone();
    assert!(!asked.iter().any(|n| n == "askpass.prompt"), "{asked:?}");
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_dynamic_and_a_remote_forward_run_on_the_shared_connection() {
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let engine = engine_with_events(PW_PASSWORD, Arc::default(), events.clone());
    let target = target();

    // -D: a SOCKS5 proxy on the phone that reaches the echo service as the
    // server sees it.
    let socks = Tunnel {
        id: "e2e-socks".into(),
        name: "socks".into(),
        host_id: target.host_id.clone(),
        kind: TunnelKind::Dynamic,
        bind_address: None,
        bind_port: 0,
        target_host: None,
        target_port: None,
    };
    let port = within("socks", engine.start_tunnel(&socks, &target))
        .await
        .expect("start the SOCKS proxy");
    assert_ne!(port, 0);
    wait_for_port_open(port, Duration::from_secs(10)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    within("socks5", async {
        stream.write_all(&[5, 1, 0]).await.unwrap();
        let mut reply = [0u8; 2];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [5, 0]);
        let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
        request.extend_from_slice(&ECHO_PORT.to_be_bytes());
        stream.write_all(&request).await.unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[..2], [5, 0], "{reply:?}");
    })
    .await;
    echo_roundtrip(&mut stream, "socks-on-the-phone").await;
    drop(stream);

    // -R: the server listens and calls back to a service on the phone.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let local_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_ok() {
                    let _ = reader
                        .get_mut()
                        .write_all(format!("phone-pong:{line}").as_bytes())
                        .await;
                }
            });
        }
    });
    let remote = Tunnel {
        id: "e2e-remote".into(),
        name: "remote".into(),
        host_id: target.host_id.clone(),
        kind: TunnelKind::Remote,
        bind_address: Some("127.0.0.1".into()),
        bind_port: 17102,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(local_port),
    };
    let server_port = within("remote", engine.start_tunnel(&remote, &target))
        .await
        .expect("start the remote forward");
    assert_eq!(server_port, 17102);
    let lease = engine.lease(&target).await.expect("the shared connection");
    let ask = "timeout 10 bash -c 'exec 3<>/dev/tcp/127.0.0.1/17102; echo ping >&3; head -n1 <&3'";
    let answer = within("remote answers", async {
        loop {
            let output = lease.session().exec(ask).await.expect("exec");
            let text = output.stdout_text();
            if text.contains("phone-pong") {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await;
    assert_eq!(answer, "phone-pong:ping\n");
    drop(lease);

    // A forward to a port that is taken fails with a reason, not silence.
    let busy = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let clash = Tunnel {
        id: "e2e-clash".into(),
        bind_port: busy.local_addr().unwrap().port(),
        kind: TunnelKind::Local,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(ECHO_PORT),
        ..socks.clone()
    };
    assert!(engine.start_tunnel(&clash, &target).await.is_err());
    let report = engine.tunnels().list();
    let row = report["tunnels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "e2e-clash")
        .expect("the failed forward is listed");
    assert_eq!(row["state"], "failed");
    assert!(row["error"].as_str().is_some_and(|e| !e.is_empty()));

    assert_eq!(engine.connections().await["opened"], 1);
    engine.stop_tunnel("e2e-socks").await;
    engine.stop_tunnel("e2e-remote").await;
    wait_for_port_shut(port, Duration::from_secs(10)).await;
    engine.shutdown().await;
}

/// Waits until a `tunnel.status` event for `id` matches `wanted`; returns it.
async fn tunnel_event(
    events: &Mutex<Vec<(String, Value)>>,
    id: &str,
    what: &str,
    wanted: impl Fn(&Value) -> bool,
) -> Value {
    within(what, async {
        loop {
            let found = events
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, payload)| name == "tunnel.status" && payload["id"] == id)
                .map(|(_, payload)| payload.clone())
                .find(|payload| wanted(payload));
            if let Some(payload) = found {
                return payload;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
}

/// A second connection as the same user, to prepare and inspect the remote
/// side with shell commands.
async fn pw_session(e2e: &E2e) -> Session {
    within(
        "connect",
        Session::connect(e2e.russh_pw_target(), &e2e.pw_scripted()),
    )
    .await
    .expect("connect")
}

async fn sh(session: &Session, command: &str) -> String {
    let output = within("exec", session.exec(command)).await.expect("exec");
    assert_eq!(
        output.exit,
        ExitStatus::Code(0),
        "{command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout_text()
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_forward_frees_its_port_when_the_connection_drops_and_comes_back() {
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let engine = engine_with_events(PW_PASSWORD, Arc::default(), events.clone());
    // The first attempt waits long enough to see the port closed meanwhile.
    engine
        .tunnels()
        .set_policy(easyssh_engine::supervisor::ReconnectPolicy {
            max_attempts: 5,
            base: Duration::from_secs(2),
            cap: Duration::from_secs(4),
        });
    let target = target();
    let tunnel = Tunnel {
        id: "e2e-comeback".into(),
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
    wait_for_port_open(port, Duration::from_secs(10)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    echo_roundtrip(&mut stream, "before").await;
    drop(stream);

    // The connection goes away (as after sleep or a network change).
    engine.forget_connection(&target).await;
    let lost = tunnel_event(&events, "e2e-comeback", "reconnecting", |p| {
        p["state"] == "reconnecting"
    })
    .await;
    assert!(
        lost["error"].as_str().is_some_and(|e| !e.is_empty()),
        "{lost}"
    );
    // Nothing listens while there is no connection behind the port.
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "port {port} still accepts connections with no SSH connection"
    );

    // It comes back on a new connection, on the same port.
    let back = within("running again", async {
        loop {
            let states: Vec<Value> = events
                .lock()
                .unwrap()
                .iter()
                .filter(|(name, p)| name == "tunnel.status" && p["id"] == "e2e-comeback")
                .map(|(_, p)| p.clone())
                .collect();
            let lost_at = states.iter().position(|p| p["state"] == "reconnecting");
            if let Some(back) =
                lost_at.and_then(|at| states[at..].iter().find(|p| p["state"] == "running"))
            {
                return back.clone();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert_eq!(back["bind_port"], port);
    wait_for_port_open(port, Duration::from_secs(10)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    echo_roundtrip(&mut stream, "after").await;
    drop(stream);
    assert_eq!(engine.connections().await["opened"], 2);

    engine.stop_tunnel("e2e-comeback").await;
    wait_for_port_shut(port, Duration::from_secs(10)).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn an_upload_replaces_an_existing_file_and_leaves_no_temporary_file() {
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sftp_id, home) = within("sftp", engine.open_files(&target()))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let dir = format!("{home}/{}", unique_name("e2e-replace"));
    sh(
        &shell,
        &format!("mkdir '{dir}' && printf old > '{dir}/notes.txt' && chmod 640 '{dir}/notes.txt'"),
    )
    .await;

    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("notes.txt");
    let data = sample_bytes(200_000);
    std::fs::write(&local, &data).unwrap();
    let remote = format!("{dir}/notes.txt");
    let end = run_transfer(&engine, entry.clone(), true, local, remote.clone()).await;
    assert_eq!(end.status, "completed", "{:?}", end.error);

    assert_eq!(
        sh(&shell, &format!("stat -c '%s %a' '{remote}'"))
            .await
            .trim(),
        format!("{} 640", data.len())
    );
    assert_eq!(sh(&shell, &format!("ls -A '{dir}'")).await, "notes.txt\n");
    sh(&shell, &format!("rm -r '{dir}'")).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_cancelled_upload_leaves_the_original_intact_and_no_temporary_file() {
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sftp_id, home) = within("sftp", engine.open_files(&target()))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let dir = format!("{home}/{}", unique_name("e2e-cancel"));
    sh(
        &shell,
        &format!("mkdir '{dir}' && printf original > '{dir}/big.bin'"),
    )
    .await;

    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("big.bin");
    std::fs::write(&local, sample_bytes(32 * 1024 * 1024)).unwrap();
    let (sender, mut receiver) = mpsc::unbounded_channel::<(String, Value)>();
    let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
        let _ = sender.send((name.to_owned(), payload));
        1usize
    });
    let id = engine.files().start_transfer(
        entry.clone(),
        true,
        local,
        format!("{dir}/big.bin"),
        false,
        sink,
    );
    // Cancel once bytes are on their way, so the temporary file exists.
    let status = within("cancel", async {
        loop {
            let (name, payload) = receiver.recv().await.expect("events");
            match name.as_str() {
                "transfer.progress" => engine.files().cancel_transfer(&id),
                "transfer.done" => return payload["status"].as_str().unwrap().to_owned(),
                _ => {}
            }
        }
    })
    .await;
    assert_eq!(status, "cancelled");
    assert_eq!(
        sh(&shell, &format!("cat '{dir}/big.bin'")).await,
        "original"
    );
    assert_eq!(sh(&shell, &format!("ls -A '{dir}'")).await, "big.bin\n");
    sh(&shell, &format!("rm -r '{dir}'")).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_forward_to_a_closed_port_reports_why_its_connections_fail() {
    let events: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let engine = engine_with_events(PW_PASSWORD, Arc::default(), events.clone());
    let target = target();
    let tunnel = Tunnel {
        id: "e2e-refused".into(),
        name: "nothing".into(),
        host_id: target.host_id.clone(),
        kind: TunnelKind::Local,
        bind_address: None,
        bind_port: 0,
        // Nothing listens on port 1 of the server.
        target_host: Some("127.0.0.1".into()),
        target_port: Some(1),
    };
    let port = within("forward", engine.start_tunnel(&tunnel, &target))
        .await
        .expect("start the forward");
    wait_for_port_open(port, Duration::from_secs(10)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    // The server refuses the channel, so the connection just ends.
    let mut buffer = [0u8; 16];
    let _ = within("refused", stream.read(&mut buffer)).await;
    let warned = tunnel_event(&events, "e2e-refused", "the failure", |p| {
        p["channel_errors"].as_u64().is_some_and(|n| n >= 1)
    })
    .await;
    assert_eq!(warned["state"], "running", "{warned}");
    let reason = warned["last_channel_error"].as_str().unwrap_or_default();
    assert!(reason.starts_with("127.0.0.1:1: "), "{reason}");
    let row = engine.tunnels().list()["tunnels"][0].clone();
    // The probe of wait_for_port_open was refused too.
    assert!(row["channel_errors"].as_u64() >= Some(1), "{row}");
    assert_eq!(
        engine.tunnels().ui_status_of("e2e-refused")["lastError"],
        reason
    );
    engine.stop_tunnel("e2e-refused").await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_failed_upload_leaves_the_original_intact_and_no_temporary_file() {
    let e2e = E2e::new();
    // Another user's file in the sticky /tmp: the upload can write its
    // temporary file next to it but cannot replace it.
    let owner = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    let remote = format!("/tmp/{}.txt", unique_name("e2e-theirs"));
    sh(
        &owner,
        &format!("printf theirs > '{remote}' && chmod 666 '{remote}'"),
    )
    .await;
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sftp_id, _) = within("sftp", engine.open_files(&target()))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("mine.txt");
    std::fs::write(&local, sample_bytes(100_000)).unwrap();
    let end = run_transfer(&engine, entry.clone(), true, local, remote.clone()).await;
    assert_eq!(end.status, "failed");
    assert!(end.error.is_some());
    assert_eq!(sh(&shell, &format!("cat '{remote}'")).await, "theirs");
    let part = remote.replacen("/tmp/", "/tmp/.", 1) + ".easyssh-part";
    assert_eq!(
        sh(
            &shell,
            &format!("test -e '{part}' && echo left || echo gone")
        )
        .await,
        "gone\n"
    );
    sh(&owner, &format!("rm -f '{remote}'")).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_failed_folder_download_keeps_the_files_it_finished() {
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sftp_id, home) = within("sftp", engine.open_files(&target()))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let dir = format!("{home}/{}", unique_name("e2e-partial"));
    // Listed by name: a.txt arrives, then b.txt cannot be read.
    sh(
        &shell,
        &format!(
            "mkdir '{dir}' && printf done > '{dir}/a.txt' && printf secret > '{dir}/b.txt' \
             && chmod 000 '{dir}/b.txt'"
        ),
    )
    .await;
    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("partial");
    let end = run_transfer_with(
        &engine,
        entry.clone(),
        false,
        local.clone(),
        dir.clone(),
        true,
    )
    .await;
    assert_eq!(end.status, "failed");
    assert_eq!(std::fs::read(local.join("a.txt")).unwrap(), b"done");
    let mut names: Vec<String> = std::fs::read_dir(&local)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.txt"]);
    sh(&shell, &format!("rm -rf '{dir}'")).await;
    engine.shutdown().await;
}

async fn next_done(receiver: &mut mpsc::UnboundedReceiver<(String, Value)>) -> Value {
    within("transfer", async {
        loop {
            let (name, payload) = receiver.recv().await.expect("events");
            if name == "transfer.done" {
                return payload;
            }
        }
    })
    .await
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// What the transfers of one queue reported, per transfer.
#[derive(Default)]
struct QueueLog {
    started: Vec<String>,
    done: Vec<(String, String, Option<String>)>,
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn dragging_many_files_queues_them_and_never_exceeds_the_session_limit() {
    // sshd allows 10 sessions per connection; before the queue every
    // transfer opened its own file session and the 11th failed.
    const FILES: usize = 25;
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let target = target();
    let (sftp_id, home) = within("sftp", engine.open_files(&target))
        .await
        .expect("open the files");
    let dir = format!("{home}/{}", unique_name("e2e-queue"));
    sh(&shell, &format!("mkdir '{dir}'")).await;

    let scratch = tempfile::tempdir().unwrap();
    let log = Arc::new(Mutex::new(QueueLog::default()));
    let (sender, mut receiver) = mpsc::unbounded_channel::<()>();
    let sink: easyssh_engine::Events = {
        let log = log.clone();
        Arc::new(move |name: &str, payload: Value| {
            let id = payload["transfer_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            match name {
                "transfer.started" => log.lock().unwrap().started.push(id),
                "transfer.done" => {
                    log.lock().unwrap().done.push((
                        id,
                        payload["status"].as_str().unwrap().to_owned(),
                        payload["error"].as_str().map(str::to_owned),
                    ));
                    let _ = sender.send(());
                }
                _ => {}
            }
            1usize
        })
    };
    let mut expected = Vec::new();
    let mut queued = 0;
    for index in 0..FILES {
        let data = sample_bytes(2 * 1024 * 1024 + index * 1_000);
        let local = scratch.path().join(format!("f{index:02}.bin"));
        std::fs::write(&local, &data).unwrap();
        let (_, waits) = engine.queue_transfer(
            "e2e",
            target.clone(),
            true,
            local,
            format!("{dir}/f{index:02}.bin"),
            false,
            sink.clone(),
        );
        queued += usize::from(waits);
        expected.push((format!("f{index:02}.bin"), sha256_hex(&data)));
    }
    assert!(queued > 0, "more files than the queue runs at once wait");
    within("queue", async {
        for _ in 0..FILES {
            receiver.recv().await.expect("transfer events");
        }
    })
    .await;
    {
        let log = log.lock().unwrap();
        let failed: Vec<_> = log.done.iter().filter(|d| d.1 != "completed").collect();
        assert!(failed.is_empty(), "{failed:?}");
        assert_eq!(
            log.started.len(),
            queued,
            "every waiting transfer got its turn"
        );
    }
    let sums = sh(&shell, &format!("cd '{dir}' && sha256sum f*.bin")).await;
    for (name, sum) in &expected {
        assert!(sums.contains(&format!("{sum}  {name}")), "{name} differs");
    }
    assert_eq!(
        sh(&shell, &format!("ls -A '{dir}' | wc -l")).await.trim(),
        FILES.to_string(),
        "no temporary files stay behind"
    );
    sh(&shell, &format!("rm -r '{dir}'")).await;
    engine.files().close(&sftp_id).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_transfer_waiting_in_the_queue_can_be_cancelled_and_never_starts() {
    let engine = engine(PW_PASSWORD, Arc::default());
    let target = target();
    let (sftp_id, home) = within("sftp", engine.open_files(&target))
        .await
        .expect("open the files");
    let scratch = tempfile::tempdir().unwrap();
    let big = scratch.path().join("big.bin");
    std::fs::write(&big, sample_bytes(8 * 1024 * 1024)).unwrap();
    let events = Arc::new(Mutex::new(Vec::<(String, String, String)>::new()));
    let sink: easyssh_engine::Events = {
        let events = events.clone();
        Arc::new(move |name: &str, payload: Value| {
            events.lock().unwrap().push((
                name.to_owned(),
                payload["transfer_id"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                payload["status"].as_str().unwrap_or_default().to_owned(),
            ));
            1usize
        })
    };
    let base = unique_name("e2e-queued-cancel");
    let mut ids = Vec::new();
    for index in 0..8 {
        let (id, _) = engine.queue_transfer(
            "e2e",
            target.clone(),
            true,
            big.clone(),
            format!("{home}/{base}-{index}.bin"),
            false,
            sink.clone(),
        );
        ids.push(id);
    }
    // The last one is certainly still waiting.
    let waiting = ids.last().unwrap().clone();
    engine.files().cancel_transfer(&waiting);
    for id in &ids[..ids.len() - 1] {
        engine.files().cancel_transfer(id);
    }
    within("cancels", async {
        loop {
            let done = events
                .lock()
                .unwrap()
                .iter()
                .filter(|e| e.0 == "transfer.done")
                .count();
            if done == ids.len() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    let events = events.lock().unwrap().clone();
    assert!(events
        .iter()
        .any(|e| e.0 == "transfer.done" && e.1 == waiting && e.2 == "cancelled"));
    assert!(
        !events
            .iter()
            .any(|e| e.0 == "transfer.started" && e.1 == waiting),
        "a cancelled transfer never leaves the queue"
    );
    engine.files().close(&sftp_id).await;
    engine.shutdown().await;
}

#[cfg(unix)]
fn mode_and_mtime(path: &std::path::Path) -> (u32, u64) {
    use std::os::unix::fs::PermissionsExt;
    let metadata = std::fs::metadata(path).unwrap();
    let mtime = metadata
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    (metadata.permissions().mode() & 0o777, mtime)
}

/// 2020-01-02 03:04:05 UTC.
const OLD_MTIME: u64 = 1_577_934_245;

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn transfers_keep_permissions_and_modification_times() {
    use std::os::unix::fs::PermissionsExt;
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sftp_id, home) = within("sftp", engine.open_files(&target()))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let dir = format!("{home}/{}", unique_name("e2e-modes"));
    sh(&shell, &format!("mkdir '{dir}'")).await;

    // Upload: an executable script with an old time.
    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("run.sh");
    std::fs::write(&local, "#!/bin/sh\necho hi\n").unwrap();
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::File::options()
        .write(true)
        .open(&local)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(OLD_MTIME))
        .unwrap();
    let remote = format!("{dir}/run.sh");
    let end = run_transfer(&engine, entry.clone(), true, local, remote.clone()).await;
    assert_eq!(end.status, "completed", "{:?}", end.error);
    assert_eq!(
        sh(&shell, &format!("stat -c '%a %Y' '{remote}'"))
            .await
            .trim(),
        format!("755 {OLD_MTIME}")
    );

    // Download: a private file with an old time.
    sh(
        &shell,
        &format!(
            "printf s > '{dir}/key' && chmod 600 '{dir}/key' && touch -d @{OLD_MTIME} '{dir}/key'"
        ),
    )
    .await;
    let back = scratch.path().join("key");
    let end = run_transfer(
        &engine,
        entry.clone(),
        false,
        back.clone(),
        format!("{dir}/key"),
    )
    .await;
    assert_eq!(end.status, "completed", "{:?}", end.error);
    assert_eq!(mode_and_mtime(&back), (0o600, OLD_MTIME));

    sh(&shell, &format!("rm -r '{dir}'")).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_large_file_round_trips_through_the_pipeline_with_the_same_checksum() {
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let target = target();
    let (sftp_id, home) = within("sftp", engine.open_files(&target))
        .await
        .expect("open the files");
    let scratch = tempfile::tempdir().unwrap();
    // Not a multiple of any request size, so the last read is short.
    let data = sample_bytes(24 * 1024 * 1024 + 12_345);
    let local = scratch.path().join("large.bin");
    std::fs::write(&local, &data).unwrap();
    let remote = format!("{home}/{}.bin", unique_name("e2e-large"));
    let sum = sha256_hex(&data);

    let (sender, mut receiver) = mpsc::unbounded_channel::<(String, Value)>();
    let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
        let _ = sender.send((name.to_owned(), payload));
        1usize
    });
    let run = |upload: bool, from: std::path::PathBuf, to: String| {
        let (id, _) =
            engine.queue_transfer("e2e", target.clone(), upload, from, to, false, sink.clone());
        id
    };
    let id = run(true, local.clone(), remote.clone());
    let done = next_done(&mut receiver).await;
    assert_eq!(done["transfer_id"], id.as_str());
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(
        sh(&shell, &format!("sha256sum '{remote}'"))
            .await
            .split_whitespace()
            .next(),
        Some(sum.as_str()),
        "uploaded bytes"
    );

    let back = scratch.path().join("back.bin");
    let id = run(false, back.clone(), remote.clone());
    let done = next_done(&mut receiver).await;
    assert_eq!(done["transfer_id"], id.as_str());
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(
        sha256_hex(&std::fs::read(&back).unwrap()),
        sum,
        "downloaded bytes"
    );
    sh(&shell, &format!("rm '{remote}'")).await;
    engine.files().close(&sftp_id).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_folder_download_follows_links_and_keeps_odd_names() {
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sftp_id, home) = within("sftp", engine.open_files(&target()))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let dir = format!("{home}/{}", unique_name("e2e-links"));
    sh(
        &shell,
        &format!(
            "set -e; mkdir -p '{dir}/sub'; cd '{dir}'; printf script > run.sh; chmod 755 run.sh; \
             printf log > 'a:b.log'; printf inner > sub/inner.txt; ln -s run.sh alias.sh; \
             ln -s sub linkdir; ln -s . loop; ln -s nowhere broken"
        ),
    )
    .await;

    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("copy");
    let end = run_transfer_with(
        &engine,
        entry.clone(),
        false,
        local.clone(),
        dir.clone(),
        true,
    )
    .await;
    assert_eq!(end.status, "completed", "{:?}", end.error);
    // The loop back to the folder itself and the broken link.
    assert_eq!(end.skipped, 2);

    let mut names: Vec<String> = std::fs::read_dir(&local)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a_b.log", "alias.sh", "linkdir", "run.sh", "sub"]);
    assert_eq!(std::fs::read(local.join("a_b.log")).unwrap(), b"log");
    assert_eq!(std::fs::read(local.join("alias.sh")).unwrap(), b"script");
    assert!(std::fs::symlink_metadata(local.join("alias.sh"))
        .unwrap()
        .is_file());
    assert_eq!(
        std::fs::read(local.join("linkdir/inner.txt")).unwrap(),
        b"inner"
    );
    assert_eq!(
        std::fs::read(local.join("sub/inner.txt")).unwrap(),
        b"inner"
    );
    assert_eq!(mode_and_mtime(&local.join("run.sh")).0, 0o755);

    // The phone's listing shows the odd name too.
    let listing = entry.list_view(&dir).await.unwrap();
    assert!(
        listing.iter().any(|row| row["name"] == "a:b.log"),
        "{listing:?}"
    );

    sh(&shell, &format!("rm -r '{dir}'")).await;
    engine.shutdown().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_folder_upload_follows_links_without_going_round_in_circles() {
    let e2e = E2e::new();
    let shell = pw_session(&e2e).await;
    let engine = engine(PW_PASSWORD, Arc::default());
    let (sftp_id, home) = within("sftp", engine.open_files(&target()))
        .await
        .expect("open the files");
    let entry = engine.files().get(&sftp_id).unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let local = scratch.path().join("site");
    std::fs::create_dir_all(local.join("assets")).unwrap();
    std::fs::write(local.join("index.html"), "home").unwrap();
    std::fs::write(local.join("assets/app.js"), "js").unwrap();
    std::os::unix::fs::symlink("index.html", local.join("default.html")).unwrap();
    std::os::unix::fs::symlink("assets", local.join("static")).unwrap();
    std::os::unix::fs::symlink("..", local.join("assets/up")).unwrap();
    std::os::unix::fs::symlink("missing", local.join("broken")).unwrap();

    let remote = format!("{home}/{}", unique_name("e2e-site"));
    let end = run_transfer_with(&engine, entry.clone(), true, local, remote.clone(), true).await;
    assert_eq!(end.status, "completed", "{:?}", end.error);
    // `assets/up` and `static/up` lead back up; `broken` points nowhere.
    assert_eq!(end.skipped, 3);
    assert_eq!(
        sh(&shell, &format!("cd '{remote}' && find . -type f | sort")).await,
        "./assets/app.js\n./default.html\n./index.html\n./static/app.js\n"
    );
    assert_eq!(
        sh(&shell, &format!("find '{remote}' -type l | wc -l"))
            .await
            .trim(),
        "0"
    );
    sh(&shell, &format!("rm -r '{remote}'")).await;
    engine.shutdown().await;
}

// ---- resumable transfers and SHA-256 checks --------------------------------

mod resume {
    use super::*;
    use easyssh_engine::files::{check_file, Check, Options, SftpEntry};
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};

    fn sha_hex(data: &[u8]) -> String {
        Sha256::digest(data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    struct Run {
        events: Vec<(String, Value)>,
        status: String,
    }

    impl Run {
        fn note(&self) -> Option<(String, Option<u64>)> {
            self.events.iter().find_map(|(name, payload)| {
                (name == "transfer.progress" && payload.get("note").is_some()).then(|| {
                    (
                        payload["note"].as_str().unwrap().to_owned(),
                        payload["resumed_from"].as_u64(),
                    )
                })
            })
        }

        fn done(&self) -> &Value {
            &self.events.last().expect("events").1
        }
    }

    /// Runs one transfer to its end; `cancel_at` cancels it once that many
    /// bytes have been reported.
    async fn run(
        engine: &Engine,
        entry: &Arc<SftpEntry>,
        upload: bool,
        local: &Path,
        remote: &str,
        options: Options,
        cancel_at: Option<u64>,
    ) -> Run {
        let (sender, mut receiver) = mpsc::unbounded_channel::<(String, Value)>();
        let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
            let _ = sender.send((name.to_owned(), payload));
            1usize
        });
        let id = engine.files().start_transfer_with(
            entry.clone(),
            upload,
            local.to_owned(),
            remote.to_owned(),
            local.is_dir(),
            options,
            sink,
        );
        let mut events = Vec::new();
        let status = within("transfer", async {
            loop {
                let (name, payload) = receiver.recv().await.expect("events");
                if let (Some(at), "transfer.progress") = (cancel_at, name.as_str()) {
                    if payload["bytes"].as_u64().unwrap_or(0) >= at {
                        engine.files().cancel_transfer(&id);
                    }
                }
                let done = name == "transfer.done";
                let status = payload["status"].as_str().map(str::to_owned);
                events.push((name, payload));
                if done {
                    return status.unwrap();
                }
            }
        })
        .await;
        Run { events, status }
    }

    struct Setup {
        _e2e: E2e,
        shell: Session,
        engine: Arc<Engine>,
        entry: Arc<SftpEntry>,
        dir: String,
        scratch: tempfile::TempDir,
    }

    async fn setup() -> Setup {
        let e2e = E2e::new();
        let shell = pw_session(&e2e).await;
        let engine = engine(PW_PASSWORD, Arc::default());
        let scratch = tempfile::tempdir().unwrap();
        engine
            .files()
            .set_resume_dir(scratch.path().join("journal"));
        let (sftp_id, home) = within("sftp", engine.open_files(&target()))
            .await
            .expect("open the files");
        let entry = engine.files().get(&sftp_id).unwrap();
        let dir = format!("{home}/{}", unique_name("e2e-resume"));
        sh(&shell, &format!("mkdir '{dir}'")).await;
        Setup {
            _e2e: e2e,
            shell,
            engine,
            entry,
            dir,
            scratch,
        }
    }

    impl Setup {
        async fn finish(self) {
            sh(&self.shell, &format!("rm -rf '{}'", self.dir)).await;
            self.engine.shutdown().await;
        }

        fn local(&self, name: &str) -> PathBuf {
            self.scratch.path().join(name)
        }
    }

    const RESUME: Options = Options {
        resume: true,
        verify: true,
    };

    #[tokio::test]
    #[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
    async fn a_cancelled_download_keeps_its_part_and_resumes_to_the_same_bytes() {
        let s = setup().await;
        let remote = format!("{}/big 报告.bin", s.dir);
        sh(
            &s.shell,
            &format!("head -c 48000000 /dev/urandom > '{remote}'"),
        )
        .await;
        let remote_sha = sh(&s.shell, &format!("sha256sum < '{remote}'")).await;
        let local = s.local("big 报告.bin");
        let first = run(
            &s.engine,
            &s.entry,
            false,
            &local,
            &remote,
            Options::default(),
            Some(6_000_000),
        )
        .await;
        assert_eq!(first.status, "cancelled");
        let part = s.local(".big 报告.bin.easyssh-part");
        assert!(part.exists(), "the part stays after a cancel");
        assert!(!local.exists(), "the real name is not written");
        let kept = std::fs::metadata(&part).unwrap().len();
        assert!(kept > 0 && kept < 48_000_000, "kept {kept}");
        let found = s.engine.files().partials("", false, None, &remote);
        assert_eq!(found.len(), 1, "the journal knows it");

        let second = run(&s.engine, &s.entry, false, &local, &remote, RESUME, None).await;
        assert_eq!(second.status, "completed", "{:?}", second.done());
        let (note, from) = second.note().expect("a note");
        assert_eq!(note, "resumed");
        assert!(
            from.unwrap() >= kept / 2,
            "resumed from {from:?}, kept {kept}"
        );
        assert!(!part.exists(), "the part is gone on success");
        assert!(s
            .engine
            .files()
            .partials("", false, None, &remote)
            .is_empty());
        assert_eq!(second.done()["verify"]["status"], "match");
        assert_eq!(
            sha_hex(&std::fs::read(&local).unwrap()),
            remote_sha.split_whitespace().next().unwrap()
        );
        s.finish().await;
    }

    #[tokio::test]
    #[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
    async fn a_cancelled_upload_keeps_its_part_and_resumes_to_the_same_bytes() {
        let s = setup().await;
        let remote = format!("{}/up 文件.bin", s.dir);
        sh(&s.shell, &format!("printf original > '{remote}'")).await;
        let local = s.local("up 文件.bin");
        let data = sample_bytes(40_000_000);
        std::fs::write(&local, &data).unwrap();
        let first = run(
            &s.engine,
            &s.entry,
            true,
            &local,
            &remote,
            Options::default(),
            Some(14_000_000),
        )
        .await;
        assert_eq!(first.status, "cancelled");
        // The original is intact and the part stays beside it.
        assert_eq!(sh(&s.shell, &format!("cat '{remote}'")).await, "original");
        let listing = sh(&s.shell, &format!("ls -A '{}'", s.dir)).await;
        assert!(listing.contains(".up 文件.bin.easyssh-part"), "{listing}");
        assert_eq!(s.engine.files().partials("", true, None, &remote).len(), 1);

        let second = run(&s.engine, &s.entry, true, &local, &remote, RESUME, None).await;
        assert_eq!(second.status, "completed", "{:?}", second.done());
        let (note, from) = second.note().expect("a note");
        assert_eq!(note, "resumed");
        assert!(from.unwrap() > 0);
        assert_eq!(second.done()["verify"]["status"], "match");
        let listing = sh(&s.shell, &format!("ls -A '{}'", s.dir)).await;
        assert_eq!(listing, "up 文件.bin\n", "no temporary file is left");
        let remote_sha = sh(&s.shell, &format!("sha256sum < '{remote}'")).await;
        assert_eq!(
            remote_sha.split_whitespace().next().unwrap(),
            sha_hex(&data)
        );
        s.finish().await;
    }

    #[tokio::test]
    #[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
    async fn a_source_that_changed_starts_again_from_zero_with_a_note() {
        let s = setup().await;
        let remote = format!("{}/changing.bin", s.dir);
        let local = s.local("changing.bin");
        std::fs::write(&local, sample_bytes(30_000_000)).unwrap();
        let first = run(
            &s.engine,
            &s.entry,
            true,
            &local,
            &remote,
            Options::default(),
            Some(10_000_000),
        )
        .await;
        assert_eq!(first.status, "cancelled");
        // Another file with the same name now.
        let changed = sample_bytes(30_000_001);
        std::fs::write(&local, &changed).unwrap();
        let second = run(&s.engine, &s.entry, true, &local, &remote, RESUME, None).await;
        assert_eq!(second.status, "completed", "{:?}", second.done());
        assert_eq!(
            second.note().map(|n| n.0).as_deref(),
            Some("source_changed")
        );
        let remote_sha = sh(&s.shell, &format!("sha256sum < '{remote}'")).await;
        assert_eq!(
            remote_sha.split_whitespace().next().unwrap(),
            sha_hex(&changed)
        );
        s.finish().await;
    }

    #[tokio::test]
    #[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
    async fn a_resumed_folder_skips_complete_files_and_discard_removes_the_rest() {
        let s = setup().await;
        let tree = s.local("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(tree.join("a.txt"), b"alpha").unwrap();
        std::fs::write(tree.join("empty"), b"").unwrap();
        std::fs::write(tree.join("big.bin"), sample_bytes(30_000_000)).unwrap();
        let remote = format!("{}/tree", s.dir);
        let (sender, mut receiver) = mpsc::unbounded_channel::<(String, Value)>();
        let sink: easyssh_engine::Events = Arc::new(move |name: &str, payload: Value| {
            let _ = sender.send((name.to_owned(), payload));
            1usize
        });
        let id = s.engine.files().start_transfer_with(
            s.entry.clone(),
            true,
            tree.clone(),
            remote.clone(),
            true,
            Options::default(),
            sink,
        );
        within("cancel", async {
            loop {
                let (name, payload) = receiver.recv().await.expect("events");
                if name == "transfer.progress" && payload["bytes"].as_u64().unwrap_or(0) > 8_000_000
                {
                    s.engine.files().cancel_transfer(&id);
                }
                if name == "transfer.done" {
                    return;
                }
            }
        })
        .await;
        let partial = s.engine.files().partials("", true, None, &remote);
        assert_eq!(partial.len(), 1, "only the cut-off file has a record");
        // Discarding removes the remote part and the record.
        let removed = s
            .engine
            .files()
            .discard_partials(&partial, Some(s.entry.sftp()))
            .await;
        assert_eq!(removed, 1);
        assert!(s
            .engine
            .files()
            .partials("", true, None, &remote)
            .is_empty());
        let listing = sh(&s.shell, &format!("ls -A '{remote}'")).await;
        assert!(!listing.contains("easyssh-part"), "{listing}");

        // Resuming the whole folder finishes it; files that were already
        // there are not sent again.
        let done = run(&s.engine, &s.entry, true, &tree, &remote, RESUME, None).await;
        assert_eq!(done.status, "completed", "{:?}", done.done());
        // Files that were already there complete are left out, and so are
        // their checks.
        let checked = done.done()["verify"]["files"].as_u64().unwrap();
        assert!((1..=3).contains(&checked), "{checked} files checked");
        let sums = sh(
            &s.shell,
            &format!("cd '{remote}' && sha256sum a.txt empty big.bin"),
        )
        .await;
        assert!(sums.contains(&sha_hex(b"alpha")), "{sums}");
        assert!(sums.contains(&sha_hex(b"")), "{sums}");
        let again = run(&s.engine, &s.entry, true, &tree, &remote, RESUME, None).await;
        assert_eq!(again.status, "completed");
        assert_eq!(again.done()["verify"]["files"], 0, "nothing was sent again");
        s.finish().await;
    }

    #[tokio::test]
    #[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
    async fn a_difference_is_reported_with_both_hashes_and_the_file_stays() {
        let s = setup().await;
        let remote = format!("{}/same size.txt", s.dir);
        let local = s.local("same size.txt");
        std::fs::write(&local, b"0123456789").unwrap();
        let ok = run(&s.engine, &s.entry, true, &local, &remote, RESUME, None).await;
        assert_eq!(ok.status, "completed");
        // Same length, other content, behind the transfer's back.
        sh(&s.shell, &format!("printf 'abcdefghij' > '{remote}'")).await;
        let checked = check_file(&s.shell, &local, &remote).await.expect("check");
        match checked {
            Check::Mismatch { local, remote } => {
                assert_eq!(local, sha_hex(b"0123456789"));
                assert_eq!(remote, sha_hex(b"abcdefghij"));
            }
            _ => panic!("the difference was not found"),
        }
        assert_eq!(sh(&s.shell, &format!("cat '{remote}'")).await, "abcdefghij");
        s.finish().await;
    }
}
