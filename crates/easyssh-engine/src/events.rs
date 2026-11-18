//! How the engine tells the UI about things that happen: a question to
//! answer, transfer progress, a tunnel coming up.
//!
//! The desktop daemon implements [`EventSink`] by sending the event to every
//! attached client; the mobile app implements it by emitting a Tauri event.
//! Event names are the protocol's (`hostkey.prompt`, `transfer.progress`,
//! `tunnel.status`, ...), payloads are the protocol's JSON.

use serde_json::Value;
use std::sync::Arc;

/// Where engine events go.
pub trait EventSink: Send + Sync + 'static {
    /// Sends `event` and returns how many listeners took it. Zero means
    /// nobody is attached (the engine then does not wait for an answer).
    fn emit(&self, event: &str, payload: Value) -> usize;
}

impl<F> EventSink for F
where
    F: Fn(&str, Value) -> usize + Send + Sync + 'static,
{
    fn emit(&self, event: &str, payload: Value) -> usize {
        self(event, payload)
    }
}

/// A shared sink.
pub type Events = Arc<dyn EventSink>;

/// A sink that drops everything (tests, headless use).
pub fn no_events() -> Events {
    Arc::new(|_: &str, _: Value| 0usize)
}

/// The name a UI event listener uses for a protocol event. Tauri event names
/// cannot contain dots, and the question dialogs listen to `hostkey` and
/// `askpass` (the daemon's `hostkey.prompt` and `askpass.prompt`):
/// `hostkey.prompt` becomes `hostkey`, `tunnel.status` becomes `tunnel-status`.
pub fn ui_event_name(event: &str) -> String {
    event
        .strip_suffix(".prompt")
        .unwrap_or(event)
        .replace('.', "-")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn a_closure_is_a_sink_and_reports_who_took_the_event() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        let sink: Events = Arc::new(move |name: &str, _: Value| {
            log.lock().unwrap().push(name.to_owned());
            2usize
        });
        assert_eq!(sink.emit("tunnel.status", Value::Null), 2);
        assert_eq!(*seen.lock().unwrap(), vec!["tunnel.status"]);
        assert_eq!(no_events().emit("x", Value::Null), 0);
    }

    #[test]
    fn protocol_names_become_ui_event_names() {
        assert_eq!(ui_event_name("hostkey.prompt"), "hostkey");
        assert_eq!(ui_event_name("hostkey.resolved"), "hostkey-resolved");
        assert_eq!(ui_event_name("askpass.prompt"), "askpass");
        assert_eq!(ui_event_name("askpass.resolved"), "askpass-resolved");
        assert_eq!(ui_event_name("tunnel.status"), "tunnel-status");
        assert_eq!(ui_event_name("transfer.progress"), "transfer-progress");
    }
}
