//! The Sentry envelope: three newline-separated JSON documents (envelope
//! header, item header, event). Written by hand; the format is small and
//! stable, and an SDK would bring its own idea of what to collect.

use crate::dsn::Dsn;
use crate::report::Report;
use serde_json::{json, Value};

pub const CONTENT_TYPE: &str = "application/x-sentry-envelope";

/// The Sentry event for a report. Only fields of the report are used: no
/// `server_name`, no `user`, no request, no environment variables, no modules.
pub fn event(report: &Report) -> Value {
    let frames: Vec<Value> = report
        .frames
        .iter()
        .rev() // Sentry lists the oldest call first
        .map(|f| json!({ "function": f }))
        .collect();
    json!({
        "event_id": report.id,
        "timestamp": report.timestamp,
        "platform": "native",
        "level": "fatal",
        "logger": report.component,
        "release": format!("anixops-ssh@{}", report.version),
        "sdk": { "name": "easyssh-telemetry", "version": env!("CARGO_PKG_VERSION") },
        "tags": { "component": report.component, "os": report.os, "arch": report.arch },
        "contexts": { "os": { "name": report.os }, "device": { "arch": report.arch } },
        "exception": { "values": [{
            "type": report.kind,
            "value": report.message,
            "mechanism": { "type": report.kind, "handled": false },
            "stacktrace": { "frames": frames },
        }]},
    })
}

/// The bytes posted for one report.
pub fn build(report: &Report, dsn: Option<&Dsn>, sent_at: i64) -> Vec<u8> {
    let payload = event(report).to_string();
    let sent_at = chrono::DateTime::from_timestamp(sent_at, 0)
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default();
    let mut header = json!({ "event_id": report.id, "sent_at": sent_at });
    if let Some(dsn) = dsn {
        header["dsn"] = json!(dsn.as_str());
    }
    let item = json!({ "type": "event", "length": payload.len() });
    format!("{header}\n{item}\n{payload}\n").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        Report::new(
            "app",
            "panic",
            "1.0.2",
            "boom",
            ["inner".to_owned(), "outer".to_owned()],
        )
    }

    #[test]
    fn three_documents_and_a_matching_length() {
        let dsn = Dsn::parse("https://k@gt.example.org/3").unwrap();
        let bytes = build(&report(), Some(&dsn), 1_700_000_000);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.ends_with('\n'));
        let lines: Vec<&str> = text.trim_end_matches('\n').split('\n').collect();
        assert_eq!(lines.len(), 3);
        let header: Value = serde_json::from_str(lines[0]).unwrap();
        let item: Value = serde_json::from_str(lines[1]).unwrap();
        let event: Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(header["event_id"], event["event_id"]);
        assert_eq!(header["dsn"], "https://k@gt.example.org/3");
        assert_eq!(header["sent_at"], "2023-11-14T22:13:20Z");
        assert_eq!(item["type"], "event");
        assert_eq!(item["length"], lines[2].len());
        assert_eq!(event["exception"]["values"][0]["value"], "boom");
        assert_eq!(event["release"], "anixops-ssh@1.0.2");
    }

    #[test]
    fn oldest_frame_first_and_nothing_extra() {
        let e = event(&report());
        let frames = &e["exception"]["values"][0]["stacktrace"]["frames"];
        assert_eq!(frames[0]["function"], "outer");
        assert_eq!(frames[1]["function"], "inner");
        for banned in ["server_name", "user", "request", "extra", "modules"] {
            assert!(e.get(banned).is_none(), "{banned}");
        }
    }
}
