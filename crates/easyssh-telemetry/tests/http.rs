//! Talks to a mock HTTP server on 127.0.0.1 only; nothing leaves the machine.

use easyssh_telemetry::report::now;
use easyssh_telemetry::spool::Spool;
use easyssh_telemetry::upload::{upload_pending, Request, Skipped, Transport, MAX_PER_RUN};
use easyssh_telemetry::{usage, Consent, Dsn, HttpTransport, Report};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
struct Captured {
    request_line: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

struct Mock {
    port: u16,
    seen: Arc<Mutex<Vec<Captured>>>,
}

/// Answers every request with the next status of `statuses` (the last repeats).
fn mock(statuses: Vec<u16>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Arc<Mutex<Vec<Captured>>> = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for (n, stream) in listener.incoming().enumerate() {
            let Ok(mut stream) = stream else { break };
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            let head_end = loop {
                let k = stream.read(&mut chunk).unwrap_or(0);
                if k == 0 {
                    break None;
                }
                buf.extend_from_slice(&chunk[..k]);
                if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(i + 4);
                }
            };
            let Some(head_end) = head_end else { continue };
            let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
            let mut lines = head.split("\r\n");
            let request_line = lines.next().unwrap_or("").to_owned();
            let headers: HashMap<String, String> = lines
                .filter_map(|l| l.split_once(':'))
                .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                .collect();
            let len: usize = headers
                .get("content-length")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            while buf.len() < head_end + len {
                let k = stream.read(&mut chunk).unwrap_or(0);
                if k == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..k]);
            }
            log.lock().unwrap().push(Captured {
                request_line,
                headers,
                body: buf[head_end..].to_vec(),
            });
            let status = statuses.get(n).or(statuses.last()).copied().unwrap_or(200);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
            );
        }
    });
    Mock { port, seen }
}

fn consent_on() -> Consent {
    Consent {
        crash_reports: true,
        usage_stats: false,
    }
}

fn spool_with(n: usize) -> (tempfile::TempDir, Spool) {
    let dir = tempfile::tempdir().unwrap();
    let spool = Spool::new(dir.path());
    for i in 0..n {
        let mut r = Report::new(
            "app",
            "panic",
            "1.0.2",
            &format!("boom {i}"),
            ["a::b".to_owned()],
        );
        r.timestamp = now() - 100 + i as i64;
        spool.write(&r).unwrap();
    }
    (dir, spool)
}

fn dsn(port: u16) -> Dsn {
    Dsn::parse(&format!("http://pubkey@127.0.0.1:{port}/42")).unwrap()
}

#[test]
fn posts_a_well_formed_envelope() {
    let server = mock(vec![200]);
    let (_d, spool) = spool_with(1);
    let out = upload_pending(
        &spool,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        now(),
    );
    assert_eq!((out.sent, out.failed, out.skipped), (1, 0, None));
    assert!(spool.list().is_empty(), "sent report is removed");

    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let req = &seen[0];
    assert_eq!(req.request_line, "POST /api/42/envelope/ HTTP/1.1");
    assert_eq!(req.headers["content-type"], "application/x-sentry-envelope");
    assert!(req.headers["x-sentry-auth"].contains("sentry_key=pubkey"));
    assert!(req.headers["x-sentry-auth"].contains("sentry_version=7"));

    let text = String::from_utf8(req.body.clone()).unwrap();
    let lines: Vec<&str> = text.trim_end().split('\n').collect();
    assert_eq!(lines.len(), 3);
    let header: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    let item: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    let event: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
    assert_eq!(header["event_id"], event["event_id"]);
    assert_eq!(header["event_id"].as_str().unwrap().len(), 32);
    assert_eq!(item["type"], "event");
    assert_eq!(item["length"].as_u64().unwrap() as usize, lines[2].len());
    assert_eq!(event["exception"]["values"][0]["value"], "boom 0");
    assert_eq!(event["tags"]["os"], std::env::consts::OS);
}

#[test]
fn no_upload_without_consent() {
    let server = mock(vec![200]);
    let (_d, spool) = spool_with(2);
    let off = Consent {
        crash_reports: false,
        usage_stats: true,
    };
    let out = upload_pending(&spool, Some(&dsn(server.port)), &off, &HttpTransport, now());
    assert_eq!(out.skipped, Some(Skipped::NoConsent));
    assert!(server.seen.lock().unwrap().is_empty());
    assert_eq!(spool.list().len(), 2, "reports stay until the user decides");
}

struct Panics;
impl Transport for Panics {
    fn post(&self, _: &Request) -> Result<u16, String> {
        panic!("the transport must not be called");
    }
}

#[test]
fn no_upload_without_a_dsn() {
    let (_d, spool) = spool_with(2);
    let out = upload_pending(&spool, None, &consent_on(), &Panics, now());
    assert_eq!(out.skipped, Some(Skipped::NoDsn));
    assert_eq!(spool.list().len(), 2);
    // The crate itself, built here without the variable, is inert.
    if option_env!("EASYSSH_SENTRY_DSN").is_none() {
        assert!(!easyssh_telemetry::dsn_configured());
    }
}

#[test]
fn offline_keeps_reports_and_backs_off() {
    // Bind and drop a listener to get a port nothing listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (_d, spool) = spool_with(2);
    let t = now();
    let out = upload_pending(&spool, Some(&dsn(port)), &consent_on(), &HttpTransport, t);
    assert_eq!((out.sent, out.failed), (0, 1));
    assert_eq!(spool.list().len(), 2);
    let again = upload_pending(&spool, Some(&dsn(port)), &consent_on(), &Panics, t + 60);
    assert_eq!(again.skipped, Some(Skipped::Backoff));
    // After the back-off a working server gets them.
    let server = mock(vec![200]);
    let later = upload_pending(
        &spool,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        t + 3601,
    );
    assert_eq!(later.sent, 2);
}

#[test]
fn server_errors_keep_the_report_and_client_errors_drop_it() {
    let server = mock(vec![503]);
    let (_d, spool) = spool_with(1);
    let out = upload_pending(
        &spool,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        now(),
    );
    assert_eq!((out.sent, out.failed), (0, 1));
    assert_eq!(spool.list().len(), 1);

    let server = mock(vec![400]);
    let (_d, spool) = spool_with(1);
    let out = upload_pending(
        &spool,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        now(),
    );
    assert_eq!((out.sent, out.dropped), (0, 1));
    assert!(spool.list().is_empty());

    let server = mock(vec![429]);
    let (_d, spool) = spool_with(1);
    let out = upload_pending(
        &spool,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        now(),
    );
    assert_eq!(out.failed, 1);
    assert_eq!(spool.list().len(), 1);
}

#[test]
fn rate_limit_per_run_and_per_day() {
    let server = mock(vec![200]);
    let (_d, spool) = spool_with(8);
    let t = now();
    let out = upload_pending(
        &spool,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        t,
    );
    assert_eq!(out.sent, MAX_PER_RUN);
    assert_eq!(out.skipped, Some(Skipped::RateLimited));
    assert_eq!(spool.list().len(), 3);
    // Second run the same day: the daily budget (10) leaves 5 more.
    let out = upload_pending(
        &spool,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        t + 10,
    );
    assert_eq!(out.sent, 3);
    // Fill the day's budget, then nothing more goes out.
    let (_d2, spool2) = spool_with(3);
    let mut state = spool2.load_state();
    state.sent = vec![t; 10];
    spool2.save_state(&state);
    let out = upload_pending(
        &spool2,
        Some(&dsn(server.port)),
        &consent_on(),
        &Panics,
        t + 20,
    );
    assert_eq!((out.sent, out.skipped), (0, Some(Skipped::RateLimited)));
    // A day later it opens again.
    let out = upload_pending(
        &spool2,
        Some(&dsn(server.port)),
        &consent_on(),
        &HttpTransport,
        t + 25 * 3600,
    );
    assert_eq!(out.sent, 3);
}

#[test]
fn usage_ping_is_gated_bucketed_and_daily() {
    let server = mock(vec![200]);
    let url = format!("http://127.0.0.1:{}/ping", server.port);
    let dir = tempfile::tempdir().unwrap();
    let spool = Spool::new(dir.path());
    let on = Consent {
        crash_reports: false,
        usage_stats: true,
    };
    let off = Consent::default();

    // Off, or no URL: never sent, never counted.
    assert_eq!(
        usage::send_if_due(
            &spool,
            Some(&url),
            &off,
            &Panics,
            "1.0.2",
            "openssh",
            3,
            "2026-11-17"
        ),
        usage::Pinged::Disabled
    );
    assert_eq!(
        usage::send_if_due(
            &spool,
            None,
            &on,
            &Panics,
            "1.0.2",
            "openssh",
            3,
            "2026-11-17"
        ),
        usage::Pinged::Disabled
    );
    usage::record_session(&spool, Some(&url), &off, "2026-11-16");
    assert_eq!(spool.load_state().usage_sessions, 0);

    for _ in 0..7 {
        usage::record_session(&spool, Some(&url), &on, "2026-11-16");
    }
    let sent = usage::send_if_due(
        &spool,
        Some(&url),
        &on,
        &HttpTransport,
        "1.0.2",
        "openssh",
        12,
        "2026-11-17",
    );
    assert_eq!(sent, usage::Pinged::Sent);
    assert_eq!(
        usage::send_if_due(
            &spool,
            Some(&url),
            &on,
            &Panics,
            "1.0.2",
            "openssh",
            12,
            "2026-11-17"
        ),
        usage::Pinged::AlreadySentToday
    );

    let seen = server.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].request_line, "POST /ping HTTP/1.1");
    let body: serde_json::Value = serde_json::from_slice(&seen[0].body).unwrap();
    let mut keys: Vec<&str> = body
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        ["app_version", "arch", "engine", "hosts", "os", "sessions"]
    );
    assert_eq!(body["hosts"], "6-20");
    assert_eq!(body["sessions"], "6-20");
    assert_eq!(body["app_version"], "1.0.2");
    assert!(!seen[0].headers.contains_key("x-sentry-auth"));
}

#[test]
fn buckets() {
    for (n, b) in [
        (0, "0"),
        (1, "1-5"),
        (5, "1-5"),
        (6, "6-20"),
        (20, "6-20"),
        (21, "21+"),
        (5000, "21+"),
    ] {
        assert_eq!(usage::bucket(n), b);
    }
}

#[test]
fn usage_endpoint_must_be_https_or_loopback() {
    let dir = tempfile::tempdir().unwrap();
    let spool = Spool::new(dir.path());
    let on = Consent {
        crash_reports: false,
        usage_stats: true,
    };
    let r = usage::send_if_due(
        &spool,
        Some("http://stats.example.org/p"),
        &on,
        &Panics,
        "1",
        "e",
        0,
        "2026-01-01",
    );
    assert_eq!(r, usage::Pinged::Disabled);
}
