//! Sync through the daemon against a real `easyssh-server` on a loopback
//! port: sign up and in, two devices, merges, history, revocation, shared
//! vaults and the fingerprint both sides compare. Each "device" is a whole
//! daemon with its own database and an in-memory key store; nothing leaves
//! the machine.

use easyssh_core::{ConfigStore, Connection, ConnectionTarget};
use easyssh_crypto::{KdfParams, MemoryKeyStore};
use easyssh_proto as proto;
use easyssh_server::{serve, Config};
use easyssh_vault::Vault;
use easysshd::link::{AskpassPrompt, Link, LinkHandler};
use easysshd::{Daemon, DaemonConfig, DaemonExtras, Server, SyncOptions};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const PASS: &str = "correct horse battery staple";

struct ServerFixture {
    url: String,
    _dir: TempDir,
}

async fn start_server() -> ServerFixture {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::for_db(dir.path().join("server.db"));
    config.auth_rate_per_minute = 10_000;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, config, std::future::pending()));
    ServerFixture { url, _dir: dir }
}

#[derive(Default)]
struct Events {
    sync: Mutex<Vec<Value>>,
    hosts_changed: Mutex<usize>,
}

impl LinkHandler for Events {
    fn askpass_prompt(&self, _: AskpassPrompt) {}
    fn hosts_changed(&self) {
        *self.hosts_changed.lock().unwrap() += 1;
    }
    fn sync_state(&self, status: Value) {
        self.sync.lock().unwrap().push(status);
    }
}

struct Device {
    daemon: Arc<Daemon>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    link: Link,
    events: Arc<Events>,
}

fn private_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path).unwrap();
}

fn fast() -> SyncOptions {
    SyncOptions {
        kdf: KdfParams::insecure_for_tests(),
        // Background syncing is exercised by its own test; the others call
        // `sync.now` so that they are deterministic (a debounced background
        // run after a save would merge before the test's own sync does).
        interval: Duration::from_secs(3600),
        debounce: Duration::from_secs(3600),
        poll: Duration::from_secs(3600),
    }
}

/// The same, with the debounced sync after a local change switched on.
fn background() -> SyncOptions {
    SyncOptions {
        debounce: Duration::from_millis(40),
        ..fast()
    }
}

async fn start(root: &Path, keys: Arc<MemoryKeyStore>) -> Device {
    start_with(root, "run", keys, fast()).await
}

/// A daemon over the data in `root`, with its IPC files in `root/<run>`.
async fn start_with(
    root: &Path,
    run: &str,
    keys: Arc<MemoryKeyStore>,
    options: SyncOptions,
) -> Device {
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(root.join("connections.json")),
        local_shell: Some("/bin/sh".into()),
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let runtime = root.join(run);
    let vault = Arc::new(Vault::with_kdf(
        keys.clone(),
        KdfParams::insecure_for_tests(),
    ));
    let extras = DaemonExtras {
        log_dir: Some(runtime.clone()),
        runtime_dir: Some(runtime.clone()),
        vault: Some(vault),
        askpass_helper: Some("/nonexistent/askpass-helper".into()),
        sync: options,
        ..DaemonExtras::default()
    };
    private_dir(&runtime);
    let daemon = Daemon::new_with(cfg, extras).await.unwrap();
    let server = Server::start_service(&runtime, daemon.clone()).unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        server
            .serve(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    let events = Arc::new(Events::default());
    let link = Link::connect(&runtime, events.clone()).await.unwrap();
    Device {
        daemon,
        stop,
        task,
        link,
        events,
    }
}

async fn new_device(dir: &TempDir, name: &str) -> Device {
    let root = dir.path().join(name);
    std::fs::create_dir_all(&root).unwrap();
    start(&root, Arc::new(MemoryKeyStore::new())).await
}

impl Device {
    async fn call(&self, kind: &str, payload: Value) -> Result<Value, String> {
        let command =
            json!({"id": uuid::Uuid::new_v4().to_string(), "kind": kind, "payload": payload});
        proto::validate_command(&command).map_err(|e| e.join("; "))?;
        let reply = self.link.request(kind, payload).await.map_err(|e| e.code)?;
        // Every reply must match its schema.
        if let Some((_, _, result)) = proto::COMMANDS.iter().find(|(k, _, _)| *k == kind) {
            if !result.is_empty() {
                proto::validate(result, &reply).unwrap_or_else(|e| panic!("{kind}: {e:?}"));
            }
        }
        Ok(reply)
    }

    async fn ok(&self, kind: &str, payload: Value) -> Value {
        self.call(kind, payload)
            .await
            .unwrap_or_else(|e| panic!("{kind} failed: {e}"))
    }

    async fn signup(&self, url: &str, user: &str) -> Value {
        self.ok(
            "sync.signup",
            json!({"server_url": url, "username": user, "passphrase": PASS, "device_name": user}),
        )
        .await
    }

    async fn signin(&self, url: &str, user: &str, name: &str) -> Value {
        self.ok(
            "sync.signin",
            json!({"server_url": url, "username": user, "passphrase": PASS, "device_name": name}),
        )
        .await
    }

    async fn sync(&self) -> Value {
        self.ok("sync.now", json!({})).await
    }

    async fn hosts(&self) -> Vec<Value> {
        self.ok("hosts.list", json!({})).await["connections"]
            .as_array()
            .unwrap()
            .clone()
    }

    async fn host(&self, id: &str) -> Option<Value> {
        self.hosts().await.into_iter().find(|h| h["id"] == id)
    }

    async fn save(&self, connection: &Value) {
        self.ok("hosts.save", json!({"connection": connection}))
            .await;
    }

    async fn edit(&self, id: &str, change: impl FnOnce(&mut Value)) {
        let mut host = self.host(id).await.expect("host exists");
        change(&mut host);
        self.save(&host).await;
    }

    async fn stop(self) {
        drop(self.link);
        let _ = self.stop.send(());
        self.task.await.unwrap();
        self.daemon.shutdown();
    }
}

fn host_json(name: &str) -> Value {
    let mut connection = Connection::alias(name, "x");
    connection.target = ConnectionTarget::Endpoint {
        hostname: format!("{}.example.test", name.to_lowercase()),
        username: Some("deploy".into()),
        port: 22,
    };
    serde_json::to_value(connection).unwrap()
}

async fn eventually<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_edit_on_one_device_appears_on_the_other() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let a = new_device(&dir, "a").await;
    let b = new_device(&dir, "b").await;

    let status = a.call("sync.status", json!({})).await.unwrap();
    assert_eq!(status["state"], "signed_out");
    assert_eq!(status["signed_in"], false);

    let status = a.signup(&server.url, "ann").await;
    assert_eq!(status["signed_in"], true);
    assert_eq!(status["state"], "idle");
    assert_eq!(status["username"], "ann");
    assert!(status["last_sync_at"].is_string());

    let host = host_json("Web");
    let id = host["id"].as_str().unwrap().to_owned();
    a.save(&host).await;
    let status = a.sync().await;
    assert_eq!(status["pending"], 0);
    assert_eq!(status["last"]["pushed"], 1);

    // The second device signs in to the existing account and receives it.
    let signed = b.signin(&server.url, "ann", "ann-laptop").await;
    assert_eq!(
        signed["fingerprint"], status["fingerprint"],
        "same account, same safety number"
    );
    assert_ne!(signed["device_id"], status["device_id"]);
    let got = b.host(&id).await.expect("the host arrived");
    assert_eq!(got["name"], "Web");

    // And back: an edit on B appears on A.
    b.edit(&id, |h| h["name"] = json!("Web 2")).await;
    b.sync().await;
    a.sync().await;
    assert_eq!(a.host(&id).await.unwrap()["name"], "Web 2");

    // A delete travels too.
    a.ok("hosts.delete", json!({"id": id})).await;
    a.sync().await;
    b.sync().await;
    assert!(b.host(&id).await.is_none());

    // The sync state was announced to clients.
    let states: Vec<String> = a
        .events
        .sync
        .lock()
        .unwrap()
        .iter()
        .map(|s| s["state"].as_str().unwrap().to_owned())
        .collect();
    assert!(states.contains(&"syncing".to_owned()));
    assert!(states.contains(&"idle".to_owned()));
    for event in a.events.sync.lock().unwrap().iter() {
        proto::validate("SyncStatus", event).unwrap();
    }

    // Two devices are listed, this one marked.
    let devices = a.ok("sync.devices.list", json!({})).await;
    let devices = devices["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2);
    assert_eq!(
        devices.iter().filter(|d| d["this_device"] == true).count(),
        1
    );
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn concurrent_edits_to_different_fields_both_survive() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let a = new_device(&dir, "a").await;
    let b = new_device(&dir, "b").await;
    a.signup(&server.url, "ann").await;
    let host = host_json("Api");
    let id = host["id"].as_str().unwrap().to_owned();
    a.save(&host).await;
    a.sync().await;
    b.signin(&server.url, "ann", "b").await;

    // Both edit while offline from each other, different fields.
    a.edit(&id, |h| h["color"] = json!("#ff0000")).await;
    b.edit(&id, |h| h["notes"] = json!("deploys on friday"))
        .await;
    a.sync().await;
    let status = b.sync().await;
    assert_eq!(status["last"]["merged"], 1);
    a.sync().await;

    for device in [&a, &b] {
        let got = device.host(&id).await.unwrap();
        assert_eq!(got["color"], "#ff0000");
        assert_eq!(got["notes"], "deploys on friday");
    }
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn a_same_field_conflict_keeps_the_loser_in_history_and_restore_syncs() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let a = new_device(&dir, "a").await;
    let b = new_device(&dir, "b").await;
    a.signup(&server.url, "ann").await;
    let host = host_json("Db");
    let id = host["id"].as_str().unwrap().to_owned();
    a.save(&host).await;
    a.sync().await;
    b.signin(&server.url, "ann", "b").await;

    a.edit(&id, |h| h["name"] = json!("db-from-a")).await;
    b.edit(&id, |h| h["name"] = json!("db-from-b")).await;
    a.sync().await;
    let status = b.sync().await;
    assert!(
        status["last"]["resolved"].as_u64().unwrap() >= 1,
        "{status}"
    );
    a.sync().await;
    let (name_a, name_b) = (
        a.host(&id).await.unwrap()["name"].clone(),
        b.host(&id).await.unwrap()["name"].clone(),
    );
    assert_eq!(name_a, name_b, "both devices converge on one winner");

    // The value that lost is in history on the device it lost on, and can be
    // restored as an ordinary edit.
    let loser = if name_a == "db-from-a" {
        ("db-from-b", &b)
    } else {
        ("db-from-a", &a)
    };
    let (loser_name, loser_device) = loser;
    let entries = loser_device.ok("hosts.history", json!({"id": id})).await;
    let entry = entries["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["field"] == "name" && e["value"] == loser_name)
        .unwrap_or_else(|| panic!("{loser_name} missing from history: {entries}"))
        .clone();
    loser_device
        .ok(
            "hosts.history_restore",
            json!({"id": id, "seq": entry["seq"]}),
        )
        .await;
    assert_eq!(loser_device.host(&id).await.unwrap()["name"], loser_name);
    loser_device.sync().await;
    let other = if loser_name == "db-from-a" { &b } else { &a };
    other.sync().await;
    assert_eq!(other.host(&id).await.unwrap()["name"], loser_name);
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn a_revoked_device_can_no_longer_push() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let a = new_device(&dir, "a").await;
    let b = new_device(&dir, "b").await;
    a.signup(&server.url, "ann").await;
    let host = host_json("Keep");
    let id = host["id"].as_str().unwrap().to_owned();
    a.save(&host).await;
    a.sync().await;
    let status_b = b.signin(&server.url, "ann", "stolen-laptop").await;

    // A cannot revoke itself, but can revoke B.
    let own = a.sync().await["device_id"].as_str().unwrap().to_owned();
    assert_eq!(
        a.call("sync.devices.revoke", json!({"device_id": own}))
            .await
            .unwrap_err(),
        "invalid_command"
    );
    a.ok(
        "sync.devices.revoke",
        json!({"device_id": status_b["device_id"]}),
    )
    .await;
    let devices = a.ok("sync.devices.list", json!({})).await;
    assert!(devices["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["revoked"] == true));

    // B edits and tries to sync: refused, and A never sees the edit.
    b.edit(&id, |h| h["name"] = json!("tampered")).await;
    let status = b.sync().await;
    assert_eq!(status["state"], "error", "{status}");
    let code = status["last_error_code"].as_str().unwrap();
    assert!(
        matches!(code, "unauthorized" | "device_revoked" | "forbidden"),
        "{code}"
    );
    a.sync().await;
    assert_eq!(a.host(&id).await.unwrap()["name"], "Keep");
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn a_shared_vault_reaches_a_member_and_the_fingerprints_match() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let a = new_device(&dir, "alice").await;
    let b = new_device(&dir, "bob").await;
    let alice = a.signup(&server.url, "alice").await;
    let bob = b.signup(&server.url, "bob").await;

    let team = a
        .ok("vault.create_shared", json!({"name": "Platform"}))
        .await;
    let vault_id = team["id"].as_str().unwrap().to_owned();
    assert_eq!(team["role"], "owner");

    // Alice looks Bob up: the number she sees is the number Bob sees for himself.
    let lookup = a
        .ok("vault.members.lookup", json!({"username": "bob"}))
        .await;
    assert_eq!(lookup["fingerprint"], bob["fingerprint"]);
    let groups: Vec<&str> = lookup["fingerprint"].as_str().unwrap().split(' ').collect();
    assert_eq!(groups.len(), 12);
    assert!(groups.iter().all(|g| g.len() == 5));
    // ... and stable across lookups.
    assert_eq!(
        a.ok("vault.members.lookup", json!({"username": "bob"}))
            .await["fingerprint"],
        lookup["fingerprint"]
    );
    assert_ne!(alice["fingerprint"], bob["fingerprint"]);

    // A number that does not match is refused.
    let wrong = a
        .call(
            "vault.members.add",
            json!({"vault_id": vault_id, "username": "bob", "role": "editor",
                   "fingerprint": "00000 00000 00000 00000 00000 00000 00000 00000 00000 00000 00000 00000"}),
        )
        .await;
    assert_eq!(wrong.unwrap_err(), "fingerprint_mismatch");
    let added = a
        .ok(
            "vault.members.add",
            json!({"vault_id": vault_id, "username": "bob", "role": "editor",
                   "fingerprint": lookup["fingerprint"]}),
        )
        .await;
    assert_eq!(added["trust"], "confirmed");

    // A host moved into the shared vault reaches Bob.
    let host = host_json("Shared");
    let old_id = host["id"].as_str().unwrap().to_owned();
    a.save(&host).await;
    let moved = a
        .ok(
            "hosts.move_vault",
            json!({"id": old_id, "vault_id": vault_id}),
        )
        .await;
    let new_id = moved["id"].as_str().unwrap().to_owned();
    assert_ne!(new_id, old_id);
    assert!(a.host(&old_id).await.is_none());
    a.sync().await;
    b.sync().await;
    let got = b.host(&new_id).await.expect("Bob sees the shared host");
    assert_eq!(got["name"], "Shared");
    let vaults = b.ok("vaults.list", json!({})).await;
    let on_b = vaults["vaults"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == vault_id.as_str())
        .unwrap()
        .clone();
    assert_eq!(on_b["name"], "Platform");
    assert_eq!(on_b["role"], "editor");

    // Bob's side of the member list shows Alice with her number.
    let members = b
        .ok("vault.members.list", json!({"vault_id": vault_id}))
        .await;
    let members = members["members"].as_array().unwrap();
    let alice_row = members.iter().find(|m| m["username"] == "alice").unwrap();
    assert_eq!(alice_row["fingerprint"], alice["fingerprint"]);
    assert_eq!(alice_row["role"], "owner");
    let me = members.iter().find(|m| m["username"] == "bob").unwrap();
    assert_eq!(me["trust"], "self");

    // An editor's change flows back to the owner.
    b.edit(&new_id, |h| h["name"] = json!("Shared by Bob"))
        .await;
    b.sync().await;
    a.sync().await;
    assert_eq!(a.host(&new_id).await.unwrap()["name"], "Shared by Bob");

    // A viewer keeps pulling but pushes nothing.
    let bob_id = lookup["account_id"].clone();
    a.ok(
        "vault.members.role",
        json!({"vault_id": vault_id, "account_id": bob_id, "role": "viewer"}),
    )
    .await;
    b.sync().await;
    b.edit(&new_id, |h| h["name"] = json!("Viewer edit")).await;
    b.sync().await;
    a.sync().await;
    assert_eq!(a.host(&new_id).await.unwrap()["name"], "Shared by Bob");

    // Removing the member ends their access to new records.
    a.ok(
        "vault.members.remove",
        json!({"vault_id": vault_id, "account_id": bob_id}),
    )
    .await;
    a.edit(&new_id, |h| h["name"] = json!("After removal"))
        .await;
    a.sync().await;
    // The server no longer lists the vault for Bob: nothing more arrives.
    let status = b.sync().await;
    assert_eq!(status["state"], "idle", "{status}");
    assert_eq!(
        b.host(&new_id).await.unwrap()["name"],
        "Viewer edit",
        "Bob's copy is no longer updated"
    );
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn sign_in_errors_and_url_rules() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let a = new_device(&dir, "a").await;

    // https only, except loopback.
    let bad = a
        .call(
            "sync.signup",
            json!({"server_url": "http://sync.example.org", "username": "ann", "passphrase": PASS}),
        )
        .await;
    assert_eq!(bad.unwrap_err(), "invalid_url");
    // A weak passphrase is refused before anything is sent.
    let weak = a
        .call(
            "sync.signup",
            json!({"server_url": server.url, "username": "ann", "passphrase": "short"}),
        )
        .await;
    assert_eq!(weak.unwrap_err(), "invalid_passphrase");
    // Not signed in: nothing to do.
    assert_eq!(
        a.call("sync.devices.list", json!({})).await.unwrap_err(),
        "not_signed_in"
    );

    a.signup(&server.url, "ann").await;
    // The name is taken.
    let b = new_device(&dir, "b").await;
    let again = b
        .call(
            "sync.signup",
            json!({"server_url": server.url, "username": "ann", "passphrase": PASS}),
        )
        .await;
    assert_eq!(again.unwrap_err(), "conflict");
    // Wrong passphrase fails on the device.
    let wrong = b
        .call(
            "sync.signin",
            json!({"server_url": server.url, "username": "ann", "passphrase": "not the passphrase"}),
        )
        .await;
    assert_eq!(wrong.unwrap_err(), "wrong_passphrase");
    // Unknown accounts look the same.
    let unknown = b
        .call(
            "sync.signin",
            json!({"server_url": server.url, "username": "nobody", "passphrase": PASS}),
        )
        .await;
    assert_eq!(unknown.unwrap_err(), "wrong_passphrase");

    // Signing out forgets the account, keeps the hosts.
    let host = host_json("Local");
    a.save(&host).await;
    a.sync().await;
    let out = a.ok("sync.signout", json!({})).await;
    assert_eq!(out["signed_in"], false);
    assert_eq!(out["state"], "signed_out");
    assert!(a.host(host["id"].as_str().unwrap()).await.is_some());
    assert_eq!(
        a.call("sync.now", json!({})).await.unwrap()["state"],
        "signed_out"
    );
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn the_account_survives_a_restart_and_the_database_holds_no_plaintext() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("a");
    std::fs::create_dir_all(&root).unwrap();
    let keys = Arc::new(MemoryKeyStore::new());
    let a = start(&root, keys.clone()).await;
    a.signup(&server.url, "ann").await;
    let host = host_json("Persist");
    a.save(&host).await;
    a.sync().await;
    a.stop().await;

    // Neither the passphrase nor any key or token is readable in the files.
    let mut all = Vec::new();
    for entry in std::fs::read_dir(&root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            all.extend(std::fs::read(path).unwrap());
        }
    }
    let contains = |needle: &str| all.windows(needle.len()).any(|w| w == needle.as_bytes());
    assert!(!contains(PASS), "the passphrase is never stored");
    assert!(!contains("device_seed") && !contains("\"token\""));

    // A new daemon over the same device key is still signed in and syncs on
    // its own at start.
    // Another runtime directory: on Windows the first daemon's pipe can linger
    // for a moment after it stopped.
    let a = start_with(&root, "run2", keys, fast()).await;
    let status = a.call("sync.status", json!({})).await.unwrap();
    assert_eq!(status["signed_in"], true);
    assert_eq!(status["username"], "ann");
    eventually("the sync at start", || async {
        a.call("sync.status", json!({})).await.unwrap()["last_sync_at"].is_string()
    })
    .await;
    // Rollback protection and the cursor are persisted: a second run pulls nothing.
    let status = a.sync().await;
    assert_eq!(status["state"], "idle", "{status}");
    assert_eq!(status["last"]["pulled"], 0);
    a.stop().await;
}

#[tokio::test]
async fn a_local_change_is_pushed_in_the_background() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let mut devices = Vec::new();
    for name in ["a", "b"] {
        let root = dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        devices.push(start_with(&root, "run", Arc::new(MemoryKeyStore::new()), background()).await);
    }
    let b = devices.pop().unwrap();
    let a = devices.pop().unwrap();
    a.signup(&server.url, "ann").await;
    b.signin(&server.url, "ann", "b").await;

    let host = host_json("Auto");
    let id = host["id"].as_str().unwrap().to_owned();
    a.save(&host).await;
    // No sync.now on A: the debounced background sync pushes it.
    eventually("the background push", || async {
        b.sync().await;
        b.host(&id).await.is_some()
    })
    .await;
    assert!(*a.events.hosts_changed.lock().unwrap() >= 1);
    a.stop().await;
    b.stop().await;
}

#[tokio::test]
async fn secrets_are_never_synced() {
    let server = start_server().await;
    let dir = tempfile::tempdir().unwrap();
    let a = new_device(&dir, "a").await;
    let b = new_device(&dir, "b").await;
    a.signup(&server.url, "ann").await;
    let host = host_json("Secret");
    let id = host["id"].as_str().unwrap().to_owned();
    a.save(&host).await;
    a.ok(
        "secrets.set",
        json!({"host_id": id, "password": "pw-SECRET-123"}),
    )
    .await;
    a.sync().await;
    b.signin(&server.url, "ann", "b").await;
    assert!(b.host(&id).await.is_some());
    let status = b.ok("secrets.status", json!({"host_id": id})).await;
    assert_eq!(status["password"], false);
    let status = a.ok("secrets.status", json!({"host_id": id})).await;
    assert_eq!(status["password"], true);
    a.stop().await;
    b.stop().await;
}
