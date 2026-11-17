//! Saved secrets against real servers: the password user logs in with a
//! password from the vault and no dialog, and the key user logs in with a key
//! from the vault that exists only as a 0600 file while the session runs.
//! The vault uses an in-memory key store; the real keychain is never touched.
#![cfg(unix)]

use easyssh_core::{ConfigStore, Connection, ConnectionTarget};
use easyssh_crypto::{KdfParams, MemoryKeyStore};
use easyssh_e2e::{E2e, PW_PASSWORD, PW_USER, TARGET_PORT};
use easyssh_vault::Vault;
use easysshd::link::{AskpassPrompt, Link, LinkHandler, SessionMsg, SessionSink};
use easysshd::{Daemon, DaemonConfig, DaemonExtras, Server};
use serde_json::json;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct Prompts(Mutex<Vec<AskpassPrompt>>);

impl LinkHandler for Prompts {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        self.0.lock().unwrap().push(prompt);
    }
}

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

/// An askpass helper that forwards the prompt to the daemon the way the real
/// helper does, in bash so no binary has to be built for the test.
fn write_helper(dir: &Path) -> PathBuf {
    let path = dir.join("askpass-helper.sh");
    let body = r#"#!/bin/bash
port="${ANIXOPS_SSH_ASKPASS%% *}"
token="${ANIXOPS_SSH_ASKPASS##* }"
prompt="${1//\\/\\\\}"
prompt="${prompt//\"/\\\"}"
exec 3<>"/dev/tcp/127.0.0.1/$port" || exit 1
printf '{"token":"%s","context":"%s","host":"%s","prompt":"%s"}\n' \
  "$token" "$ANIXOPS_SSH_ASKPASS_CONTEXT" "$ANIXOPS_SSH_ASKPASS_HOST" "$prompt" >&3
IFS= read -r reply <&3
case "$reply" in
  \"*\") reply="${reply#\"}"; printf '%s\n' "${reply%\"}" ;;
  *) exit 1 ;;
esac
"#;
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn key_files(runtime: &Path) -> Vec<PathBuf> {
    match std::fs::read_dir(runtime.join("keys")) {
        Ok(entries) => entries.filter_map(|e| e.ok().map(|e| e.path())).collect(),
        Err(_) => Vec::new(),
    }
}

fn endpoint_host(name: &str, user: &str) -> Connection {
    let mut connection = Connection::alias(name, "x");
    connection.target = ConnectionTarget::Endpoint {
        hostname: "127.0.0.1".into(),
        username: Some(user.into()),
        port: TARGET_PORT,
    };
    connection
}

struct Harness {
    link: Link,
    prompts: Arc<Prompts>,
    runtime: PathBuf,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<()>,
    daemon: Arc<Daemon>,
}

async fn start(e2e: &E2e) -> Harness {
    let runtime = e2e.dir.path().join("run");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&runtime)
        .unwrap();
    let config = DaemonConfig {
        config_store: ConfigStore::at(e2e.dir.path().join("connections.json")),
        local_shell: None,
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let vault = Arc::new(Vault::with_kdf(
        Arc::new(MemoryKeyStore::new()),
        KdfParams::insecure_for_tests(),
    ));
    let extras = DaemonExtras {
        runtime_dir: Some(runtime.clone()),
        vault: Some(vault),
        askpass_helper: Some(write_helper(e2e.dir.path())),
        ..DaemonExtras::default()
    };
    let daemon = Daemon::new_with(config, extras).await.expect("daemon");
    let server = Server::start_service(&runtime, daemon.clone()).expect("server");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        server
            .serve(async {
                let _ = stopped.await;
            })
            .await
            .expect("serve")
    });
    let prompts = Arc::new(Prompts::default());
    let link = Link::connect(&runtime, prompts.clone())
        .await
        .expect("connect");
    Harness {
        link,
        prompts,
        runtime,
        stop,
        serving,
        daemon,
    }
}

impl Harness {
    async fn save(&self, connection: &Connection) {
        self.link
            .request(
                "hosts.save",
                json!({"connection": serde_json::to_value(connection).unwrap()}),
            )
            .await
            .expect("hosts.save");
    }

    async fn stop(self) {
        drop(self.link);
        let _ = self.stop.send(());
        self.serving.await.expect("server task");
        self.daemon.shutdown();
    }
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn stored_password_logs_in_without_a_dialog() {
    let e2e = E2e::new();
    let h = start(&e2e).await;
    let host = endpoint_host("E2E password host", PW_USER);
    h.save(&host).await;
    h.link
        .request(
            "secrets.set",
            json!({"host_id": host.id, "password": PW_PASSWORD}),
        )
        .await
        .expect("secrets.set");

    let (seen, sink) = collector();
    let id = h
        .link
        .open_session(Some(&host.id), 100, 30, sink)
        .await
        .expect("session.open");
    h.link
        .write_session(&id, "echo e2e-vault-$(id -un)\r")
        .unwrap();
    eventually("the password user's shell", || {
        text(&seen).contains("e2e-vault-pwuser")
    })
    .await;
    assert!(
        h.prompts.0.lock().unwrap().is_empty(),
        "a stored password must not raise a dialog"
    );
    assert!(
        !text(&seen).contains(PW_PASSWORD),
        "the password was echoed"
    );
    h.link.write_session(&id, "exit\r").unwrap();
    h.link.close_session(&id);
    h.stop().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn stored_key_logs_in_through_a_short_lived_file() {
    let e2e = E2e::new();
    let h = start(&e2e).await;
    let host = endpoint_host("E2E key host", "keyuser");
    h.save(&host).await;
    let key = std::fs::read_to_string(&e2e.key).expect("test key");
    h.link
        .request(
            "secrets.set",
            json!({"host_id": host.id, "private_key": key}),
        )
        .await
        .expect("secrets.set");

    let (seen, sink) = collector();
    let id = h
        .link
        .open_session(Some(&host.id), 100, 30, sink)
        .await
        .expect("session.open");
    h.link
        .write_session(&id, "echo e2e-vault-$(id -un)\r")
        .unwrap();
    eventually("the key user's shell", || {
        text(&seen).contains("e2e-vault-keyuser")
    })
    .await;

    // While the session lives the key is one owner-only file.
    let files = key_files(&h.runtime);
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(
        std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(h.prompts.0.lock().unwrap().is_empty());

    h.link.write_session(&id, "exit\r").unwrap();
    eventually("the key file to be deleted", || {
        key_files(&h.runtime).is_empty()
    })
    .await;
    h.link.close_session(&id);
    h.stop().await;
}
