//! Daemon services over real IPC with a local `/bin/sh` PTY.
#![cfg(unix)]

use easyssh_core::{ConfigStore, Connection};
use easyssh_proto as proto;
use easysshd::askpass;
use easysshd::link::{AskpassPrompt, Link, LinkHandler, SessionMsg};
use easysshd::{Client, Daemon, DaemonConfig, Server};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(15);

struct Running {
    dir: TempDir,
    daemon: Arc<Daemon>,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Running {
    fn runtime(&self) -> PathBuf {
        self.dir.path().join("run")
    }
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.await.unwrap();
        self.daemon.shutdown();
    }
}

async fn start_with(scrollback: usize, idle: Duration) -> Running {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(dir.path().join("connections.json")),
        local_shell: Some("/bin/sh".into()),
        scrollback_bytes: scrollback,
        idle_timeout: idle,
    };
    let daemon = Daemon::new(cfg).await.unwrap();
    let server = Server::start_service(dir.path().join("run"), daemon.clone()).unwrap();
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        server
            .serve(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    Running {
        dir,
        daemon,
        stop: Some(tx),
        task,
    }
}

async fn start() -> Running {
    start_with(1 << 20, Duration::from_secs(600)).await
}

/// A raw protocol client that keeps the events it saw while waiting.
struct Tc {
    client: Client,
    events: Vec<Value>,
}

impl Tc {
    async fn connect(rt: &std::path::Path) -> Tc {
        let client = Client::connect(rt).await.unwrap();
        let mut tc = Tc {
            client,
            events: Vec::new(),
        };
        let hello = tc
            .call("proto.hello", json!({"schema_version": 1}))
            .await
            .unwrap();
        assert_eq!(hello["schema_version"], 1);
        tc
    }

    /// The next reply, keeping events seen on the way.
    async fn reply(&mut self) -> Value {
        loop {
            let v = self.recv().await;
            if v.get("kind").is_none() {
                return v;
            }
            self.events.push(v);
        }
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

    /// Send a command and return its `ok` (or the `error`), keeping events.
    async fn call(&mut self, kind: &str, payload: Value) -> Result<Value, Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let cmd = json!({"id": id, "kind": kind, "payload": payload});
        self.client.send(cmd).await.unwrap();
        loop {
            let v = self.recv().await;
            if v.get("kind").is_some() {
                self.events.push(v);
            } else {
                assert_eq!(v["id"], id.as_str(), "reply for another command");
                return match v.get("ok") {
                    Some(ok) => Ok(ok.clone()),
                    None => Err(v["error"].clone()),
                };
            }
        }
    }

    /// Output text of `session_id` seen so far (events already collected).
    fn output(&self, session_id: &str) -> String {
        self.events
            .iter()
            .filter(|e| e["kind"] == "session.output" && e["payload"]["session_id"] == session_id)
            .map(|e| e["payload"]["data"].as_str().unwrap())
            .collect()
    }

    /// The `session.exit` payload, whether it already arrived or not.
    async fn until_exit(&mut self, session_id: &str) -> Value {
        loop {
            if let Some(e) = self
                .events
                .iter()
                .find(|e| e["kind"] == "session.exit" && e["payload"]["session_id"] == session_id)
            {
                return e["payload"].clone();
            }
            let v = self.recv().await;
            assert!(v.get("kind").is_some(), "unexpected reply {v}");
            self.events.push(v);
        }
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

    async fn open(&mut self) -> String {
        let ok = self
            .call(
                "session.open",
                json!({"host_id": null, "cols": 80, "rows": 24}),
            )
            .await
            .unwrap();
        ok["session_id"].as_str().unwrap().to_owned()
    }

    async fn write(&mut self, session_id: &str, data: &str) {
        self.call(
            "session.write",
            json!({"session_id": session_id, "data": data}),
        )
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn open_write_output_resize_close() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let id = c.open().await;

    // The typed text is echoed as `$((1+1))`; only the shell's evaluation
    // produces `marker-2`.
    c.write(&id, "echo marker-$((1+1))\n").await;
    c.until_output(&id, "marker-2").await;

    // Output events carry the per-session byte offset, contiguous.
    let mut next = 0;
    for e in c
        .events
        .iter()
        .filter(|e| e["kind"] == "session.output" && e["payload"]["session_id"] == id.as_str())
    {
        assert_eq!(e["payload"]["seq"], next);
        next += e["payload"]["data"].as_str().unwrap().len() as u64;
    }
    // Event `seq` is per connection and strictly increasing.
    let seqs: Vec<u64> = c
        .events
        .iter()
        .map(|e| e["seq"].as_u64().unwrap())
        .collect();
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");

    c.call(
        "session.resize",
        json!({"session_id": id, "cols": 100, "rows": 30}),
    )
    .await
    .unwrap();
    c.write(&id, "stty size\n").await;
    c.until_output(&id, "30 100").await;

    let list = c.call("session.list", json!({})).await.unwrap();
    let info = &list["sessions"][0];
    assert_eq!(info["session_id"], id.as_str());
    assert_eq!(info["running"], true);
    assert_eq!(info["cols"], 100);
    assert_eq!(info["rows"], 30);
    assert!(info["next_seq"].as_u64().unwrap() >= c.output(&id).len() as u64);

    c.call("session.close", json!({"session_id": id}))
        .await
        .unwrap();
    // The child is killed and the attached client hears about it.
    c.until_exit(&id).await;
    let gone = c
        .call("session.write", json!({"session_id": id, "data": "x"}))
        .await
        .unwrap_err();
    assert_eq!(gone["code"], "unknown_session");
    s.stop().await;
}

#[tokio::test]
async fn attach_after_detach_replays_scrollback() {
    let s = start().await;
    let mut a = Tc::connect(&s.runtime()).await;
    let id = a.open().await;
    a.write(&id, "echo before-$((2+3))\n").await;
    a.until_output(&id, "before-5").await;
    // The first client goes away (a UI reload); the session keeps running.
    drop(a);

    let mut b = Tc::connect(&s.runtime()).await;
    let list = b.call("session.list", json!({})).await.unwrap();
    assert_eq!(list["sessions"][0]["session_id"], id.as_str());
    assert_eq!(list["sessions"][0]["running"], true);

    let attached = b
        .call("session.attach", json!({"session_id": id}))
        .await
        .unwrap();
    assert_eq!(attached["truncated"], false);
    assert_eq!(attached["first_seq"], 0);
    // The reply came first, then the replay.
    let replay = b.until_output(&id, "before-5").await;
    assert!(replay.len() as u64 <= attached["next_seq"].as_u64().unwrap() + 1024);
    // New output continues after the replay on the same attachment.
    b.write(&id, "echo after-$((2+3))\n").await;
    let all = b.until_output(&id, "after-5").await;
    assert!(all.find("before-5").unwrap() < all.find("after-5").unwrap());

    // `from_seq` skips what the client already has.
    let mut c = Tc::connect(&s.runtime()).await;
    let next = attached["next_seq"].as_u64().unwrap();
    let r = c
        .call(
            "session.attach",
            json!({"session_id": id, "from_seq": next}),
        )
        .await
        .unwrap();
    assert_eq!(r["truncated"], false);
    c.write(&id, "echo third-$((2+3))\n").await;
    let out = c.until_output(&id, "third-5").await;
    assert!(!out.contains("before-5"), "{out:?}");

    // Detach stops delivery to that connection only.
    c.call("session.detach", json!({"session_id": id}))
        .await
        .unwrap();
    s.stop().await;
}

#[tokio::test]
async fn ended_session_stays_for_reattach() {
    let s = start().await;
    let mut a = Tc::connect(&s.runtime()).await;
    let id = a.open().await;
    a.write(&id, "echo bye-$((4+4)); exit 3\n").await;
    assert_eq!(a.until_exit(&id).await["code"], 3);
    drop(a);
    let mut b = Tc::connect(&s.runtime()).await;
    let attached = b
        .call("session.attach", json!({"session_id": id}))
        .await
        .unwrap();
    assert_eq!(attached["running"], false);
    b.until_output(&id, "bye-8").await;
    assert_eq!(b.until_exit(&id).await["code"], 3);
    b.call("session.close", json!({"session_id": id}))
        .await
        .unwrap();
    let list = b.call("session.list", json!({})).await.unwrap();
    assert_eq!(list["sessions"].as_array().unwrap().len(), 0);
    s.stop().await;
}

#[tokio::test]
async fn ring_buffer_drops_oldest() {
    let cap = 2048;
    let s = start_with(cap, Duration::from_secs(600)).await;
    let mut a = Tc::connect(&s.runtime()).await;
    let id = a.open().await;
    // 6000 filler bytes, then a marker, all after the first marker.
    a.write(&id, "echo first-$((1+1))\n").await;
    a.until_output(&id, "first-2").await;
    a.write(
        &id,
        "head -c 6000 /dev/zero | tr '\\0' x; echo; echo last-$((1+1))\n",
    )
    .await;
    a.until_output(&id, "last-2").await;
    let total = a.output(&id).len();
    assert!(total > cap * 2);

    let mut b = Tc::connect(&s.runtime()).await;
    let attached = b
        .call("session.attach", json!({"session_id": id}))
        .await
        .unwrap();
    assert_eq!(attached["truncated"], true);
    let first = attached["first_seq"].as_u64().unwrap();
    assert!(first > 0);
    let next = attached["next_seq"].as_u64().unwrap();
    assert!(next - first <= cap as u64, "{first}..{next}");
    let replay = b.until_output(&id, "last-2").await;
    assert_eq!(replay.len() as u64, next - first);
    assert!(!replay.contains("first-2"), "oldest output was kept");
    s.stop().await;
}

#[tokio::test]
async fn two_clients_receive_the_same_output() {
    let s = start().await;
    let mut a = Tc::connect(&s.runtime()).await;
    let mut b = Tc::connect(&s.runtime()).await;
    let id = a.open().await;
    b.call("session.attach", json!({"session_id": id}))
        .await
        .unwrap();
    a.write(&id, "echo shared-$((6*7))\n").await;
    let out_a = a.until_output(&id, "shared-42").await;
    let out_b = b.until_output(&id, "shared-42").await;
    // The same stream, byte for byte, from the start.
    let upto = |s: &str| s[..s.find("shared-42").unwrap() + 9].to_owned();
    assert_eq!(upto(&out_a), upto(&out_b));
    // Input from the second client reaches the same shell.
    b.write(&id, "echo from-b-$((6*7))\n").await;
    a.until_output(&id, "from-b-42").await;
    s.stop().await;
}

#[tokio::test]
async fn invalid_commands_are_rejected_by_the_schema() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let id = c.open().await;

    // Wrong payload shape (session_id must be a string, extra fields refused).
    let e = c
        .call("session.write", json!({"session_id": 1, "data": "x"}))
        .await
        .unwrap_err();
    assert_eq!(e["code"], "invalid_command");
    let e = c
        .call(
            "session.resize",
            json!({"session_id": id, "cols": 100000, "rows": 1}),
        )
        .await
        .unwrap_err();
    assert_eq!(e["code"], "invalid_command");
    let e = c
        .call("session.close", json!({"session_id": id, "force": true}))
        .await
        .unwrap_err();
    assert_eq!(e["code"], "invalid_command");
    // Unknown kind.
    let e = c.call("session.explode", json!({})).await.unwrap_err();
    assert_eq!(e["code"], "unknown_command");

    // Not an envelope at all: the reply still validates and uses the nil id.
    c.client.send(json!({"hello": "world"})).await.unwrap();
    let v = c.reply().await;
    assert_eq!(v["id"], "00000000-0000-0000-0000-000000000000");
    assert_eq!(v["error"]["code"], "invalid_command");
    // Bad id format.
    c.client
        .send(json!({"id": "nope", "kind": "hosts.list", "payload": {}}))
        .await
        .unwrap();
    let v = c.reply().await;
    assert_eq!(v["error"]["code"], "invalid_command");

    // The connection and the session survive all of that.
    c.write(&id, "echo alive-$((1+1))\n").await;
    c.until_output(&id, "alive-2").await;
    s.stop().await;
}

#[tokio::test]
async fn hosts_roundtrip_through_the_config_store() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let empty = c.call("hosts.list", json!({})).await.unwrap();
    proto::validate("HostsListResult", &empty).unwrap();
    assert_eq!(empty["connections"].as_array().unwrap().len(), 0);

    let connection = Connection::alias("Production", "prod");
    let value = serde_json::to_value(&connection).unwrap();
    let saved = c
        .call("hosts.save", json!({"connection": value}))
        .await
        .unwrap();
    assert_eq!(saved["id"], connection.id.as_str());

    // Stored in the SQLite database (the v1 JSON is not written), which a
    // second process can open while the daemon runs.
    assert!(!s.dir.path().join("connections.json").exists());
    let other = easyssh_store::open_app_store(&s.dir.path().join("connections.json")).unwrap();
    assert_eq!(other.store.list_hosts().unwrap().len(), 1);

    let list = c.call("hosts.list", json!({})).await.unwrap();
    proto::validate("HostsListResult", &list).unwrap();
    assert_eq!(list["connections"][0]["name"], "Production");

    // Unsafe values are refused like in the app.
    let mut bad = serde_json::to_value(Connection::alias("x", "-oProxyCommand=evil")).unwrap();
    bad["name"] = json!("bad");
    let e = c
        .call("hosts.save", json!({"connection": bad}))
        .await
        .unwrap_err();
    assert_eq!(e["code"], "invalid_host");

    c.call("hosts.delete", json!({"id": connection.id}))
        .await
        .unwrap();
    let list = c.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(list["connections"].as_array().unwrap().len(), 0);

    // Unknown hosts do not open sessions.
    let e = c
        .call(
            "session.open",
            json!({"host_id": "missing", "cols": 80, "rows": 24}),
        )
        .await
        .unwrap_err();
    assert_eq!(e["code"], "unknown_host");
    s.stop().await;
}

#[tokio::test]
async fn askpass_prompts_reach_clients_and_answers_return() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let endpoint = c.call("askpass.endpoint", json!({})).await.unwrap()["endpoint"]
        .as_str()
        .unwrap()
        .to_owned();

    // A helper process would do exactly this.
    let ep = endpoint.clone();
    let helper = std::thread::spawn(move || {
        askpass::ask(&ep, "ctx-1", "prod-db", "postgres@10.0.2.20's password: ")
    });
    let prompt = loop {
        let v = c.recv().await;
        if v["kind"] == "askpass.prompt" {
            break v["payload"].clone();
        }
    };
    assert_eq!(prompt["host"], "prod-db");
    assert_eq!(prompt["confirm"], false);
    assert!(prompt["prompt"].as_str().unwrap().contains("password"));
    c.call(
        "askpass.reply",
        json!({"id": prompt["id"], "answer": "hunter2"}),
    )
    .await
    .unwrap();
    assert_eq!(helper.join().unwrap().as_deref(), Some("hunter2"));

    // A host-key question is flagged as a confirmation; cancelling it makes
    // the same process's next prompt fail without asking again.
    let ep = endpoint.clone();
    let helper = std::thread::spawn(move || {
        askpass::ask(&ep, "ctx-2", "h", "Are you sure (yes/no/[fingerprint])? ")
    });
    let prompt = loop {
        let v = c.recv().await;
        if v["kind"] == "askpass.prompt" {
            break v["payload"].clone();
        }
    };
    assert_eq!(prompt["confirm"], true);
    c.call("askpass.reply", json!({"id": prompt["id"], "answer": null}))
        .await
        .unwrap();
    assert_eq!(helper.join().unwrap(), None);
    let ep = endpoint.clone();
    let again = tokio::task::spawn_blocking(move || askpass::ask(&ep, "ctx-2", "h", "again: "))
        .await
        .unwrap();
    assert_eq!(again, None);

    // A wrong token is refused.
    let bad = format!("{} wrong", endpoint.split(' ').next().unwrap());
    let refused = tokio::task::spawn_blocking(move || askpass::ask(&bad, "c", "h", "p: "))
        .await
        .unwrap();
    assert_eq!(refused, None);

    // With nobody attached there is nobody to ask.
    drop(c);
    eventually("the daemon notices the client left", || {
        s.daemon.client_count() == 0
    })
    .await;
    let ep = endpoint.clone();
    let nobody = tokio::task::spawn_blocking(move || askpass::ask(&ep, "ctx-3", "h", "p: "))
        .await
        .unwrap();
    assert_eq!(nobody, None);
    s.stop().await;
}

#[tokio::test]
async fn exits_when_idle_but_not_while_in_use() {
    let s = start_with(1 << 20, Duration::from_millis(400)).await;
    let c = Tc::connect(&s.runtime()).await;
    // A connected client keeps it alive.
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), s.daemon.wait_idle())
            .await
            .is_err()
    );
    drop(c);
    tokio::time::timeout(Duration::from_secs(10), s.daemon.wait_idle())
        .await
        .expect("idle daemon should finish waiting");

    // A running session keeps it alive without any client.
    let mut c = Tc::connect(&s.runtime()).await;
    let id = c.open().await;
    drop(c);
    assert!(
        tokio::time::timeout(Duration::from_millis(1200), s.daemon.wait_idle())
            .await
            .is_err()
    );
    s.daemon.shutdown();
    let _ = id;
    tokio::time::timeout(Duration::from_secs(10), s.daemon.wait_idle())
        .await
        .expect("no running session left");
    s.stop().await;
}

struct Handler {
    prompts: Mutex<Vec<AskpassPrompt>>,
}

impl LinkHandler for Handler {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        self.prompts.lock().unwrap().push(prompt);
    }
}

type Seen = Arc<Mutex<Vec<SessionMsg>>>;

fn collector() -> (Seen, Box<dyn FnMut(SessionMsg) -> bool + Send>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    (
        seen,
        Box::new(move |m| {
            sink.lock().unwrap().push(m);
            true
        }),
    )
}

fn text(seen: &Seen) -> String {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|m| match m {
            SessionMsg::Data { data, .. } => Some(data.as_str()),
            SessionMsg::Exit { .. } => None,
        })
        .collect()
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while !f() {
        assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn link_survives_a_reload_like_the_app_does() {
    let s = start().await;
    let handler = Arc::new(Handler {
        prompts: Mutex::new(Vec::new()),
    });
    let link = Link::connect(s.runtime(), handler.clone()).await.unwrap();
    assert!(link.askpass_endpoint().contains(' '));

    let (first, sink) = collector();
    let id = link.open_session(None, 80, 24, sink).await.unwrap();
    link.write_session(&id, "echo one-$((1+1))\n").unwrap();
    eventually("first output", || text(&first).contains("one-2")).await;

    // "Reload": the old sink is gone, the new one gets the replay.
    link.detach_session(&id);
    let (second, sink) = collector();
    let attached = link.attach_session(&id, None, sink).await.unwrap();
    assert!(attached.running);
    eventually("replay", || text(&second).contains("one-2")).await;
    link.write_session(&id, "echo two-$((1+1))\n").unwrap();
    eventually("live", || text(&second).contains("two-2")).await;
    // The first sink saw nothing after it was detached.
    assert!(!text(&first).contains("two-2"));

    let sessions = link.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, id);

    link.resize_session(&id, 90, 20).unwrap();
    link.write_session(&id, "exit 7\n").unwrap();
    eventually("exit", || {
        second
            .lock()
            .unwrap()
            .contains(&SessionMsg::Exit { code: Some(7) })
    })
    .await;
    link.close_session(&id);

    // A session killed by daemon shutdown tells its sink.
    let (third, sink) = collector();
    let id2 = link.open_session(None, 80, 24, sink).await.unwrap();
    s.daemon.shutdown();
    let _ = id2;
    s.stop().await;
    eventually("shutdown exit", || {
        third
            .lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, SessionMsg::Exit { .. }))
    })
    .await;
}

#[tokio::test]
async fn app_starts_the_daemon_detached_and_reconnects() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("run");
    let exe = PathBuf::from(env!("CARGO_BIN_EXE_easysshd"));
    let handler = Arc::new(Handler {
        prompts: Mutex::new(Vec::new()),
    });
    let args = vec![
        "--runtime-dir".into(),
        rt.clone().into_os_string(),
        "--config".into(),
        dir.path().join("c.json").into_os_string(),
        // Exit soon after the test, so no process is left behind.
        "--idle-timeout-secs".into(),
        "2".into(),
    ];
    let link = easysshd::launch::connect_or_spawn(
        &rt,
        handler.clone(),
        || easysshd::launch::spawn_detached(&exe, &args),
        Duration::from_secs(20),
    )
    .await
    .expect("daemon starts");
    let (seen, sink) = collector();
    // A real default shell here: this daemon was not given /bin/sh.
    let id = link.open_session(None, 80, 24, sink).await.unwrap();
    link.write_session(&id, "echo detached-$((3+4))\n").unwrap();
    eventually("output", || text(&seen).contains("detached-7")).await;
    drop(link);

    // A second "app launch" finds it running and does not spawn another.
    let again = easysshd::launch::connect_or_spawn(
        &rt,
        handler,
        || panic!("daemon should already be running"),
        Duration::from_secs(5),
    )
    .await
    .expect("reconnect");
    let sessions = again.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, id);
    again.close_session(&id);
}
