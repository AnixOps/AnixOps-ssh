//! The built-in engine inside the daemon: one shared connection per host,
//! prompts forwarded to the client, and the fallback to OpenSSH.
#![cfg(unix)]

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use easyssh_core::{AppConfig, ConfigStore, Connection, SshEngine};
use easyssh_e2e::russh_support::{echo_roundtrip, wait_for_port_open, within, within_for};
use easyssh_e2e::{E2e, ECHO_PORT, PW_PASSWORD, TARGET_PORT};
use easysshd::link::{
    AskpassPrompt, HostKeyPrompt, Link, LinkError, LinkHandler, SessionMsg, SessionSink,
};
use easysshd::{Daemon, DaemonConfig, Server};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpStream;

/// An ed25519 public key that no container has.
const OTHER_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIKxXnqrgYJGHpjbb27Ddo024CtOCBG5K2xV1JzRg3eRu";
const TUNNEL_PORT: u16 = 17301;

/// Answers prompts the way a user would and remembers what was asked.
struct Client {
    link: Mutex<Option<Link>>,
    host_key_answer: Mutex<String>,
    password: Mutex<Option<String>>,
    host_keys: Mutex<Vec<HostKeyPrompt>>,
    asks: Mutex<Vec<AskpassPrompt>>,
    /// `(kind, transfer id, status)` of `transfer.started` and `transfer.done`.
    transfers: Mutex<Vec<(String, String, String)>>,
}

impl Client {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            link: Mutex::new(None),
            host_key_answer: Mutex::new("accept_and_save".into()),
            password: Mutex::new(None),
            host_keys: Mutex::new(Vec::new()),
            asks: Mutex::new(Vec::new()),
            transfers: Mutex::new(Vec::new()),
        })
    }

    fn link(&self) -> Link {
        self.link.lock().unwrap().clone().expect("link attached")
    }

    fn answer_host_keys(&self, decision: &str) {
        *self.host_key_answer.lock().unwrap() = decision.to_owned();
    }
}

impl LinkHandler for Client {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        let id = prompt.id;
        self.asks.lock().unwrap().push(prompt);
        let answer = self.password.lock().unwrap().clone();
        let _ = self.link().askpass_reply(id, answer);
    }

    fn transfer_started(&self, transfer_id: &str) {
        self.transfers
            .lock()
            .unwrap()
            .push(("started".into(), transfer_id.into(), String::new()));
    }

    fn transfer_done(&self, transfer_id: &str, status: &str, error: Option<&str>) {
        self.transfers.lock().unwrap().push((
            "done".into(),
            transfer_id.into(),
            format!("{status} {}", error.unwrap_or_default()),
        ));
    }

    fn hostkey_prompt(&self, prompt: HostKeyPrompt) {
        let id = prompt.id;
        self.host_keys.lock().unwrap().push(prompt);
        let decision = self.host_key_answer.lock().unwrap().clone();
        let _ = self.link().hostkey_reply(id, &decision);
    }
}

struct Rig {
    e2e: E2e,
    client: Arc<Client>,
    link: Link,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    serving: Option<tokio::task::JoinHandle<()>>,
}

impl Rig {
    /// A daemon with `ssh_engine = russh` and the given hosts.
    async fn start(aliases: &[(&str, &str)], linger: Option<Duration>) -> (Rig, Vec<Connection>) {
        let e2e = E2e::new();
        let store = ConfigStore::at(e2e.dir.path().join("daemon").join("connections.json"));
        let mut config = AppConfig::new();
        config.ssh_engine = Some(SshEngine::Russh);
        let hosts: Vec<Connection> = aliases
            .iter()
            .map(|(name, alias)| Connection::alias(*name, *alias))
            .collect();
        config.connections = hosts.clone();
        store.save(&config).expect("write config");

        let daemon = Daemon::new(DaemonConfig {
            config_store: store,
            local_shell: None,
            scrollback_bytes: 1 << 20,
            idle_timeout: Duration::from_secs(600),
        })
        .await
        .expect("daemon");
        daemon.set_known_hosts(e2e.dir.path().join("daemon").join("known_hosts"), None);
        if let Some(linger) = linger {
            daemon.set_connection_linger(linger);
        }
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
        let client = Client::new();
        let link = Link::connect(&runtime, client.clone())
            .await
            .expect("connect");
        *client.link.lock().unwrap() = Some(link.clone());
        (
            Rig {
                e2e,
                client,
                link,
                stop: Some(stop),
                serving: Some(serving),
            },
            hosts,
        )
    }

    fn config_path(&self) -> std::path::PathBuf {
        self.e2e.dir.path().join("daemon").join("connections.json")
    }

    /// The host keys the daemon keeps in the app database (they sync with
    /// the vault), as `known_hosts` lines. Read through a second connection
    /// to the same database, as the app does.
    fn stored_host_keys(&self) -> Vec<String> {
        let opened = easyssh_store::open_app_store(&self.config_path()).expect("open the database");
        opened
            .store
            .list_known_hosts()
            .expect("list known hosts")
            .iter()
            .map(|record| {
                format!(
                    "{} {} {}",
                    record.item.host,
                    record.item.key_type,
                    B64.encode(&record.item.key_blob)
                )
            })
            .collect()
    }

    /// Replaces whatever is stored for the target with `key`, the way a
    /// server that was reinstalled would look to the daemon.
    fn store_other_host_key(&self, key: &str) {
        let opened = easyssh_store::open_app_store(&self.config_path()).expect("open the database");
        for record in opened.store.list_known_hosts().expect("list known hosts") {
            opened
                .store
                .soft_delete_known_host(&record.item.id)
                .expect("remove the entry");
        }
        let report = opened
            .store
            .import_known_hosts(&format!("[127.0.0.1]:{TARGET_PORT} ssh-ed25519 {key}\n"))
            .expect("store the other key");
        assert_eq!(report.imported, 1);
    }

    async fn connections(&self) -> Value {
        self.link
            .request("engine.connections", json!({}))
            .await
            .expect("engine.connections")
    }

    async fn wait_live(&self, live: u64) {
        within_for("connections to settle", Duration::from_secs(20), async {
            while self.connections().await["live"].as_u64() != Some(live) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await;
    }

    async fn finish(mut self) {
        // Break the link <-> handler cycle so everything can go away.
        *self.client.link.lock().unwrap() = None;
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(serving) = self.serving.take() {
            let _ = serving.await;
        }
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
    within_for(what, Duration::from_secs(60), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
}

/// Opens a shell on `host`, runs `command` and waits for `expect`.
async fn shell_says(rig: &Rig, host: &Connection, command: &str, expect: &str) -> String {
    let (seen, sink) = collector();
    let opened = within(
        "session.open",
        rig.link.open_session_info(Some(&host.id), 100, 30, sink),
    )
    .await
    .expect("session.open");
    rig.link
        .write_session(&opened.session_id, &format!("{command}\r"))
        .unwrap();
    eventually(expect, || text(&seen).contains(expect)).await;
    opened.session_id
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn terminal_files_and_tunnel_share_one_connection() {
    let (rig, hosts) = Rig::start(&[("target", "e2e-target")], None).await;
    let host = &hosts[0];

    // Terminal: the daemon says which engine runs it.
    let (seen, sink) = collector();
    let opened = within(
        "session.open",
        rig.link.open_session_info(Some(&host.id), 100, 30, sink),
    )
    .await
    .expect("session.open");
    assert_eq!(opened.engine.as_deref(), Some("russh"));
    assert!(opened.engine_reason.is_some());
    rig.link
        .write_session(&opened.session_id, "echo shared-$((20+22))\r")
        .unwrap();
    eventually("shell output", || text(&seen).contains("shared-42")).await;
    let listed = rig.link.list_sessions().await.expect("session.list");
    assert_eq!(serde_json::to_value(&listed[0]).unwrap()["engine"], "russh");

    // Files over the same connection.
    let sftp = within(
        "sftp.open",
        rig.link.request("sftp.open", json!({"host_id": host.id})),
    )
    .await
    .expect("sftp.open");
    let sftp_id = sftp["sftp_id"].as_str().unwrap().to_owned();
    let home = sftp["home"].as_str().unwrap().to_owned();
    let name = easyssh_e2e::unique_name("daemon-dir");
    let dir = format!("{home}/{name}");
    rig.link
        .request("sftp.mkdir", json!({"sftp_id": sftp_id, "path": dir}))
        .await
        .expect("sftp.mkdir");
    let listing = rig
        .link
        .request("sftp.list", json!({"sftp_id": sftp_id, "path": home}))
        .await
        .expect("sftp.list");
    assert!(
        listing["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["name"] == name.as_str()),
        "{listing}"
    );
    rig.link
        .request(
            "sftp.remove",
            json!({"sftp_id": sftp_id, "path": dir, "directory": true}),
        )
        .await
        .expect("sftp.remove");

    // A -L tunnel to the echo service.
    let tunnel = json!({
        "id": "t-shared", "name": "echo", "host_id": host.id, "kind": "local",
        "bind_address": "127.0.0.1", "bind_port": TUNNEL_PORT,
        "target_host": "127.0.0.1", "target_port": ECHO_PORT,
    });
    within(
        "tunnel.start",
        rig.link.request("tunnel.start", json!({"tunnel": tunnel})),
    )
    .await
    .expect("tunnel.start");
    wait_for_port_open(TUNNEL_PORT, Duration::from_secs(10)).await;
    let mut stream = TcpStream::connect(("127.0.0.1", TUNNEL_PORT))
        .await
        .expect("connect to tunnel");
    echo_roundtrip(&mut stream, "through-the-daemon").await;

    // One SSH connection, three users.
    let info = rig.connections().await;
    assert_eq!(info["opened"], 1, "{info}");
    assert_eq!(info["live"], 1, "{info}");
    assert_eq!(info["connections"][0]["leases"], 3, "{info}");
    assert_eq!(rig.client.host_keys.lock().unwrap().len(), 1);

    rig.link
        .request("tunnel.stop", json!({"id": "t-shared"}))
        .await
        .expect("tunnel.stop");
    rig.link
        .request("sftp.close", json!({"sftp_id": sftp_id}))
        .await
        .expect("sftp.close");
    rig.link.close_session(&opened.session_id);
    rig.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn monitor_reads_the_server_on_the_terminal_connection() {
    let (rig, hosts) = Rig::start(&[("target", "e2e-target")], None).await;
    let host = &hosts[0];

    // No terminal yet: the monitor never connects on its own.
    let error = rig
        .link
        .request("monitor.sample", json!({"host_id": host.id}))
        .await
        .expect_err("no connection to read through");
    assert_eq!(error.code, "not_connected", "{error}");
    assert_eq!(rig.connections().await["opened"], 0);

    let id = shell_says(
        &rig,
        host,
        "sleep 4242 & echo started-$((1+1))",
        "started-2",
    )
    .await;
    let sample = within(
        "monitor.sample",
        rig.link.request(
            "monitor.sample",
            json!({"host_id": host.id, "detail": true}),
        ),
    )
    .await
    .expect("monitor.sample");
    let output = sample["output"].as_str().unwrap();
    for section in [
        "@os Linux",
        "@uptime",
        "@loadavg",
        "@stat",
        "@meminfo",
        "@netdev",
        "@df",
        "@pstat",
        "@ps",
        "@exe",
    ] {
        assert!(output.contains(section), "{section} missing in {output}");
    }
    assert!(output.trim_end().ends_with("@end"), "{output}");
    assert!(sample["latency_ms"].as_f64().is_some(), "{sample}");

    // The process list can end a process the login user owns.
    let pid: u64 = output
        .lines()
        .find(|line| line.trim_end().ends_with("sleep 4242"))
        .and_then(|line| line.split_whitespace().next())
        .and_then(|pid| pid.parse().ok())
        .unwrap_or_else(|| panic!("sleep 4242 not listed in {output}"));
    rig.link
        .request(
            "monitor.signal",
            json!({"host_id": host.id, "pid": pid, "signal": "TERM"}),
        )
        .await
        .expect("monitor.signal");
    let mut gone = false;
    for _ in 0..50 {
        if let Err(error) = rig
            .link
            .request(
                "monitor.signal",
                json!({"host_id": host.id, "pid": pid, "signal": "TERM"}),
            )
            .await
        {
            assert_eq!(error.code, "signal_failed", "{error}");
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gone, "the process outlived TERM");

    // Still the one connection the terminal opened.
    let info = rig.connections().await;
    assert_eq!(info["opened"], 1, "{info}");
    rig.link.close_session(&id);
    rig.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn jump_host_runs_on_the_built_in_engine() {
    let (rig, hosts) = Rig::start(&[("via bastion", "e2e-target-via-bastion")], None).await;
    let id = shell_says(&rig, &hosts[0], "echo jumped-$((40+2))", "jumped-42").await;
    let info = rig.connections().await;
    assert_eq!(
        info["connections"][0]["jump"].as_array().unwrap().len(),
        1,
        "{info}"
    );
    rig.link.close_session(&id);
    rig.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn password_is_asked_once_for_the_whole_connection() {
    let (rig, hosts) = Rig::start(&[("pw", "e2e-target-pw")], None).await;
    *rig.client.password.lock().unwrap() = Some(PW_PASSWORD.to_owned());
    let id = shell_says(&rig, &hosts[0], "echo pw-$((6*7))", "pw-42").await;

    // Files reuse the connection, so nobody is asked again.
    rig.link
        .request("sftp.open", json!({"host_id": hosts[0].id}))
        .await
        .expect("sftp.open");
    let asks = rig.client.asks.lock().unwrap().clone();
    assert_eq!(asks.len(), 1, "{asks:?}");
    assert_eq!(asks[0].kind.as_deref(), Some("password"));
    assert_eq!(rig.connections().await["opened"], 1);
    rig.link.close_session(&id);
    rig.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn unknown_host_key_is_asked_once_then_remembered() {
    let (rig, hosts) = Rig::start(
        &[("target", "e2e-target")],
        Some(Duration::from_millis(100)),
    )
    .await;
    let id = shell_says(&rig, &hosts[0], "echo first-$((1+1))", "first-2").await;
    {
        let prompts = rig.client.host_keys.lock().unwrap();
        assert_eq!(prompts.len(), 1, "{prompts:?}");
        assert_eq!(prompts[0].status, "unknown");
        assert_eq!(prompts[0].host, "127.0.0.1");
        assert_eq!(prompts[0].port, TARGET_PORT);
        assert!(prompts[0].fingerprint.starts_with("SHA256:"));
    }
    let stored = rig.stored_host_keys().join("\n");
    assert!(
        stored.contains(&format!("[127.0.0.1]:{TARGET_PORT}")),
        "{stored}"
    );
    // The app-owned file of earlier versions is not written any more.
    assert!(!rig
        .e2e
        .dir
        .path()
        .join("daemon")
        .join("known_hosts")
        .exists());

    // The connection goes away, a new one finds the key on record.
    rig.link.close_session(&id);
    rig.wait_live(0).await;
    let id = shell_says(&rig, &hosts[0], "echo second-$((2+2))", "second-4").await;
    assert_eq!(rig.client.host_keys.lock().unwrap().len(), 1);
    assert_eq!(rig.connections().await["opened"], 2);
    rig.link.close_session(&id);
    rig.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn changed_host_key_is_refused_until_explicitly_replaced() {
    let (rig, hosts) = Rig::start(
        &[("target", "e2e-target")],
        Some(Duration::from_millis(100)),
    )
    .await;
    let id = shell_says(&rig, &hosts[0], "echo trusted-$((1+1))", "trusted-2").await;
    rig.link.close_session(&id);
    rig.wait_live(0).await;

    let stored = rig.stored_host_keys();
    assert_eq!(stored.len(), 1, "{stored:?}");
    let fields: Vec<&str> = stored[0].split_whitespace().collect();
    assert_eq!(fields.len(), 3, "{stored:?}");
    assert_ne!(fields[2], OTHER_KEY);
    rig.store_other_host_key(OTHER_KEY);

    // Neither "accept and save" nor "reject" lets a changed key through.
    for decision in ["accept_and_save", "accept_once", "reject"] {
        rig.client.answer_host_keys(decision);
        let (_, sink) = collector();
        let error: LinkError = within(
            "session.open",
            rig.link
                .open_session_info(Some(&hosts[0].id), 100, 30, sink),
        )
        .await
        .expect_err("a changed key must be refused");
        assert_eq!(error.code, "hostkey_changed", "{decision}: {error}");
    }
    {
        let prompts = rig.client.host_keys.lock().unwrap();
        assert!(prompts.iter().skip(1).all(|p| p.status == "changed"));
        assert!(!prompts.last().unwrap().expected.is_empty());
    }
    assert!(rig.stored_host_keys().join("\n").contains(OTHER_KEY));

    // Only an explicit replacement works.
    rig.client.answer_host_keys("replace_and_save");
    let id = shell_says(&rig, &hosts[0], "echo replaced-$((3+4))", "replaced-7").await;
    assert!(!rig.stored_host_keys().join("\n").contains(OTHER_KEY));
    rig.link.close_session(&id);
    rig.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn proxy_command_falls_back_to_openssh() {
    let (rig, hosts) = Rig::start(&[("proxycmd", "e2e-target-proxycmd")], None).await;
    let (seen, sink) = collector();
    let opened = within(
        "session.open",
        rig.link
            .open_session_info(Some(&hosts[0].id), 100, 30, sink),
    )
    .await
    .expect("session.open");
    assert_eq!(opened.engine.as_deref(), Some("openssh"));
    let reason = opened.engine_reason.clone().unwrap_or_default();
    assert!(reason.contains("ProxyCommand"), "{reason}");
    rig.link
        .write_session(&opened.session_id, "echo proxied-$((8+8))\r")
        .unwrap();
    eventually("shell output", || text(&seen).contains("proxied-16")).await;
    // No built-in connection was made.
    assert_eq!(rig.connections().await["opened"], 0);

    let resolved = rig
        .link
        .request("engine.resolve", json!({"host_id": hosts[0].id}))
        .await
        .expect("engine.resolve");
    assert_eq!(resolved["engine"], "openssh");
    if cfg!(unix) {
        // The OpenSSH terminal is the master of a control socket; the monitor
        // runs its script through it without a login of its own.
        let sample = rig
            .link
            .request("monitor.sample", json!({"host_id": hosts[0].id}))
            .await
            .expect("monitor.sample through the OpenSSH master");
        let output = sample["output"].as_str().unwrap_or_default();
        assert!(output.contains("@os"), "{output}");
        assert!(output.contains("@end"), "{output}");
    } else {
        // Windows OpenSSH cannot share a connection.
        let error = rig
            .link
            .request("monitor.sample", json!({"host_id": hosts[0].id}))
            .await
            .expect_err("no monitor for OpenSSH on Windows");
        assert_eq!(error.code, "wrong_engine", "{error}");
    }
    rig.link.close_session(&opened.session_id);
    rig.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn many_enqueued_transfers_share_the_connection_and_wait_their_turn() {
    // More files than sshd's 10 sessions per connection.
    const FILES: usize = 14;
    let (rig, hosts) = Rig::start(&[("target", "e2e-target")], None).await;
    let host = &hosts[0];
    let scratch = tempfile::tempdir().unwrap();
    let sftp = within(
        "sftp.open",
        rig.link.request("sftp.open", json!({"host_id": host.id})),
    )
    .await
    .expect("sftp.open");
    let sftp_id = sftp["sftp_id"].as_str().unwrap().to_owned();
    let home = sftp["home"].as_str().unwrap().to_owned();
    let dir = format!("{home}/{}", easyssh_e2e::unique_name("e2e-enqueue"));
    rig.link
        .request("sftp.mkdir", json!({"sftp_id": sftp_id, "path": dir}))
        .await
        .expect("sftp.mkdir");

    let mut ids = Vec::new();
    let mut queued = 0;
    for index in 0..FILES {
        let local = scratch.path().join(format!("f{index}.bin"));
        std::fs::write(&local, easyssh_e2e::sample_bytes(6 * 1024 * 1024 + index)).unwrap();
        let answer = rig
            .link
            .request(
                "transfer.enqueue",
                json!({
                    "host_id": host.id,
                    "direction": "upload",
                    "local_path": local.to_string_lossy(),
                    "remote_path": format!("{dir}/f{index}.bin"),
                }),
            )
            .await
            .expect("transfer.enqueue");
        queued += usize::from(answer["queued"].as_bool().unwrap());
        ids.push(answer["transfer_id"].as_str().unwrap().to_owned());
    }
    assert!(queued > 0, "the later ones wait");
    eventually("every transfer ends", || {
        rig.client
            .transfers
            .lock()
            .unwrap()
            .iter()
            .filter(|t| t.0 == "done")
            .count()
            == FILES
    })
    .await;
    let seen = rig.client.transfers.lock().unwrap().clone();
    for id in &ids {
        assert!(
            seen.iter()
                .any(|t| t.0 == "done" && &t.1 == id && t.2.starts_with("completed")),
            "{id}: {seen:?}"
        );
    }
    assert_eq!(seen.iter().filter(|t| t.0 == "started").count(), queued);
    let listed = rig
        .link
        .request("sftp.list", json!({"sftp_id": sftp_id, "path": dir}))
        .await
        .expect("sftp.list");
    assert_eq!(listed["entries"].as_array().unwrap().len(), FILES);
    rig.link
        .request(
            "sftp.remove",
            json!({"sftp_id": sftp_id, "path": dir, "directory": true}),
        )
        .await
        .expect("sftp.remove");
    rig.finish().await;
}
