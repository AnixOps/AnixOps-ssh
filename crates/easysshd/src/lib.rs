//! Authenticated local IPC for the EasySSH desktop daemon.
//!
//! See the crate README for the threat model and wire format.

pub mod agent;
pub mod askpass;
pub mod channel;
mod client;
pub mod daemon;
mod error;
pub mod frame;
pub mod integration;
pub mod launch;
pub mod link;
pub mod log;
mod platform;
pub mod recorder;
pub mod runtime;
mod server;
pub mod service;
pub mod session;
pub mod supervisor;
pub mod syncsvc;
pub mod terminal;
pub mod transport;

pub use client::Client;
pub use error::{Error, Result};
pub use runtime::{default_runtime_dir, Paths};
pub use server::{handler, BoxFuture, Handler, Outbox, Server, Service};
pub use service::{Daemon, DaemonConfig, DaemonExtras};
pub use syncsvc::SyncOptions;

/// Built-in handler: `{"type":"ping"}` is answered with `{"type":"pong"}`.
pub fn builtin_handler() -> Handler {
    use serde_json::{json, Value};
    handler(|msg: Value| async move {
        Some(match msg.get("type").and_then(Value::as_str) {
            Some("ping") => json!({"type": "pong"}),
            _ => json!({"type": "error", "message": "unknown message type"}),
        })
    })
}
