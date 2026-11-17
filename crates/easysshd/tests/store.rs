//! The daemon's hosts and known hosts are backed by the SQLite store: first
//! start migrates the v1 JSON once, saved hosts survive a restart, and another
//! process may use the same database while the daemon runs.

use easyssh_core::{ConfigStore, Connection};
use easyssh_proto as proto;
use easysshd::link::{AskpassPrompt, Link, LinkHandler};
use easysshd::{Daemon, DaemonConfig, Server};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const V1_FULL: &str = include_str!("../../easyssh-store/fixtures/v1_full.json");
const ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

struct NoPrompts;

impl LinkHandler for NoPrompts {
    fn askpass_prompt(&self, _prompt: AskpassPrompt) {}
}

struct Running {
    daemon: Arc<Daemon>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    link: Link,
}

impl Running {
    async fn call(&self, kind: &str, payload: Value) -> Result<Value, String> {
        let command =
            json!({"id": uuid::Uuid::new_v4().to_string(), "kind": kind, "payload": payload});
        proto::validate_command(&command).map_err(|e| e.join("; "))?;
        self.link.request(kind, payload).await.map_err(|e| e.code)
    }

    async fn stop(self) {
        drop(self.link);
        let _ = self.stop.send(());
        self.task.await.unwrap();
        self.daemon.shutdown();
    }
}

fn v1_path(root: &Path) -> PathBuf {
    root.join("connections.json")
}

/// A daemon whose configuration lives under `root`, like `daemon::run` builds it.
async fn start(root: &Path) -> Running {
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(v1_path(root)),
        local_shell: Some("/bin/sh".into()),
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let daemon = Daemon::new(cfg).await.unwrap();
    // A runtime directory per daemon: a Windows named pipe of a daemon that
    // just stopped may not be reusable at once.
    static STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let runtime = root.join(format!(
        "run{}",
        STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
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
    let link = Link::connect(&runtime, Arc::new(NoPrompts)).await.unwrap();
    Running {
        daemon,
        stop,
        task,
        link,
    }
}

fn names(list: &Value) -> Vec<String> {
    let mut names: Vec<String> = list["connections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn saved_hosts_survive_a_daemon_restart() {
    let dir = TempDir::new().unwrap();
    let first = start(dir.path()).await;
    let connection = Connection::alias("Production", "prod");
    first
        .call(
            "hosts.save",
            json!({"connection": serde_json::to_value(&connection).unwrap()}),
        )
        .await
        .unwrap();
    first.stop().await;

    // The database, not the v1 file, holds it.
    assert!(dir.path().join("easyssh.db").is_file());
    assert!(!v1_path(dir.path()).exists());

    let second = start(dir.path()).await;
    let list = second.call("hosts.list", json!({})).await.unwrap();
    proto::validate("HostsListResult", &list).unwrap();
    assert_eq!(names(&list), ["Production"]);
    assert_eq!(list["connections"][0]["id"], connection.id.as_str());
    second.stop().await;
}

#[tokio::test]
async fn first_start_migrates_the_v1_file_once() {
    let dir = TempDir::new().unwrap();
    std::fs::write(v1_path(dir.path()), V1_FULL).unwrap();

    let first = start(dir.path()).await;
    let list = first.call("hosts.list", json!({})).await.unwrap();
    proto::validate("HostsListResult", &list).unwrap();
    // The fixture's third host fails validation and is skipped.
    assert_eq!(names(&list), ["Bastion", "Production DB"]);
    assert_eq!(list["groups"].as_array().unwrap().len(), 3);
    let backup = dir.path().join("connections.v1-backup.json");
    assert!(backup.is_file(), "the v1 file is kept as a backup");
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), V1_FULL);
    assert_eq!(
        std::fs::read_to_string(v1_path(dir.path())).unwrap(),
        V1_FULL
    );

    // Delete one, restart: a second start must not bring it back.
    let id = list["connections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Bastion")
        .unwrap()["id"]
        .clone();
    first.call("hosts.delete", json!({"id": id})).await.unwrap();
    first.stop().await;

    let second = start(dir.path()).await;
    let list = second.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(names(&list), ["Production DB"]);
    second.stop().await;
}

#[tokio::test]
async fn a_group_travels_with_the_host_and_hosts_join_it() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let group_id = uuid::Uuid::new_v4().to_string();
    let mut connection = Connection::alias("Web", "web");
    connection.group_id = Some(group_id.clone());
    s.call(
        "hosts.save",
        json!({
            "connection": serde_json::to_value(&connection).unwrap(),
            "group": {"id": group_id, "name": "Prod", "color": null, "parent_id": null},
        }),
    )
    .await
    .unwrap();
    let list = s.call("hosts.list", json!({})).await.unwrap();
    proto::validate("HostsListResult", &list).unwrap();
    assert_eq!(list["groups"][0]["name"], "Prod");
    assert_eq!(list["connections"][0]["group_id"], group_id.as_str());
    // Saving again (an edit) keeps one record.
    connection.favorite = true;
    s.call(
        "hosts.save",
        json!({"connection": serde_json::to_value(&connection).unwrap()}),
    )
    .await
    .unwrap();
    let list = s.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(list["connections"].as_array().unwrap().len(), 1);
    assert_eq!(list["connections"][0]["favorite"], true);
    s.stop().await;
}

#[tokio::test]
async fn another_process_can_use_the_database_while_the_daemon_runs() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    // The app's in-process fallback opens the same file the same way.
    let fallback = easyssh_store::open_app_store(&v1_path(dir.path())).unwrap();
    assert!(fallback.migration.is_none());
    fallback
        .store
        .save_host(&Connection::alias("From fallback", "fb"), None)
        .unwrap();
    let list = s.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(names(&list), ["From fallback"]);
    // And the other way round.
    s.call(
        "hosts.save",
        json!({"connection": serde_json::to_value(Connection::alias("From daemon", "d")).unwrap()}),
    )
    .await
    .unwrap();
    assert_eq!(fallback.store.list_hosts().unwrap().len(), 2);
    s.stop().await;
}

#[tokio::test]
async fn known_hosts_import_list_remove() {
    let dir = TempDir::new().unwrap();
    // A stand-in for ~/.ssh/known_hosts; the real one is never read.
    let file = dir.path().join("known_hosts");
    std::fs::write(
        &file,
        format!(
            "# comment\nexample.com ssh-ed25519 {ED25519} me@laptop\n\
             [example.com]:2222 ssh-ed25519 {ED25519}\nbroken line\n"
        ),
    )
    .unwrap();
    let s = start(dir.path()).await;

    let empty = s.call("known_hosts.list", json!({})).await.unwrap();
    proto::validate("KnownHostsListResult", &empty).unwrap();
    assert!(empty["entries"].as_array().unwrap().is_empty());

    let report = s
        .call("known_hosts.import", json!({"path": file}))
        .await
        .unwrap();
    proto::validate("KnownHostsImportResult", &report).unwrap();
    assert_eq!(report["imported"], 2);
    assert_eq!(report["duplicates"], 0);
    assert_eq!(report["skipped"][0]["line"], 4);

    let again = s
        .call("known_hosts.import", json!({"path": file}))
        .await
        .unwrap();
    assert_eq!(
        (again["imported"].clone(), again["duplicates"].clone()),
        (json!(0), json!(2))
    );

    let list = s.call("known_hosts.list", json!({})).await.unwrap();
    proto::validate("KnownHostsListResult", &list).unwrap();
    let entries = list["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    let plain = entries.iter().find(|e| e["host"] == "example.com").unwrap();
    assert_eq!(plain["port"], 22);
    assert_eq!(plain["key_type"], "ssh-ed25519");
    assert_eq!(plain["comment"], "me@laptop");
    assert!(plain["fingerprint"]
        .as_str()
        .unwrap()
        .starts_with("SHA256:"));
    assert!(
        plain.get("key_blob").is_none(),
        "the key itself is not sent"
    );

    let id = plain["id"].clone();
    s.call("known_hosts.remove", json!({"id": id}))
        .await
        .unwrap();
    // Removing it again, or an unknown id, is not an error.
    s.call("known_hosts.remove", json!({"id": id}))
        .await
        .unwrap();
    s.call("known_hosts.remove", json!({"id": "nope"}))
        .await
        .unwrap();
    let list = s.call("known_hosts.list", json!({})).await.unwrap();
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);

    // Importing the file again brings the removed entry back.
    let back = s
        .call("known_hosts.import", json!({"path": file}))
        .await
        .unwrap();
    assert_eq!(
        (back["imported"].clone(), back["duplicates"].clone()),
        (json!(1), json!(1))
    );

    // A missing file is an error, not a crash.
    let missing = dir.path().join("none");
    assert_eq!(
        s.call("known_hosts.import", json!({"path": missing}))
            .await
            .unwrap_err(),
        "io_error"
    );
    s.stop().await;
}
