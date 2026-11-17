//! Serial ports as sessions, over real IPC. A pseudo-terminal pair stands in
//! for the device: the test holds the master side (the "device") and the
//! daemon opens the slave side by its path like any serial port.
#![cfg(unix)]

use easyssh_core::{ConfigStore, Connection, ConnectionTarget, SerialSettings};
use easyssh_proto as proto;
use easysshd::{Client, Daemon, DaemonConfig, Server};
use portable_pty::{native_pty_system, MasterPty, PtySize};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
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

async fn start() -> Running {
    let dir = tempfile::tempdir().unwrap();
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(dir.path().join("connections.json")),
        local_shell: Some("/bin/sh".into()),
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
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
            proto::validate_event(&v).expect("event fails the schema");
        } else {
            proto::validate("Reply", &v).expect("reply fails the schema");
        }
        v
    }

    async fn call(&mut self, kind: &str, payload: Value) -> Result<Value, Value> {
        let id = uuid::Uuid::new_v4().to_string();
        let cmd = json!({"id": id, "kind": kind, "payload": payload});
        proto::validate_command(&cmd).expect("command fails the schema");
        self.client.send(cmd).await.unwrap();
        loop {
            let v = self.recv().await;
            if v.get("kind").is_some() {
                self.events.push(v);
            } else {
                return match v.get("ok") {
                    Some(ok) => Ok(ok.clone()),
                    None => Err(v["error"].clone()),
                };
            }
        }
    }

    fn output(&self, session_id: &str) -> String {
        self.events
            .iter()
            .filter(|e| e["kind"] == "session.output" && e["payload"]["session_id"] == session_id)
            .map(|e| e["payload"]["data"].as_str().unwrap())
            .collect()
    }

    async fn until_output(&mut self, session_id: &str, needle: &str) {
        while !self.output(session_id).contains(needle) {
            let v = self.recv().await;
            assert!(v.get("kind").is_some(), "unexpected reply {v}");
            self.events.push(v);
        }
    }

    async fn until_exit(&mut self, session_id: &str) {
        loop {
            if self
                .events
                .iter()
                .any(|e| e["kind"] == "session.exit" && e["payload"]["session_id"] == session_id)
            {
                return;
            }
            let v = self.recv().await;
            assert!(v.get("kind").is_some(), "unexpected reply {v}");
            self.events.push(v);
        }
    }
}

/// The "device": the master side of a pty, and the path of its slave side.
struct FakeDevice {
    /// Kept open: the pty lives as long as its master.
    _master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    received: mpsc::Receiver<Vec<u8>>,
    path: String,
    stop: Arc<AtomicBool>,
}

impl Drop for FakeDevice {
    /// Pulling the plug: the reader thread lets go of its end of the pty,
    /// which hangs up the slave the daemon has open.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl FakeDevice {
    fn new() -> Self {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let path = pair
            .master
            .tty_name()
            .expect("this platform names its pty slaves")
            .to_string_lossy()
            .into_owned();
        // The slave is only a path to the daemon: close our end.
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().unwrap();
        let writer = pair.master.take_writer().unwrap();
        let (tx, received) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 1024];
            // Until something opens the slave, reads fail with a hangup;
            // keep trying, as a real device would just be quiet.
            while !stopped.load(Ordering::SeqCst) {
                match reader.read(&mut buffer) {
                    Ok(read) if read > 0 => {
                        if tx.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                    _ => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        FakeDevice {
            _master: pair.master,
            writer,
            received,
            path,
            stop,
        }
    }

    fn say(&mut self, text: &str) {
        self.writer.write_all(text.as_bytes()).unwrap();
        self.writer.flush().unwrap();
    }

    /// What the daemon sent to the device, until `needle` shows up.
    fn heard(&self, needle: &str) -> bool {
        let mut seen = String::new();
        let deadline = std::time::Instant::now() + WAIT;
        while std::time::Instant::now() < deadline {
            if let Ok(chunk) = self.received.recv_timeout(Duration::from_millis(100)) {
                seen.push_str(&String::from_utf8_lossy(&chunk));
                if seen.contains(needle) {
                    return true;
                }
            }
        }
        false
    }
}

fn serial_host(name: &str, port: &str) -> Value {
    let mut connection = Connection::alias(name, "x");
    connection.target = ConnectionTarget::Serial(SerialSettings::new(port));
    serde_json::to_value(&connection).unwrap()
}

#[tokio::test]
async fn a_serial_host_opens_reads_writes_and_closes() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let mut device = FakeDevice::new();

    let host = serial_host("Router console", &device.path);
    let host_id = host["id"].as_str().unwrap().to_owned();
    c.call("hosts.save", json!({"connection": host}))
        .await
        .unwrap();
    // It is stored and listed as a serial host.
    let list = c.call("hosts.list", json!({})).await.unwrap();
    proto::validate("HostsListResult", &list).unwrap();
    assert_eq!(list["connections"][0]["target"]["kind"], "serial");
    assert_eq!(list["connections"][0]["target"]["baud"], 115_200);

    let opened = c
        .call(
            "session.open",
            json!({"host_id": host_id, "cols": 80, "rows": 24}),
        )
        .await
        .unwrap();
    let id = opened["session_id"].as_str().unwrap().to_owned();
    // No SSH engine is involved.
    assert!(opened.get("engine").is_none());

    // The device talks: the text reaches the client, with offsets.
    device.say("U-Boot 2024.01\r\nautoboot: ");
    c.until_output(&id, "autoboot: ").await;
    assert!(c.output(&id).contains("U-Boot 2024.01"));

    // The client types: the device hears it, byte for byte.
    c.call("session.write", json!({"session_id": id, "data": "AT\r"}))
        .await
        .unwrap();
    assert!(device.heard("AT\r"), "the device never received the input");

    // Resizing a serial line is harmless.
    c.call(
        "session.resize",
        json!({"session_id": id, "cols": 120, "rows": 40}),
    )
    .await
    .unwrap();

    // A second client attaching later replays the scrollback.
    let mut late = Tc::connect(&s.runtime()).await;
    late.call("session.attach", json!({"session_id": id}))
        .await
        .unwrap();
    late.until_output(&id, "U-Boot 2024.01").await;

    c.call("session.close", json!({"session_id": id}))
        .await
        .unwrap();
    c.until_exit(&id).await;
    s.stop().await;
}

#[tokio::test]
async fn the_session_ends_when_the_device_goes_away() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let mut device = FakeDevice::new();
    let host = serial_host("Unplugged", &device.path);
    let host_id = host["id"].as_str().unwrap().to_owned();
    c.call("hosts.save", json!({"connection": host}))
        .await
        .unwrap();
    let opened = c
        .call(
            "session.open",
            json!({"host_id": host_id, "cols": 80, "rows": 24}),
        )
        .await
        .unwrap();
    let id = opened["session_id"].as_str().unwrap().to_owned();
    // The line works, then the cable is pulled.
    device.say("login: ");
    c.until_output(&id, "login: ").await;
    drop(device);
    c.until_exit(&id).await;
    let list = c.call("session.list", json!({})).await.unwrap();
    assert_eq!(list["sessions"][0]["running"], false);
    s.stop().await;
}

#[tokio::test]
async fn a_missing_port_fails_the_open_with_a_clear_code() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let host = serial_host("Nothing there", "/dev/definitely-not-a-serial-port");
    let host_id = host["id"].as_str().unwrap().to_owned();
    c.call("hosts.save", json!({"connection": host}))
        .await
        .unwrap();
    let error = c
        .call(
            "session.open",
            json!({"host_id": host_id, "cols": 80, "rows": 24}),
        )
        .await
        .unwrap_err();
    assert_eq!(error["code"], "serial_not_found");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("definitely-not-a-serial-port"));
    s.stop().await;
}

#[tokio::test]
async fn ports_are_listed_and_bad_serial_hosts_are_refused() {
    let s = start().await;
    let mut c = Tc::connect(&s.runtime()).await;
    let ports = c.call("serial.ports", json!({})).await.unwrap();
    proto::validate("SerialPortsResult", &ports).unwrap();
    assert!(ports["ports"].is_array());

    // The schema already refuses a baud rate that is out of range.
    let mut bad = serial_host("Bad baud", "/dev/ttyS0");
    bad["target"]["baud"] = json!(7);
    assert!(proto::validate("Connection", &bad).is_err());
    // The daemon checks what the schema cannot: a port path is one line.
    let bad = serial_host("Bad path", "/dev/tty\nUSB0");
    let error = c
        .call("hosts.save", json!({"connection": bad}))
        .await
        .unwrap_err();
    assert_eq!(error["code"], "invalid_host");

    s.stop().await;
}
