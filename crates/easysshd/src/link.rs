//! The client side of the daemon protocol, for the app: one authenticated
//! connection, request/reply correlation, and routing of session events to
//! per-session sinks. It knows nothing about any UI toolkit.
//!
//! Commands are queued in call order, so `post` calls (keystrokes) reach the
//! daemon in the order they were made even from synchronous code.

use crate::channel::{ChannelReader, ChannelWriter};
use crate::Client;
use easyssh_proto as proto;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use tokio::sync::{mpsc, oneshot};

/// Largest `session.write` payload sent in one command.
const WRITE_CHUNK: usize = 64 * 1024;

#[derive(Debug, Clone)]
pub struct LinkError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LinkError {}

fn err(code: &str, message: impl Into<String>) -> LinkError {
    LinkError {
        code: code.to_owned(),
        message: message.into(),
    }
}

fn disconnected() -> LinkError {
    err("disconnected", "the EasySSH daemon connection was lost")
}

/// What a session sink receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMsg {
    /// Output text starting at stream offset `seq`.
    Data {
        seq: u64,
        data: String,
    },
    Exit {
        code: Option<u32>,
    },
}

/// Receives a session's messages; returns `false` when its consumer is gone,
/// which drops the sink (the session keeps running in the daemon).
pub type SessionSink = Box<dyn FnMut(SessionMsg) -> bool + Send>;

#[derive(Debug, Clone)]
pub struct AskpassPrompt {
    pub id: u64,
    pub host: String,
    pub prompt: String,
    pub confirm: bool,
}

pub trait LinkHandler: Send + Sync + 'static {
    /// OpenSSH asked for a password, passphrase or confirmation.
    fn askpass_prompt(&self, prompt: AskpassPrompt);
    /// The connection to the daemon ended. Every sink has received `Exit`.
    fn lost(&self) {}
}

type Reply = Result<Value, LinkError>;

struct Pending {
    reply: oneshot::Sender<Reply>,
    /// A sink to install, atomically with the reply, for the session named
    /// here or (when `None`) in the reply's `session_id`.
    install: Option<(Option<String>, SessionSink)>,
}

struct Inner {
    tx: mpsc::UnboundedSender<Value>,
    pending: Mutex<HashMap<String, Pending>>,
    sinks: Mutex<HashMap<String, SessionSink>>,
    alive: AtomicBool,
    askpass_endpoint: OnceLock<String>,
    handler: Arc<dyn LinkHandler>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Inner {
    fn mark_lost(&self) {
        if !self.alive.swap(false, Ordering::SeqCst) {
            return;
        }
        // Dropping the senders fails every waiting request.
        lock(&self.pending).clear();
        let sinks: Vec<_> = lock(&self.sinks).drain().collect();
        for (_, mut sink) in sinks {
            sink(SessionMsg::Exit { code: None });
        }
        self.handler.lost();
    }

    fn route(&self, msg: Value) {
        if msg.get("kind").is_some() {
            if let Err(errors) = proto::validate_event(&msg) {
                eprintln!(
                    "easysshd link: dropping invalid event: {}",
                    errors.join("; ")
                );
                return;
            }
            self.route_event(&msg["kind"], &msg["payload"]);
            return;
        }
        let Some(id) = msg.get("id").and_then(Value::as_str) else {
            return;
        };
        let Some(pending) = lock(&self.pending).remove(id) else {
            return; // a `post`, or already failed
        };
        let result: Reply = match msg.get("ok") {
            Some(ok) => Ok(ok.clone()),
            None => {
                let e = &msg["error"];
                Err(err(
                    e["code"].as_str().unwrap_or("error"),
                    e["message"].as_str().unwrap_or("request failed"),
                ))
            }
        };
        // Install the sink before anything after the reply is routed.
        if let (Ok(ok), Some((sid, sink))) = (&result, pending.install) {
            let sid = sid.or_else(|| ok["session_id"].as_str().map(str::to_owned));
            if let Some(sid) = sid {
                lock(&self.sinks).insert(sid, sink);
            }
        }
        let _ = pending.reply.send(result);
    }

    fn route_event(&self, kind: &Value, payload: &Value) {
        match kind.as_str() {
            Some("session.output") => {
                let Some(sid) = payload["session_id"].as_str() else {
                    return;
                };
                let mut sinks = lock(&self.sinks);
                if let Some(sink) = sinks.get_mut(sid) {
                    let msg = SessionMsg::Data {
                        seq: payload["seq"].as_u64().unwrap_or(0),
                        data: payload["data"].as_str().unwrap_or_default().to_owned(),
                    };
                    if !sink(msg) {
                        sinks.remove(sid);
                    }
                }
            }
            Some("session.exit") => {
                let Some(sid) = payload["session_id"].as_str() else {
                    return;
                };
                if let Some(mut sink) = lock(&self.sinks).remove(sid) {
                    sink(SessionMsg::Exit {
                        code: payload["code"].as_u64().map(|c| c as u32),
                    });
                }
            }
            Some("askpass.prompt") => self.handler.askpass_prompt(AskpassPrompt {
                id: payload["id"].as_u64().unwrap_or(0),
                host: payload["host"].as_str().unwrap_or_default().to_owned(),
                prompt: payload["prompt"].as_str().unwrap_or_default().to_owned(),
                confirm: payload["confirm"].as_bool().unwrap_or(false),
            }),
            _ => {}
        }
    }
}

/// A live connection to the daemon. Cheap to clone.
#[derive(Clone)]
pub struct Link {
    inner: Arc<Inner>,
}

impl Link {
    /// Connect, say hello (checking the schema version) and learn the askpass
    /// endpoint. Must run inside a tokio runtime, which also hosts the
    /// connection's background tasks.
    pub async fn connect(
        runtime_dir: impl AsRef<Path>,
        handler: Arc<dyn LinkHandler>,
    ) -> Result<Link, LinkError> {
        let client = Client::connect(runtime_dir)
            .await
            .map_err(|e| err("connect_failed", e.to_string()))?;
        let (reader, writer) = client.into_split();
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            tx,
            pending: Mutex::new(HashMap::new()),
            sinks: Mutex::new(HashMap::new()),
            alive: AtomicBool::new(true),
            askpass_endpoint: OnceLock::new(),
            handler,
        });
        tokio::spawn(read_loop(inner.clone(), reader));
        tokio::spawn(write_loop(inner.clone(), writer, rx));
        let link = Link { inner };
        let hello = link
            .request(
                "proto.hello",
                json!({"schema_version": proto::SCHEMA_VERSION}),
            )
            .await?;
        if hello["schema_version"].as_u64() != Some(proto::SCHEMA_VERSION as u64) {
            return Err(err(
                "schema_mismatch",
                format!("the daemon speaks schema {}", hello["schema_version"]),
            ));
        }
        let endpoint = link.request("askpass.endpoint", json!({})).await?;
        let _ = link
            .inner
            .askpass_endpoint
            .set(endpoint["endpoint"].as_str().unwrap_or_default().to_owned());
        Ok(link)
    }

    pub fn is_alive(&self) -> bool {
        self.inner.alive.load(Ordering::SeqCst)
    }

    /// `"<port> <token>"` for `ANIXOPS_SSH_ASKPASS`; a secret, for OpenSSH
    /// child processes only.
    pub fn askpass_endpoint(&self) -> &str {
        self.inner.askpass_endpoint.get().map_or("", String::as_str)
    }

    fn build(&self, kind: &str, payload: Value) -> Result<(String, Value), LinkError> {
        if !self.is_alive() {
            return Err(disconnected());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let command = json!({"id": id, "kind": kind, "payload": payload});
        proto::validate_command(&command)
            .map_err(|errors| err("invalid_command", errors.join("; ")))?;
        Ok((id, command))
    }

    async fn send_request(
        &self,
        kind: &str,
        payload: Value,
        install: Option<(Option<String>, SessionSink)>,
    ) -> Reply {
        let (id, command) = self.build(kind, payload)?;
        let (reply, receiver) = oneshot::channel();
        lock(&self.inner.pending).insert(id.clone(), Pending { reply, install });
        if self.inner.tx.send(command).is_err() {
            lock(&self.inner.pending).remove(&id);
            return Err(disconnected());
        }
        // A failed connection drops the sender.
        receiver.await.unwrap_or_else(|_| Err(disconnected()))
    }

    /// Send a command and wait for its reply.
    pub async fn request(&self, kind: &str, payload: Value) -> Reply {
        self.send_request(kind, payload, None).await
    }

    /// Queue a command without waiting for the reply (its outcome is ignored).
    /// Calls are delivered in order. For keystrokes and resizes.
    pub fn post(&self, kind: &str, payload: Value) -> Result<(), LinkError> {
        let (_, command) = self.build(kind, payload)?;
        self.inner.tx.send(command).map_err(|_| disconnected())
    }

    /// `session.open`; the sink receives the session's output from the start.
    pub async fn open_session(
        &self,
        host_id: Option<&str>,
        cols: u16,
        rows: u16,
        sink: SessionSink,
    ) -> Result<String, LinkError> {
        let reply = self
            .send_request(
                "session.open",
                json!({"host_id": host_id, "cols": cols, "rows": rows}),
                Some((None, sink)),
            )
            .await?;
        Ok(reply["session_id"].as_str().unwrap_or_default().to_owned())
    }

    /// `session.attach`: the sink receives the buffered output from
    /// `from_seq` (everything, when `None`), then live output, replacing any
    /// sink this link already had for the session.
    pub async fn attach_session(
        &self,
        session_id: &str,
        from_seq: Option<u64>,
        sink: SessionSink,
    ) -> Result<proto::SessionAttachResult, LinkError> {
        let mut payload = json!({"session_id": session_id});
        if let Some(from) = from_seq {
            payload["from_seq"] = json!(from);
        }
        // Whatever the old sink was owed is superseded by the replay.
        lock(&self.inner.sinks).remove(session_id);
        let reply = self
            .send_request(
                "session.attach",
                payload,
                Some((Some(session_id.to_owned()), sink)),
            )
            .await?;
        serde_json::from_value(reply).map_err(|e| err("invalid_reply", e.to_string()))
    }

    pub async fn list_sessions(&self) -> Result<Vec<proto::SessionInfo>, LinkError> {
        let reply = self.request("session.list", json!({})).await?;
        serde_json::from_value::<proto::SessionListResult>(reply)
            .map(|r| r.sessions)
            .map_err(|e| err("invalid_reply", e.to_string()))
    }

    /// Stop receiving a session's events here; the session keeps running.
    pub fn detach_session(&self, session_id: &str) {
        lock(&self.inner.sinks).remove(session_id);
        let _ = self.post("session.detach", json!({"session_id": session_id}));
    }

    /// Kill and forget a session.
    pub fn close_session(&self, session_id: &str) {
        lock(&self.inner.sinks).remove(session_id);
        let _ = self.post("session.close", json!({"session_id": session_id}));
    }

    pub fn write_session(&self, session_id: &str, data: &str) -> Result<(), LinkError> {
        let mut rest = data;
        loop {
            let mut end = rest.len().min(WRITE_CHUNK);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            let (chunk, tail) = rest.split_at(end);
            self.post(
                "session.write",
                json!({"session_id": session_id, "data": chunk}),
            )?;
            if tail.is_empty() {
                return Ok(());
            }
            rest = tail;
        }
    }

    pub fn resize_session(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), LinkError> {
        self.post(
            "session.resize",
            json!({"session_id": session_id, "cols": cols, "rows": rows}),
        )
    }

    /// `askpass.reply`; `None` cancels the prompt.
    pub fn askpass_reply(&self, id: u64, answer: Option<String>) -> Result<(), LinkError> {
        self.post("askpass.reply", json!({"id": id, "answer": answer}))
    }
}

async fn read_loop(inner: Arc<Inner>, mut reader: ChannelReader) {
    while let Ok(msg) = reader.recv().await {
        inner.route(msg);
    }
    inner.mark_lost();
}

async fn write_loop(
    inner: Arc<Inner>,
    mut writer: ChannelWriter,
    mut rx: mpsc::UnboundedReceiver<Value>,
) {
    while let Some(msg) = rx.recv().await {
        if writer.send(&msg).await.is_err() {
            break;
        }
    }
    inner.mark_lost();
}
