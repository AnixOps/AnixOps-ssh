//! Phase 1 hardening: reconnecting with exact replay, restarts, the build and
//! schema handshake, `hosts.changed`, `askpass.resolved`, `SSH_AUTH_SOCK`
//! validation and the daemon log. Unix only (PTYs and a `/bin/sh`).
#![cfg(unix)]

use easyssh_core::{ConfigStore, Connection};
use easyssh_proto as proto;
use easysshd::agent::{validate_agent_socket, validate_for_uid};
use easysshd::askpass;
use easysshd::link::{AskpassPrompt, ExitReason, Link, LinkHandler, SessionMsg};
use easysshd::supervisor::{Backoff, SupervisedMsg, SupervisedSink, Supervisor, SupervisorConfig};
use easysshd::{Client, Daemon, DaemonConfig, DaemonExtras, Server};
use serde_json::{json, Value};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(20);

// ---- an in-process daemon ------------------------------------------------

struct Running {
    daemon: Arc<Daemon>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Running {
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.await.unwrap();
    }

    async fn exited(&mut self) {
        tokio::time::timeout(WAIT, &mut self.task)
            .await
            .expect("the daemon did not exit")
            .unwrap();
    }
}

/// A daemon like `daemon::run` builds it: it leaves when it drains or idles.
async fn start_daemon(root: &Path, build: &str, log_dir: Option<PathBuf>) -> Running {
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(root.join("connections.json")),
        local_shell: Some("/bin/sh".into()),
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let extras = DaemonExtras {
        build_version: Some(build.to_owned()),
        log_dir,
        ..DaemonExtras::default()
    };
    let daemon = Daemon::new_with(cfg, extras).await.unwrap();
    let server = Server::start_service(root.join("run"), daemon.clone()).unwrap();
    let (tx, rx) = oneshot::channel();
    let idle = daemon.clone();
    let stopping = daemon.clone();
    let task = tokio::spawn(async move {
        server
            .serve(async {
                tokio::select! {
                    _ = rx => {}
                    _ = idle.wait_idle() => {}
                }
            })
            .await
            .unwrap();
        // A real process takes its connections with it when it exits.
        stopping.drop_connections();
        stopping.shutdown();
    });
    Running {
        daemon,
        stop: Some(tx),
        task,
    }
}

// ---- helpers --------------------------------------------------------------

#[derive(Default)]
struct Handler {
    resolved: Mutex<Vec<u64>>,
    prompts: Mutex<Vec<AskpassPrompt>>,
    hosts_changed: Mutex<u32>,
}

impl LinkHandler for Handler {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        self.prompts.lock().unwrap().push(prompt);
    }
    fn askpass_resolved(&self, id: u64) {
        self.resolved.lock().unwrap().push(id);
    }
    fn hosts_changed(&self) {
        *self.hosts_changed.lock().unwrap() += 1;
    }
}

type Seen = Arc<Mutex<Vec<SupervisedMsg>>>;

/// A sink for a plain `Link`.
fn collector() -> (Seen, Box<dyn FnMut(SessionMsg) -> bool + Send>) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    (
        seen,
        Box::new(move |m| {
            sink.lock().unwrap().push(SupervisedMsg::Session(m));
            true
        }),
    )
}

/// A sink for a supervised session.
fn supervised() -> (Seen, SupervisedSink) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    (
        seen,
        Box::new(move |m| {
            sink.lock().unwrap().push(m);
            true
        }),
    )
}

fn exited() -> SupervisedMsg {
    SupervisedMsg::Session(SessionMsg::Exit {
        code: None,
        reason: ExitReason::Disconnected,
    })
}

fn text(seen: &Seen) -> String {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|m| match m {
            SupervisedMsg::Session(SessionMsg::Data { data, .. }) => Some(data.as_str()),
            _ => None,
        })
        .collect()
}

fn has(seen: &Seen, wanted: &SupervisedMsg) -> bool {
    seen.lock().unwrap().contains(wanted)
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A raw protocol client that keeps the events it saw while waiting.
struct Tc {
    client: Client,
    events: Vec<Value>,
}

impl Tc {
    async fn connect(rt: &Path) -> Tc {
        let mut tc = Tc {
            client: Client::connect(rt).await.unwrap(),
            events: Vec::new(),
        };
        tc.call("proto.hello", json!({"schema_version": 1}))
            .await
            .unwrap();
        tc
    }

    async fn recv(&mut self) -> Value {
        let v = tokio::time::timeout(WAIT, self.client.recv())
            .await
            .expect("timed out waiting for the daemon")
            .unwrap();
        if v.get("kind").is_some() {
            proto::validate_event(&v).expect("daemon sent an event that fails the schema");
        } else {
            proto::validate("Reply", &v).expect("daemon sent a reply that fails the schema");
        }
        v
    }

    async fn call(&mut self, kind: &str, payload: Value) -> Result<Value, Value> {
        let id = uuid::Uuid::new_v4().to_string();
        self.client
            .send(json!({"id": id, "kind": kind, "payload": payload}))
            .await
            .unwrap();
        loop {
            let v = self.recv().await;
            if v.get("kind").is_some() {
                self.events.push(v);
            } else {
                assert_eq!(v["id"], id.as_str());
                return match v.get("ok") {
                    Some(ok) => Ok(ok.clone()),
                    None => Err(v["error"].clone()),
                };
            }
        }
    }

    /// The payload of the next event of `kind`, from those already seen or
    /// still to come.
    async fn event(&mut self, kind: &str) -> Value {
        loop {
            if let Some(i) = self.events.iter().position(|e| e["kind"] == kind) {
                return self.events.remove(i)["payload"].clone();
            }
            let v = self.recv().await;
            assert!(v.get("kind").is_some(), "unexpected reply {v}");
            self.events.push(v);
        }
    }

    fn output(&self, session_id: &str) -> String {
        self.events
            .iter()
            .filter(|e| e["kind"] == "session.output" && e["payload"]["session_id"] == session_id)
            .map(|e| e["payload"]["data"].as_str().unwrap())
            .collect()
    }

    async fn until_output(&mut self, session_id: &str, needle: &str) -> String {
        loop {
            let out = self.output(session_id);
            if out.contains(needle) {
                return out;
            }
            let v = self.recv().await;
            assert!(v.get("kind").is_some(), "unexpected reply {v}");
            self.events.push(v);
        }
    }
}

fn no_spawn() -> Arc<dyn Fn() -> std::io::Result<()> + Send + Sync> {
    Arc::new(|| Err(std::io::Error::other("this test must not start a daemon")))
}

fn supervisor_config(
    rt: &Path,
    version: &str,
    spawn: easysshd::supervisor::Spawn,
) -> SupervisorConfig {
    SupervisorConfig {
        runtime_dir: rt.to_path_buf(),
        app_version: version.to_owned(),
        spawn,
        start_wait: Duration::from_secs(10),
        backoff: Backoff {
            first: Duration::from_millis(50),
            max: Duration::from_millis(400),
        },
    }
}

/// Numbers of the `line-N` lines in terminal output.
fn numbered_lines(output: &str) -> Vec<u32> {
    output
        .split("\r\n")
        .filter_map(|line| line.strip_prefix("line-"))
        .filter_map(|n| n.trim().parse().ok())
        .collect()
}

// ---- reconnect and re-attach -----------------------------------------------

#[tokio::test]
async fn reconnect_reattaches_from_the_last_offset_without_loss_or_repeat() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let daemon = start_daemon(root.path(), "1.0.0", None).await;
    let handler = Arc::new(Handler::default());
    let sup = Supervisor::new(supervisor_config(&rt, "1.0.0", no_spawn()), handler);
    sup.start().await.unwrap();

    let (seen, sink) = supervised();
    let id = sup.open_session(None, 80, 24, sink).await.unwrap();
    sup.write_session(
        &id,
        "echo; for i in $(seq 1 300); do echo line-$i; sleep 0.01; done; echo end-$((1+1))\n",
    )
    .unwrap();
    eventually("some output", || numbered_lines(&text(&seen)).len() > 20).await;

    // The connection breaks while the program keeps printing.
    daemon.daemon.drop_connections();
    eventually("reconnecting state", || {
        has(&seen, &SupervisedMsg::Reconnecting)
    })
    .await;
    eventually("resumed state", || has(&seen, &SupervisedMsg::Resumed)).await;
    eventually("the end of the output", || text(&seen).contains("end-2")).await;

    // Every line exactly once, in order, and the stream offsets are
    // contiguous: nothing lost, nothing repeated.
    let output = text(&seen);
    let lines = numbered_lines(&output);
    assert_eq!(lines, (1..=300).collect::<Vec<u32>>());
    let mut next = 0;
    for msg in seen.lock().unwrap().iter() {
        if let SupervisedMsg::Session(SessionMsg::Data { seq, data }) = msg {
            assert_eq!(*seq, next, "output offsets must be contiguous");
            next += data.len() as u64;
        }
    }
    // The session still takes input on the new connection.
    sup.write_session(&id, "echo after-$((2+3))\n").unwrap();
    eventually("input after reconnecting", || {
        text(&seen).contains("after-5")
    })
    .await;
    assert!(!sup.is_reconnecting());
    sup.close_session(&id);
    daemon.stop().await;
}

#[tokio::test]
async fn a_restarted_daemon_ends_the_old_sessions() {
    // A real daemon process, killed the hard way and started again.
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_easysshd"));
    let args: Vec<std::ffi::OsString> = vec![
        "--runtime-dir".into(),
        rt.clone().into(),
        "--config".into(),
        root.path().join("c.json").into(),
        // Leave on its own soon after the test.
        "--idle-timeout-secs".into(),
        "3".into(),
    ];
    let spawn: easysshd::supervisor::Spawn = {
        let args = args.clone();
        Arc::new(move || easysshd::launch::spawn_detached(&exe, &args))
    };
    let handler = Arc::new(Handler::default());
    let mut cfg = supervisor_config(&rt, env!("CARGO_PKG_VERSION"), spawn);
    cfg.start_wait = Duration::from_secs(20);
    let sup = Supervisor::new(cfg, handler);
    sup.start().await.expect("the first daemon starts");

    let (seen, sink) = supervised();
    let id = sup.open_session(None, 80, 24, sink).await.unwrap();
    sup.write_session(&id, "echo alive-$((4+4))\n").unwrap();
    eventually("output", || text(&seen).contains("alive-8")).await;
    let old_pid: i32 = std::fs::read_to_string(rt.join("easysshd.lock"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // SAFETY: plain kill(2) of the daemon process this test started.
    unsafe { libc::kill(old_pid, libc::SIGKILL) };
    eventually("reconnecting state", || {
        has(&seen, &SupervisedMsg::Reconnecting)
    })
    .await;
    // The supervisor starts a new daemon; the session is not in it.
    eventually("the tab sees the session ended", || has(&seen, &exited())).await;
    assert!(!has(&seen, &SupervisedMsg::Resumed));
    eventually("connected again", || sup.link().is_some()).await;
    let new_pid: i32 = std::fs::read_to_string(rt.join("easysshd.lock"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_ne!(old_pid, new_pid);
    assert!(sup.list_sessions().await.unwrap().is_empty());
    // And it works: a new session on the new daemon.
    let (fresh, sink) = supervised();
    let id2 = sup.open_session(None, 80, 24, sink).await.unwrap();
    sup.write_session(&id2, "echo fresh-$((5+5))\n").unwrap();
    eventually("fresh output", || text(&fresh).contains("fresh-10")).await;
    sup.close_session(&id2);
    // SAFETY: as above.
    unsafe { libc::kill(new_pid, libc::SIGTERM) };
}

// ---- build and schema handshake --------------------------------------------

#[tokio::test]
async fn a_daemon_of_another_build_without_sessions_is_replaced() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let mut old = start_daemon(root.path(), "1.0.0", None).await;
    let started: Arc<Mutex<Vec<Running>>> = Arc::new(Mutex::new(Vec::new()));
    let spawn: easysshd::supervisor::Spawn = {
        let started = started.clone();
        let root = root.path().to_path_buf();
        Arc::new(move || {
            let (started, root) = (started.clone(), root.clone());
            tokio::spawn(async move {
                let new = start_daemon(&root, "2.0.0", None).await;
                started.lock().unwrap().push(new);
            });
            Ok(())
        })
    };
    let sup = Supervisor::new(
        supervisor_config(&rt, "2.0.0", spawn),
        Arc::new(Handler::default()),
    );
    sup.start().await.unwrap();
    old.exited().await;
    let link = sup.link().expect("connected");
    assert_eq!(link.daemon_app_version(), Some("2.0.0"));
    assert!(!link.is_draining());
    assert!(sup.accepts_new_sessions());
    let new = started.lock().unwrap().pop().expect("a new daemon started");
    new.stop().await;
}

#[tokio::test]
async fn a_daemon_that_never_came_up_is_started_again() {
    // A daemon spawned while its predecessor is still exiting loses the
    // instance lock and is gone; the launcher must not wait for it forever.
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let started: Arc<Mutex<Vec<Running>>> = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(0u32));
    let link = {
        let (started, calls) = (started.clone(), calls.clone());
        let root = root.path().to_path_buf();
        easysshd::launch::connect_or_spawn(
            &rt,
            Arc::new(Handler::default()),
            move || {
                let n = {
                    let mut calls = calls.lock().unwrap();
                    *calls += 1;
                    *calls
                };
                // The first start comes to nothing.
                if n > 1 {
                    let (started, root) = (started.clone(), root.clone());
                    tokio::spawn(async move {
                        let new = start_daemon(&root, "1.0.0", None).await;
                        started.lock().unwrap().push(new);
                    });
                }
                Ok(())
            },
            Duration::from_secs(20),
        )
        .await
        .expect("a later start answers")
    };
    assert_eq!(*calls.lock().unwrap(), 2);
    drop(link);
    let new = started.lock().unwrap().pop().expect("a daemon started");
    new.stop().await;
}

#[tokio::test]
async fn a_daemon_of_another_build_keeps_its_sessions_until_they_end() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let mut old = start_daemon(root.path(), "1.0.0", None).await;
    // An earlier app opened a session on the old daemon.
    let earlier = Link::connect(&rt, Arc::new(Handler::default()))
        .await
        .unwrap();
    let (old_seen, sink) = collector();
    let old_id = earlier.open_session(None, 80, 24, sink).await.unwrap();

    let started: Arc<Mutex<Vec<Running>>> = Arc::new(Mutex::new(Vec::new()));
    let spawn: easysshd::supervisor::Spawn = {
        let started = started.clone();
        let root = root.path().to_path_buf();
        Arc::new(move || {
            let (started, root) = (started.clone(), root.clone());
            tokio::spawn(async move {
                let new = start_daemon(&root, "2.0.0", None).await;
                started.lock().unwrap().push(new);
            });
            Ok(())
        })
    };
    let sup = Supervisor::new(
        supervisor_config(&rt, "2.0.0", spawn),
        Arc::new(Handler::default()),
    );
    sup.start().await.unwrap();

    // Attached to the old daemon for its session, but no new ones go there.
    assert!(sup.link().is_some());
    assert!(!sup.accepts_new_sessions());
    let listed = sup.list_sessions().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].session_id, old_id);
    let (seen, sink) = supervised();
    sup.attach_session(&old_id, None, sink).await.unwrap();
    earlier
        .write_session(&old_id, "echo still-$((6+1))\n")
        .unwrap();
    eventually("output on the old daemon", || {
        text(&seen).contains("still-7")
    })
    .await;
    let refused = sup
        .link()
        .unwrap()
        .open_session(None, 80, 24, collector().1)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "draining");
    drop(old_seen);

    // The last session ends: the old daemon leaves and the matching one
    // takes over.
    sup.close_session(&old_id);
    old.exited().await;
    eventually("the matching daemon is in use", || {
        sup.accepts_new_sessions()
            && sup
                .link()
                .and_then(|l| l.daemon_app_version().map(str::to_owned))
                == Some("2.0.0".to_owned())
    })
    .await;
    let new = started.lock().unwrap().pop().expect("a new daemon started");
    new.stop().await;
}

#[tokio::test]
async fn schema_mismatch_is_a_clear_error_and_a_stale_daemon_leaves_when_idle() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let mut daemon = start_daemon(root.path(), "1.0.0", None).await;

    // A client on an older schema: refused, the daemon stays.
    let mut c = Tc::connect(&rt).await;
    let e = c
        .call("proto.hello", json!({"schema_version": 0}))
        .await
        .unwrap_err();
    assert_eq!(e["code"], "schema_mismatch");
    let message = e["message"].as_str().unwrap();
    assert!(
        message.contains("schema 1") && message.contains("update the app"),
        "{message}"
    );
    assert!(!daemon.daemon.is_draining());
    c.call("hosts.list", json!({})).await.unwrap();

    // A client on a newer schema: the daemon is the stale one. It says so,
    // refuses new sessions and exits once none runs.
    let e = c
        .call("proto.hello", json!({"schema_version": 2}))
        .await
        .unwrap_err();
    assert_eq!(e["code"], "schema_mismatch");
    assert!(e["message"]
        .as_str()
        .unwrap()
        .contains("exit when its sessions end"));
    assert!(daemon.daemon.is_draining());
    let e = c
        .call(
            "session.open",
            json!({"host_id": null, "cols": 80, "rows": 24}),
        )
        .await
        .unwrap_err();
    assert_eq!(e["code"], "draining");
    drop(c);
    daemon.exited().await;
    assert!(Client::connect(&rt).await.is_err(), "the daemon is gone");
}

#[tokio::test]
async fn hello_reports_versions_and_draining() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let daemon = start_daemon(root.path(), "7.7.7", None).await;
    let mut c = Tc::connect(&rt).await;
    let hello = c
        .call(
            "proto.hello",
            json!({"schema_version": 1, "app_version": "7.7.7"}),
        )
        .await
        .unwrap();
    assert_eq!(hello["app_version"], "7.7.7");
    assert_eq!(hello["draining"], false);
    let ok = c
        .call("daemon.shutdown_when_idle", json!({}))
        .await
        .unwrap();
    assert_eq!(ok["running_sessions"], 0);
    let hello = c
        .call("proto.hello", json!({"schema_version": 1}))
        .await
        .unwrap();
    assert_eq!(hello["draining"], true);
    drop(c);
    let mut daemon = daemon;
    daemon.exited().await;
}

// ---- hosts.changed ----------------------------------------------------------

#[tokio::test]
async fn hosts_changed_reaches_every_attached_client() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let daemon = start_daemon(root.path(), "1.0.0", None).await;
    let mut a = Tc::connect(&rt).await;
    let mut b = Tc::connect(&rt).await;
    let watcher = Arc::new(Handler::default());
    let link = Link::connect(&rt, watcher.clone()).await.unwrap();

    let connection = Connection::alias("Prod", "prod");
    let group = json!({"id": "group-1", "name": "Production", "color": null});
    let mut value = serde_json::to_value(&connection).unwrap();
    value["group_id"] = json!("group-1");
    a.call("hosts.save", json!({"connection": value, "group": group}))
        .await
        .unwrap();
    assert_eq!(b.event("hosts.changed").await, json!({}));
    assert_eq!(a.event("hosts.changed").await, json!({}));
    eventually("the link's handler", || {
        *watcher.hosts_changed.lock().unwrap() == 1
    })
    .await;

    // The group came with the host.
    let list = b.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(list["connections"].as_array().unwrap().len(), 1);
    assert_eq!(list["groups"][0]["name"], "Production");

    a.call("hosts.delete", json!({"id": connection.id}))
        .await
        .unwrap();
    b.event("hosts.changed").await;
    eventually("the second change", || {
        *watcher.hosts_changed.lock().unwrap() == 2
    })
    .await;
    // A failed save changes nothing and tells nobody.
    let bad = json!({"connection": {"id": "x"}});
    assert!(a.call("hosts.save", bad).await.is_err());
    assert!(b.events.iter().all(|e| e["kind"] != "hosts.changed"));
    drop(link);
    daemon.stop().await;
}

// ---- askpass.resolved --------------------------------------------------------

#[tokio::test]
async fn an_answer_closes_the_prompt_for_the_other_clients() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let daemon = start_daemon(root.path(), "1.0.0", None).await;
    let mut first = Tc::connect(&rt).await;
    let second_handler = Arc::new(Handler::default());
    let _second = Link::connect(&rt, second_handler.clone()).await.unwrap();
    let endpoint = first.call("askpass.endpoint", json!({})).await.unwrap()["endpoint"]
        .as_str()
        .unwrap()
        .to_owned();

    let helper = tokio::task::spawn_blocking(move || {
        askpass::ask(
            &endpoint,
            "ctx",
            "prod-db",
            "postgres@10.0.2.20's password: ",
        )
    });
    let prompt = first.event("askpass.prompt").await;
    eventually("the prompt reaches the second client", || {
        !second_handler.prompts.lock().unwrap().is_empty()
    })
    .await;
    assert!(second_handler.resolved.lock().unwrap().is_empty());

    first
        .call(
            "askpass.reply",
            json!({"id": prompt["id"], "answer": "hunter2"}),
        )
        .await
        .unwrap();
    assert_eq!(helper.await.unwrap().as_deref(), Some("hunter2"));
    let id = prompt["id"].as_u64().unwrap();
    eventually("the second client is told", || {
        second_handler.resolved.lock().unwrap().contains(&id)
    })
    .await;
    assert_eq!(first.event("askpass.resolved").await, json!({"id": id}));
    daemon.stop().await;
}

// ---- SSH_AUTH_SOCK -----------------------------------------------------------

#[test]
fn agent_socket_validation() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("agent.sock");
    let _listener = UnixListener::bind(&sock).unwrap();
    let path = sock.to_str().unwrap();
    // SAFETY: geteuid cannot fail.
    let me = unsafe { libc::geteuid() };

    assert_eq!(validate_for_uid(path, me).unwrap(), sock);
    assert_eq!(validate_agent_socket(path).unwrap(), Some(sock.clone()));
    // A relative path, a regular file, a missing path and another owner.
    assert!(validate_agent_socket("agent.sock").is_err());
    assert!(validate_agent_socket("./agent.sock").is_err());
    let file = dir.path().join("plain");
    std::fs::write(&file, "x").unwrap();
    assert!(validate_agent_socket(file.to_str().unwrap()).is_err());
    assert!(validate_agent_socket(dir.path().to_str().unwrap()).is_err());
    assert!(validate_agent_socket("/nonexistent/agent").is_err());
    assert!(validate_agent_socket("").is_err());
    assert!(validate_for_uid(path, me.wrapping_add(1)).is_err());
    // A symlink to the socket is fine; the target decides.
    let link = dir.path().join("link.sock");
    std::os::unix::fs::symlink(&sock, &link).unwrap();
    assert!(validate_agent_socket(link.to_str().unwrap()).is_ok());
}

#[tokio::test]
async fn sessions_use_the_clients_agent_when_it_is_valid() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let daemon = start_daemon(root.path(), "1.0.0", None).await;
    let agents = tempfile::tempdir().unwrap();
    let sock = agents.path().join("agent.sock");
    let _listener = UnixListener::bind(&sock).unwrap();
    let mut c = Tc::connect(&rt).await;

    let open =
        |sock: Value| json!({"host_id": null, "cols": 200, "rows": 24, "ssh_auth_sock": sock});
    let id = c
        .call("session.open", open(json!(sock.to_str().unwrap())))
        .await
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        "session.write",
        json!({"session_id": id, "data": "echo agent=[$SSH_AUTH_SOCK]\n"}),
    )
    .await
    .unwrap();
    let out = c
        .until_output(&id, &format!("agent=[{}]", sock.display()))
        .await;
    assert!(out.contains("agent="));

    // A path that does not validate is ignored; the session still opens.
    let id = c
        .call("session.open", open(json!("relative/agent.sock")))
        .await
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        "session.write",
        json!({"session_id": id, "data": "echo marker-$((1+1))\n"}),
    )
    .await
    .unwrap();
    let out = c.until_output(&id, "marker-2").await;
    assert!(!out.contains("agent=[relative"), "{out}");
    daemon.stop().await;
}

// ---- the log ----------------------------------------------------------------

#[tokio::test]
async fn the_log_has_lifecycle_and_no_secrets_or_terminal_content() {
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let logs = root.path().join("logs");
    let daemon = start_daemon(root.path(), "1.0.0", Some(logs.clone())).await;
    let mut c = Tc::connect(&rt).await;

    // Terminal input and output.
    let id = c
        .call(
            "session.open",
            json!({"host_id": null, "cols": 80, "rows": 24}),
        )
        .await
        .unwrap()["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    c.call(
        "session.write",
        json!({"session_id": id, "data": "echo TERMINALSECRET-$((2+3))\n"}),
    )
    .await
    .unwrap();
    c.until_output(&id, "TERMINALSECRET-5").await;

    // An askpass prompt and its answer.
    let endpoint = c.call("askpass.endpoint", json!({})).await.unwrap()["endpoint"]
        .as_str()
        .unwrap()
        .to_owned();
    let helper = tokio::task::spawn_blocking(move || {
        askpass::ask(&endpoint, "ctx", "PROMPTHOST", "PROMPTSECRET password: ")
    });
    let prompt = c.event("askpass.prompt").await;
    c.call(
        "askpass.reply",
        json!({"id": prompt["id"], "answer": "ANSWERSECRET"}),
    )
    .await
    .unwrap();
    assert_eq!(helper.await.unwrap().as_deref(), Some("ANSWERSECRET"));

    // A malformed command whose validation error would quote its value, an
    // unknown host, and a host save.
    let _ = c
        .call(
            "askpass.reply",
            json!({"id": "BADVALUESECRET", "answer": null}),
        )
        .await;
    let _ = c
        .call(
            "session.open",
            json!({"host_id": "UNKNOWNHOSTSECRET", "cols": 80, "rows": 24}),
        )
        .await;
    let connection = Connection::alias("HOSTNAMESECRET", "hostalias");
    c.call(
        "hosts.save",
        json!({"connection": serde_json::to_value(&connection).unwrap()}),
    )
    .await
    .unwrap();
    c.call("session.close", json!({"session_id": id}))
        .await
        .unwrap();
    let read_log = || {
        let mut log = String::new();
        for entry in std::fs::read_dir(&logs).unwrap() {
            let entry = entry.unwrap();
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("easysshd.log")
            {
                log.push_str(&std::fs::read_to_string(entry.path()).unwrap());
            }
        }
        log
    };
    // The exit is logged when the closed shell has been reaped, which can
    // come after the reply to session.close; wait for it before stopping.
    eventually("the session exit is logged", || {
        read_log().contains("ended, exit code")
    })
    .await;
    drop(c);
    daemon.stop().await;

    let log = read_log();
    for kind in [
        "daemon created",
        "connected",
        "session.open",
        "askpass.reply",
        "hosts.save",
        "session.close",
        "failed: unknown_host",
        "rejected a message: invalid_command",
        "ended, exit code",
    ] {
        assert!(log.contains(kind), "the log lacks {kind:?}:\n{log}");
    }
    assert!(log.contains(&format!("session {id}")), "{log}");
    for secret in [
        "TERMINALSECRET",
        "PROMPTSECRET",
        "PROMPTHOST",
        "ANSWERSECRET",
        "BADVALUESECRET",
        "UNKNOWNHOSTSECRET",
        "HOSTNAMESECRET",
        "hostalias",
        "hunter2",
    ] {
        assert!(
            !log.contains(secret),
            "{secret} leaked into the log:\n{log}"
        );
    }
}

#[tokio::test]
async fn a_plain_link_still_reports_a_lost_connection_as_an_exit() {
    // A plain link (no supervisor) still reports a lost connection as an
    // exit, as before.
    let root = tempfile::tempdir().unwrap();
    let rt = root.path().join("run");
    let daemon = start_daemon(root.path(), "1.0.0", None).await;
    let link = Link::connect(&rt, Arc::new(Handler::default()))
        .await
        .unwrap();
    let (seen, sink) = collector();
    link.open_session(None, 80, 24, sink).await.unwrap();
    daemon.daemon.drop_connections();
    eventually("exit on loss", || has(&seen, &exited())).await;
    assert!(!link.is_alive());
    daemon.stop().await;
}
