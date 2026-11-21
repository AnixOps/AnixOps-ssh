//! TEST ONLY. Runs the real session daemon (built-in engine) against the e2e
//! OpenSSH server and relays its sessions over stdin/stdout as JSON lines, so
//! the app's frontend in a browser (Playwright, `scripts/terminal-e2e.mjs`)
//! talks to a real SSH server through the same daemon the app uses.
//!
//! In:  {"op":"open","host":"e2e-target","cols":80,"rows":24,"seq":1,"integration":false}
//!      {"op":"write","id":"…","data":"…"}
//!      {"op":"resize","id":"…","cols":100,"rows":30}
//!      {"op":"close","id":"…"}
//!      {"op":"request","seq":2,"kind":"sftp.list","payload":{…},"host":"e2e-target"}
//!        (any daemon request; with `host`, its saved id goes in `host_id`,
//!        or in `tunnel.host_id` for `tunnel.start`)
//! Out: {"op":"ready"}
//!      {"op":"opened","seq":1,"id":"…"} or {"op":"error","seq":1,"message":"…"}
//!      {"op":"data","id":"…","data":"…"}
//!      {"op":"exit","id":"…","code":0,"reason":"exit"}
//!      {"op":"reply","seq":2,"value":{…}} or {"op":"reply","seq":2,"error":"…"}
//!      {"op":"transfer","id":"…","event":{"kind":"progress","bytes":1,"total":2}}
//!      {"op":"tunnel","status":{"id":"…","state":"running","bind_port":1080,…}}
//!
//! Needs the e2e containers (see e2e/README.md) and nothing else; it never
//! reads the user's SSH configuration.
#[cfg(unix)]
mod bridge {
    use easyssh_core::{ConfigStore, Connection, SshEngine};
    use easyssh_e2e::E2e;
    use easysshd::link::{
        AskpassPrompt, HostKeyPrompt, Link, LinkHandler, SessionMsg, TunnelStatus,
    };
    use easysshd::{Daemon, DaemonConfig, Server};
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, BufReader};

    /// Accepts host keys and answers no password prompts, like the e2e client.
    struct Answerer {
        link: Mutex<Option<Link>>,
    }

    impl LinkHandler for Answerer {
        fn askpass_prompt(&self, prompt: AskpassPrompt) {
            if let Some(link) = self.link.lock().unwrap().clone() {
                let _ = link.askpass_reply(prompt.id, None);
            }
        }

        fn hostkey_prompt(&self, prompt: HostKeyPrompt) {
            if let Some(link) = self.link.lock().unwrap().clone() {
                let _ = link.hostkey_reply(prompt.id, "accept_and_save");
            }
        }

        fn transfer_started(&self, transfer_id: &str) {
            emit(json!({"op": "transfer", "id": transfer_id, "event": {"kind": "started"}}));
        }

        fn transfer_progress(&self, transfer_id: &str, bytes: u64, total: Option<u64>) {
            emit(json!({
                "op": "transfer",
                "id": transfer_id,
                "event": {"kind": "progress", "bytes": bytes, "total": total},
            }));
        }

        fn transfer_note(&self, transfer_id: &str, note: &str, resumed_from: Option<u64>) {
            emit(json!({
                "op": "transfer",
                "id": transfer_id,
                "event": {"kind": "note", "note": note, "resumedFrom": resumed_from},
            }));
        }

        fn transfer_verify(&self, transfer_id: &str, verify: &serde_json::Value) {
            emit(json!({
                "op": "transfer",
                "id": transfer_id,
                "event": {
                    "kind": "verify",
                    "status": verify["status"],
                    "files": verify["files"],
                    "path": verify["path"],
                    "localSha256": verify["local_sha256"],
                    "remoteSha256": verify["remote_sha256"],
                    "detail": verify["detail"],
                },
            }));
        }

        fn transfer_done(&self, transfer_id: &str, status: &str, error: Option<&str>) {
            let event = match status {
                "completed" => json!({"kind": "done"}),
                "cancelled" => json!({"kind": "cancelled"}),
                _ => json!({"kind": "failed", "message": error.unwrap_or("the transfer failed")}),
            };
            emit(json!({"op": "transfer", "id": transfer_id, "event": event}));
        }

        fn tunnel_status(&self, status: TunnelStatus) {
            emit(json!({
                "op": "tunnel",
                "status": {
                    "id": status.id,
                    "state": status.state,
                    "bind_port": status.bind_port,
                    "error": status.error,
                    "channel_errors": status.channel_errors,
                    "last_channel_error": status.last_channel_error,
                },
            }));
        }
    }

    /// The saved connection for `host`, with or without the shell
    /// integration, saving it on first use (each `Connection::alias` call
    /// makes a new id).
    async fn saved_connection(
        link: &Link,
        saved: &mut HashMap<(String, bool), Connection>,
        host: &str,
        integration: bool,
    ) -> Result<Connection, String> {
        let key = (host.to_owned(), integration);
        if let Some(connection) = saved.get(&key) {
            return Ok(connection.clone());
        }
        let mut connection = Connection::alias(host, host);
        connection.shell_integration = integration;
        link.request(
            "hosts.save",
            json!({"connection": serde_json::to_value(&connection).unwrap()}),
        )
        .await
        .map_err(|error| error.to_string())?;
        saved.insert(key, connection.clone());
        Ok(connection)
    }

    static OUT: Mutex<()> = Mutex::new(());

    fn emit(value: Value) {
        let _guard = OUT.lock().unwrap();
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{value}");
        let _ = stdout.flush();
    }

    pub async fn run() {
        let e2e = E2e::new();
        let config_path = e2e.dir.path().join("connections.json");
        let config = DaemonConfig {
            config_store: ConfigStore::at(config_path.clone()),
            local_shell: None,
            scrollback_bytes: 1 << 20,
            idle_timeout: Duration::from_secs(3600),
        };
        let daemon = Daemon::new(config).await.expect("daemon");
        let engine = match std::env::var("EASYSSH_BRIDGE_ENGINE").as_deref() {
            Ok("openssh") => SshEngine::Openssh,
            _ => SshEngine::Russh,
        };
        easyssh_store::open_app_store(&config_path)
            .expect("open the app database")
            .store
            .set_setting_as("ssh_engine", &engine)
            .expect("save ssh_engine");
        let runtime = e2e.dir.path().join("run");
        let server = Server::start_service(&runtime, daemon.clone()).expect("start server");
        tokio::spawn(async move {
            let _ = server.serve(std::future::pending::<()>()).await;
        });

        let answerer = Arc::new(Answerer {
            link: Mutex::new(None),
        });
        let link = Link::connect(&runtime, answerer.clone())
            .await
            .expect("connect to the daemon");
        *answerer.link.lock().unwrap() = Some(link.clone());

        let mut saved = HashMap::<(String, bool), Connection>::new();
        emit(json!({"op": "ready"}));

        let mut lines = BufReader::new(tokio::io::stdin()).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let text = |key: &str| message[key].as_str().unwrap_or_default().to_owned();
            let number = |key: &str, default: u64| message[key].as_u64().unwrap_or(default) as u16;
            match message["op"].as_str().unwrap_or_default() {
                "open" => {
                    let host = message["host"].as_str().unwrap_or("e2e-target").to_owned();
                    let seq = message["seq"].clone();
                    let connection = match saved_connection(
                        &link,
                        &mut saved,
                        &host,
                        message["integration"] == true,
                    )
                    .await
                    {
                        Ok(connection) => connection,
                        Err(message) => {
                            emit(json!({"op": "error", "seq": seq, "message": message}));
                            continue;
                        }
                    };
                    let opened_id = Arc::new(Mutex::new(None::<String>));
                    let early = Arc::new(Mutex::new(Vec::<SessionMsg>::new()));
                    let (sink_id, sink_early) = (opened_id.clone(), early.clone());
                    let sink = Box::new(move |message: SessionMsg| {
                        let id = sink_id.lock().unwrap().clone();
                        match id {
                            Some(id) => forward(&id, message),
                            None => sink_early.lock().unwrap().push(message),
                        }
                        true
                    });
                    // A hung open would block every later message (this loop
                    // is serial), so it is reported as an error instead.
                    let opened = tokio::time::timeout(
                        Duration::from_secs(30),
                        link.open_session(
                            Some(&connection.id),
                            number("cols", 80),
                            number("rows", 24),
                            sink,
                        ),
                    )
                    .await;
                    let opened = match opened {
                        Ok(opened) => opened.map_err(|error| error.to_string()),
                        Err(_) => {
                            eprintln!("web_bridge: session open timed out after 30 s");
                            Err("session open timed out after 30 s".to_owned())
                        }
                    };
                    match opened {
                        Ok(id) => {
                            emit(json!({"op": "opened", "seq": seq, "id": id}));
                            let pending: Vec<_> = early.lock().unwrap().drain(..).collect();
                            for message in pending {
                                forward(&id, message);
                            }
                            *opened_id.lock().unwrap() = Some(id);
                        }
                        Err(error) => emit(json!({"op": "error", "seq": seq, "message": error})),
                    }
                }
                "write" => {
                    let _ = link.write_session(&text("id"), &text("data"));
                }
                "resize" => {
                    let _ =
                        link.resize_session(&text("id"), number("cols", 80), number("rows", 24));
                }
                "close" => link.close_session(&text("id")),
                "request" => {
                    let seq = message["seq"].clone();
                    let mut payload = message["payload"].clone();
                    if let Some(host) = message["host"].as_str() {
                        match saved_connection(
                            &link,
                            &mut saved,
                            host,
                            message["integration"] == true,
                        )
                        .await
                        {
                            Ok(connection) => {
                                if payload["tunnel"].is_object() {
                                    payload["tunnel"]["host_id"] = json!(connection.id);
                                } else {
                                    payload["host_id"] = json!(connection.id);
                                }
                            }
                            Err(error) => {
                                emit(json!({"op": "reply", "seq": seq, "error": error}));
                                continue;
                            }
                        }
                    }
                    let (link, kind) = (link.clone(), text("kind"));
                    tokio::spawn(async move {
                        match link.request(&kind, payload).await {
                            Ok(value) => emit(json!({"op": "reply", "seq": seq, "value": value})),
                            Err(error) => {
                                emit(json!({"op": "reply", "seq": seq, "error": error.message}))
                            }
                        }
                    });
                }
                _ => {}
            }
        }
        answerer.link.lock().unwrap().take();
        daemon.shutdown();
    }

    fn forward(id: &str, message: SessionMsg) {
        match message {
            SessionMsg::Data { data, .. } => emit(json!({"op": "data", "id": id, "data": data})),
            SessionMsg::Exit { code, reason } => emit(json!({
                "op": "exit",
                "id": id,
                "code": code,
                "reason": reason.kind(),
                "signal": reason.signal(),
            })),
        }
    }
}

#[cfg(unix)]
#[tokio::main]
async fn main() {
    bridge::run().await;
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the web bridge needs the Linux e2e containers");
}
