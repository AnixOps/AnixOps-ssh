//! Authenticated local IPC for the EasySSH desktop daemon.
//!
//! See the crate README for the threat model and wire format.

pub mod channel;
mod client;
mod error;
pub mod frame;
mod platform;
pub mod runtime;
mod server;
pub mod session;
pub mod transport;

pub use client::Client;
pub use error::{Error, Result};
pub use runtime::{default_runtime_dir, Paths};
pub use server::{handler, Handler, Server};

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
