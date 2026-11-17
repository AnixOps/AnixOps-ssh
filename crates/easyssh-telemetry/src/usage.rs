//! Anonymous usage statistics: one small JSON document per day, only when the
//! user turned them on and the build names an endpoint.
//!
//! Fields, all of them: `app_version`, `os`, `arch`, `engine`, `hosts` (bucket)
//! and `sessions` (bucket). No identifier of any kind is sent or stored, so
//! two pings cannot be linked to each other or to a person.

use crate::dsn::check_url;
use crate::spool::Spool;
use crate::upload::{Request, Transport};
use crate::Consent;
use serde_json::{json, Value};

/// `0`, `1-5`, `6-20` or `21+`.
pub fn bucket(n: usize) -> &'static str {
    match n {
        0 => "0",
        1..=5 => "1-5",
        6..=20 => "6-20",
        _ => "21+",
    }
}

pub fn payload(version: &str, engine: &str, hosts: usize, sessions: usize) -> Value {
    json!({
        "app_version": version,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "engine": engine,
        "hosts": bucket(hosts),
        "sessions": bucket(sessions),
    })
}

fn enabled(url: Option<&str>, consent: &Consent) -> Option<String> {
    let url = url?;
    (consent.usage_stats && check_url(url).is_ok()).then(|| url.to_owned())
}

/// Counts one opened session for the day, only while usage statistics are on.
pub fn record_session(spool: &Spool, url: Option<&str>, consent: &Consent, today: &str) {
    if enabled(url, consent).is_none() {
        return;
    }
    let mut state = spool.load_state();
    if state.usage_day != today {
        // The count of the finished day waits for the next ping.
        state.usage_prev_sessions = state.usage_sessions;
        state.usage_sessions = 0;
        state.usage_day = today.to_owned();
    }
    state.usage_sessions = state.usage_sessions.saturating_add(1);
    spool.save_state(&state);
}

/// What the next ping would contain, for "View pending reports".
pub fn pending(
    spool: &Spool,
    url: Option<&str>,
    consent: &Consent,
    version: &str,
    engine: &str,
    hosts: usize,
    today: &str,
) -> Option<Value> {
    enabled(url, consent)?;
    let state = spool.load_state();
    (state.usage_last_sent != today)
        .then(|| payload(version, engine, hosts, reported_sessions(&state, today)))
}

/// Sessions of the last finished day: what a ping sends.
fn reported_sessions(state: &crate::spool::State, today: &str) -> usize {
    if state.usage_day != today {
        state.usage_sessions as usize
    } else {
        state.usage_prev_sessions as usize
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Pinged {
    Disabled,
    AlreadySentToday,
    Sent,
    Failed,
}

/// Sends the ping when it is due (at most once per UTC day).
#[allow(clippy::too_many_arguments)]
pub fn send_if_due(
    spool: &Spool,
    url: Option<&str>,
    consent: &Consent,
    transport: &dyn Transport,
    version: &str,
    engine: &str,
    hosts: usize,
    today: &str,
) -> Pinged {
    let Some(url) = enabled(url, consent) else {
        return Pinged::Disabled;
    };
    let mut state = spool.load_state();
    if state.usage_last_sent == today {
        return Pinged::AlreadySentToday;
    }
    let body = payload(version, engine, hosts, reported_sessions(&state, today)).to_string();
    let request = Request {
        url: &url,
        content_type: "application/json",
        auth: None,
        body: body.as_bytes(),
    };
    match transport.post(&request) {
        Ok(200..=299) => {
            if state.usage_day != today {
                state.usage_sessions = 0;
                state.usage_day = today.to_owned();
            }
            state.usage_prev_sessions = 0;
            state.usage_last_sent = today.to_owned();
            spool.save_state(&state);
            Pinged::Sent
        }
        _ => Pinged::Failed,
    }
}
