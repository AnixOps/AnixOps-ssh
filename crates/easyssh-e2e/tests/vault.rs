//! Saved secrets against real servers, on both engines: the password user
//! logs in with a password from the vault and no dialog, and the key user
//! logs in with a key from the vault. On OpenSSH the key exists only as a
//! 0600 file while the session runs; on the built-in engine it never touches
//! the disk at all, also when it is encrypted and its passphrase is saved.
//! The vault uses an in-memory key store; the real keychain is never touched.
#![cfg(unix)]

#[macro_use]
mod support;

use easyssh_core::{ConfigStore, Connection, ConnectionTarget};
use easyssh_crypto::{KdfParams, MemoryKeyStore};
use easyssh_e2e::{run_command, E2e, PW_PASSWORD, PW_USER, TARGET_PORT};
use easyssh_vault::Vault;
use easysshd::link::{Link, SessionMsg, SessionSink};
use easysshd::{Daemon, DaemonConfig, DaemonExtras, Server};
use serde_json::json;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
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
    client: Arc<Client>,
    runtime: PathBuf,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<()>,
    daemon: Arc<Daemon>,
}

async fn start(e2e: &E2e, engine: EngineKind) -> Harness {
    let runtime = e2e.dir.path().join("run");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&runtime)
        .unwrap();
    let config_path = e2e.dir.path().join("connections.json");
    let config = DaemonConfig {
        config_store: ConfigStore::at(config_path.clone()),
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
    set_engine(&config_path, engine);
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
    let (client, link) = Client::connect(&runtime).await;
    Harness {
        link,
        client,
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
        self.client.detach();
        drop(self.link);
        let _ = self.stop.send(());
        self.serving.await.expect("server task");
        self.daemon.shutdown();
    }
}

async fn stored_password_logs_in_without_a_dialog(engine: EngineKind) {
    let e2e = E2e::new();
    let h = start(&e2e, engine).await;
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
        h.client.asks.lock().unwrap().is_empty(),
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

both_engines!(stored_password_logs_in_without_a_dialog);

/// A key saved in the vault logs the key user in. The config's own
/// `IdentityFile` is removed first, so the saved key is the only way in.
/// OpenSSH gets it as one owner-only file for as long as `ssh` runs; the
/// built-in engine keeps it in memory and never writes a file.
async fn stored_key_logs_in(engine: EngineKind) {
    let e2e = E2e::new();
    let h = start(&e2e, engine).await;
    let host = endpoint_host("E2E key host", "keyuser");
    h.save(&host).await;
    let key = std::fs::read_to_string(&e2e.key).expect("test key");
    std::fs::remove_file(&e2e.key).expect("remove the config's key file");
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

    match engine {
        EngineKind::Openssh => {
            // While the session lives the key is one owner-only file.
            let files = key_files(&h.runtime);
            assert_eq!(files.len(), 1, "{files:?}");
            assert_eq!(
                std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        EngineKind::Russh => {
            // The key went to the engine from memory: no file at any time.
            assert!(
                key_files(&h.runtime).is_empty(),
                "{:?}",
                key_files(&h.runtime)
            );
        }
    }
    assert!(h.client.asks.lock().unwrap().is_empty());

    h.link.write_session(&id, "exit\r").unwrap();
    eventually("the key file to be deleted", || {
        key_files(&h.runtime).is_empty()
    })
    .await;
    h.link.close_session(&id);
    h.stop().await;
}

both_engines!(stored_key_logs_in);

/// Copies the test key and protects the copy with a passphrase.
fn encrypted_key(e2e: &E2e, passphrase: &str) -> String {
    let copy = e2e.dir.path().join("id_e2e_vault_encrypted");
    std::fs::copy(&e2e.key, &copy).unwrap();
    let mut command = std::process::Command::new("ssh-keygen");
    command
        .args(["-q", "-p", "-N", passphrase, "-f"])
        .arg(&copy);
    let output = run_command(command, Duration::from_secs(30));
    assert!(output.status.success(), "ssh-keygen -p failed");
    let text = std::fs::read_to_string(&copy).unwrap();
    std::fs::remove_file(&copy).unwrap();
    assert!(text.contains("OPENSSH PRIVATE KEY"));
    text
}

/// An encrypted key and its passphrase, both saved in the vault, log the key
/// user in on the built-in engine: no dialog (neither for the passphrase nor
/// for the host), no key file on disk, and the key text never appears in the
/// terminal. The config's own `IdentityFile` is gone, so only the vault key
/// can have worked.
#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn stored_encrypted_key_and_passphrase_log_in_on_the_built_in_engine() {
    let e2e = E2e::new();
    let h = start(&e2e, EngineKind::Russh).await;
    let host = endpoint_host("E2E encrypted key host", "keyuser");
    h.save(&host).await;
    let key = encrypted_key(&e2e, "e2e vault passphrase");
    std::fs::remove_file(&e2e.key).expect("remove the config's key file");
    h.link
        .request(
            "secrets.set",
            json!({
                "host_id": host.id,
                "private_key": key,
                "passphrase": "e2e vault passphrase",
            }),
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
    assert!(
        h.client.asks.lock().unwrap().is_empty(),
        "a saved passphrase must not raise a dialog: {:?}",
        h.client.asks.lock().unwrap()
    );
    assert!(key_files(&h.runtime).is_empty());
    assert!(!text(&seen).contains("e2e vault passphrase"));
    assert!(!text(&seen).contains("PRIVATE KEY"));
    // Nothing under the test directory holds the key either.
    let body = key.lines().nth(1).unwrap().to_owned();
    assert!(
        files_containing(e2e.dir.path(), &body).is_empty(),
        "{:?}",
        files_containing(e2e.dir.path(), &body)
    );
    h.link.write_session(&id, "exit\r").unwrap();
    h.link.close_session(&id);
    h.stop().await;
}

/// Without a saved passphrase the encrypted vault key asks the user, once,
/// and the answer unlocks it.
#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn stored_encrypted_key_without_a_passphrase_asks_for_it() {
    let e2e = E2e::new();
    let h = start(&e2e, EngineKind::Russh).await;
    h.client.set_password(Some("e2e vault passphrase"));
    let host = endpoint_host("E2E encrypted key host", "keyuser");
    h.save(&host).await;
    let key = encrypted_key(&e2e, "e2e vault passphrase");
    std::fs::remove_file(&e2e.key).expect("remove the config's key file");
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
    let asks = h.client.asks.lock().unwrap().clone();
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert_eq!(asks[0].kind.as_deref(), Some("passphrase"));
    h.link.write_session(&id, "exit\r").unwrap();
    h.link.close_session(&id);
    h.stop().await;
}

/// Every file under `dir` that contains `needle`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                    found.push(path);
                }
            }
        }
    }
    found
}
