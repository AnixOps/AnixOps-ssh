//! Phase 3: hosts saved through the daemon live in the SQLite store and are
//! still there after the daemon is stopped and started again. Needs no
//! containers, so it runs with the ordinary test suite on every platform.

use easyssh_core::{ConfigStore, Connection};
use easysshd::link::{AskpassPrompt, Link, LinkHandler};
use easysshd::{Daemon, DaemonConfig, Server};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

struct NoPrompts;

impl LinkHandler for NoPrompts {
    fn askpass_prompt(&self, _prompt: AskpassPrompt) {}
}

/// Runs a daemon over `root` until `body` is done, then stops it.
async fn with_daemon<F, Fut>(root: &Path, body: F)
where
    F: FnOnce(Link) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let config = DaemonConfig {
        config_store: ConfigStore::at(root.join("connections.json")),
        local_shell: None,
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let daemon = Daemon::new(config).await.expect("daemon");
    let runtime = root.join("run");
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
    let link = Link::connect(&runtime, Arc::new(NoPrompts))
        .await
        .expect("connect");
    body(link).await;
    let _ = stop.send(());
    serving.await.expect("daemon stopped");
    daemon.shutdown();
}

#[tokio::test]
async fn hosts_saved_through_the_daemon_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let production = Connection::alias("Production", "prod");
    let staging = Connection::alias("Staging", "staging");

    let (p, s) = (production.clone(), staging.clone());
    with_daemon(dir.path(), |link| async move {
        for connection in [&p, &s] {
            link.request(
                "hosts.save",
                json!({"connection": serde_json::to_value(connection).unwrap()}),
            )
            .await
            .expect("hosts.save");
        }
        link.request("hosts.delete", json!({"id": s.id}))
            .await
            .expect("hosts.delete");
    })
    .await;

    let expected = production.id.clone();
    with_daemon(dir.path(), |link| async move {
        let list = link.request("hosts.list", json!({})).await.expect("list");
        let hosts = list["connections"].as_array().unwrap();
        assert_eq!(
            hosts.len(),
            1,
            "the saved host is back, the deleted one is not"
        );
        assert_eq!(hosts[0]["id"], expected.as_str());
        assert_eq!(hosts[0]["name"], "Production");
    })
    .await;
    assert!(dir.path().join("easyssh.db").is_file());
}
