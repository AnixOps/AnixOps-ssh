//! `hosts.import_preview` and `hosts.import_apply` through a real daemon:
//! replies follow the schema, applying announces `hosts.changed` to every
//! client (and only when something changed), and files are bounded.

use base64::Engine;
use easyssh_core::ConfigStore;
use easyssh_proto as proto;
use easysshd::link::{AskpassPrompt, Link, LinkHandler};
use easysshd::{Daemon, DaemonConfig, Server};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const CSV: &[u8] = include_bytes!("../../easyssh-import/tests/fixtures/hosts.csv");
const OPENSSH_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../easyssh-import/tests/fixtures/openssh"
);

#[derive(Default)]
struct Counter(AtomicU32);

impl LinkHandler for Counter {
    fn askpass_prompt(&self, _prompt: AskpassPrompt) {}
    fn hosts_changed(&self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

struct Running {
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    daemon: Arc<Daemon>,
    first: Link,
    first_changes: Arc<Counter>,
    second: Link,
    second_changes: Arc<Counter>,
}

async fn start(root: &Path) -> Running {
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(root.join("connections.json")),
        local_shell: Some("/bin/sh".into()),
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let daemon = Daemon::new(cfg).await.unwrap();
    let runtime = root.join("run");
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
    let first_changes = Arc::new(Counter::default());
    let second_changes = Arc::new(Counter::default());
    Running {
        stop,
        task,
        daemon,
        first: Link::connect(&runtime, first_changes.clone())
            .await
            .unwrap(),
        first_changes,
        second: Link::connect(&runtime, second_changes.clone())
            .await
            .unwrap(),
        second_changes,
    }
}

impl Running {
    async fn call(&self, kind: &str, payload: Value) -> Result<Value, String> {
        let command =
            json!({"id": uuid::Uuid::new_v4().to_string(), "kind": kind, "payload": payload});
        proto::validate_command(&command).map_err(|e| e.join("; "))?;
        self.first
            .request(kind, payload)
            .await
            .map_err(|e| format!("{}: {}", e.code, e.message))
    }

    async fn stop(self) {
        drop((self.first, self.second));
        let _ = self.stop.send(());
        self.task.await.unwrap();
        self.daemon.shutdown();
    }
}

async fn eventually(what: &str, mut ready: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ready() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {what}");
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn host_count(list: &Value) -> usize {
    list["connections"].as_array().unwrap().len()
}

#[tokio::test]
async fn preview_then_apply_announces_the_change_to_every_client() {
    let dir = TempDir::new().unwrap();
    let daemon = start(dir.path()).await;

    let preview = daemon
        .call(
            "hosts.import_preview",
            json!({"data": b64(CSV), "file_name": "hosts.csv"}),
        )
        .await
        .unwrap();
    proto::validate("HostsImportPreviewResult", &preview).unwrap();
    assert_eq!(preview["format"], "csv");
    let hosts = preview["hosts"].as_array().unwrap().clone();
    assert!(hosts.iter().any(|h| h["label"] == "web1"));
    assert!(hosts.iter().all(|h| h["exists"] == false));
    // A preview changes nothing and tells nobody.
    let list = daemon.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(host_count(&list), 0);
    assert_eq!(daemon.first_changes.0.load(Ordering::SeqCst), 0);

    // The same file by path.
    let path = dir.path().join("hosts.csv");
    std::fs::write(&path, CSV).unwrap();
    let by_path = daemon
        .call(
            "hosts.import_preview",
            json!({"path": path.to_str().unwrap()}),
        )
        .await
        .unwrap();
    assert_eq!(by_path["hosts"], preview["hosts"]);

    let selected: Vec<Value> = hosts
        .iter()
        .filter(|h| h["label"] == "web1" || h["label"] == "bastion")
        .cloned()
        .collect();
    let payload = json!({
        "hosts": selected,
        "group_mode": "source",
        "duplicates": "skip",
    });
    let applied = daemon
        .call("hosts.import_apply", payload.clone())
        .await
        .unwrap();
    proto::validate("HostsImportApplyResult", &applied).unwrap();
    assert_eq!(applied["created"], 2);
    assert_eq!(applied["groups_created"], 2);
    eventually("hosts.changed on both clients", || {
        daemon.first_changes.0.load(Ordering::SeqCst) == 1
            && daemon.second_changes.0.load(Ordering::SeqCst) == 1
    })
    .await;

    let list = daemon.call("hosts.list", json!({})).await.unwrap();
    proto::validate("HostsListResult", &list).unwrap();
    assert_eq!(host_count(&list), 2);

    // The preview now knows them.
    let again = daemon
        .call("hosts.import_preview", json!({"data": b64(CSV)}))
        .await
        .unwrap();
    let exists: Vec<&str> = again["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["exists"] == true)
        .map(|h| h["label"].as_str().unwrap())
        .collect();
    assert_eq!(exists, ["web1", "bastion"]);

    // Applying the same selection again changes nothing, so nobody is told.
    let applied = daemon.call("hosts.import_apply", payload).await.unwrap();
    assert_eq!(
        (applied["created"].as_u64(), applied["unchanged"].as_u64()),
        (Some(0), Some(2))
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(daemon.first_changes.0.load(Ordering::SeqCst), 1);
    assert_eq!(daemon.second_changes.0.load(Ordering::SeqCst), 1);

    // `keep-both` adds copies, `update` adds none and nothing is removed.
    for policy in ["keep-both", "update"] {
        daemon
            .call(
                "hosts.import_apply",
                json!({"hosts": selected, "group_mode": "none", "duplicates": policy}),
            )
            .await
            .unwrap();
    }
    let list = daemon.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(host_count(&list), 4);
    daemon.stop().await;
}

#[tokio::test]
async fn include_resolves_in_the_base_dir_only_when_one_is_given() {
    let dir = TempDir::new().unwrap();
    let daemon = start(dir.path()).await;
    let config = Path::new(OPENSSH_DIR).join("config");
    let labels = |preview: &Value| -> Vec<String> {
        preview["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["label"].as_str().unwrap().to_owned())
            .collect()
    };
    let without = daemon
        .call(
            "hosts.import_preview",
            json!({"path": config.to_str().unwrap(), "file_name": "config"}),
        )
        .await
        .unwrap();
    assert_eq!(without["format"], "openssh");
    assert!(!labels(&without).contains(&"included".to_owned()));
    let with = daemon
        .call(
            "hosts.import_preview",
            json!({
                "path": config.to_str().unwrap(),
                "file_name": "config",
                "base_dir": OPENSSH_DIR,
            }),
        )
        .await
        .unwrap();
    proto::validate("HostsImportPreviewResult", &with).unwrap();
    assert!(labels(&with).contains(&"included".to_owned()));
    let legacy = with["hosts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["label"] == "legacy")
        .unwrap();
    assert!(!legacy["openssh_only"].as_array().unwrap().is_empty());
    daemon.stop().await;
}

#[tokio::test]
async fn bad_requests_get_a_clear_error_and_change_nothing() {
    let dir = TempDir::new().unwrap();
    let daemon = start(dir.path()).await;

    // More than 8 MiB by path is refused before it is read. (Inline data is
    // bounded by the 4 MiB frame, which the transport enforces.)
    let big = vec![b'a'; easyssh_store::import::MAX_IMPORT_BYTES + 1];
    let path = dir.path().join("big.csv");
    std::fs::write(&path, &big).unwrap();
    let error = daemon
        .call(
            "hosts.import_preview",
            json!({"path": path.to_str().unwrap()}),
        )
        .await
        .unwrap_err();
    assert!(
        error.starts_with("invalid_import:") && error.contains("over the limit of 8 MiB"),
        "{error}"
    );

    // Neither or both sources, and bad base64.
    for payload in [
        json!({}),
        json!({"path": "a", "data": "b"}),
        json!({"data": "***"}),
    ] {
        let error = daemon
            .call("hosts.import_preview", payload)
            .await
            .unwrap_err();
        assert!(error.starts_with("invalid_import:"), "{error}");
    }
    assert!(daemon
        .call(
            "hosts.import_preview",
            json!({"path": "/nonexistent/hosts"})
        )
        .await
        .unwrap_err()
        .starts_with("invalid_import:"));

    // An unknown format is refused by the schema; a missing target group by the daemon.
    assert!(daemon
        .call(
            "hosts.import_preview",
            json!({"data": "", "format": "nope"})
        )
        .await
        .is_err());
    let host = json!({"label": "h", "hostname": "h.example.com", "port": 22});
    let error = daemon
        .call(
            "hosts.import_apply",
            json!({"hosts": [host], "group_mode": "existing", "duplicates": "skip"}),
        )
        .await
        .unwrap_err();
    assert!(
        error.starts_with("invalid_import:") && error.contains("target group"),
        "{error}"
    );
    let list = daemon.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(host_count(&list), 0);
    assert_eq!(daemon.first_changes.0.load(Ordering::SeqCst), 0);
    daemon.stop().await;
}
