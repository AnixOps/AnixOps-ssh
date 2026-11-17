//! Sending: the transport, the consent gate, the rate limit and back-off.

use crate::dsn::Dsn;
use crate::envelope;
use crate::spool::Spool;
use crate::Consent;
use std::time::Duration;

/// At most this many reports leave in a rolling 24 hours...
pub const MAX_PER_DAY: usize = 10;
/// ...and at most this many in one run.
pub const MAX_PER_RUN: usize = 5;
/// After a failure nothing is sent for this long.
pub const BACKOFF_SECS: i64 = 3600;

pub struct Request<'a> {
    pub url: &'a str,
    pub content_type: &'a str,
    pub auth: Option<String>,
    pub body: &'a [u8],
}

/// One HTTP `POST`; the status code, or a message when there was none.
pub trait Transport {
    fn post(&self, request: &Request) -> Result<u16, String>;
}

/// `ureq` with short timeouts and no redirects (a redirect could send the
/// report somewhere the build did not name).
pub struct HttpTransport;

impl Transport for HttpTransport {
    fn post(&self, request: &Request) -> Result<u16, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirects(0)
            .user_agent(concat!("easyssh-telemetry/", env!("CARGO_PKG_VERSION")))
            .build();
        let mut call = agent
            .post(request.url)
            .set("Content-Type", request.content_type);
        if let Some(auth) = &request.auth {
            call = call.set("X-Sentry-Auth", auth);
        }
        match call.send_bytes(request.body) {
            Ok(response) => Ok(response.status()),
            Err(ureq::Error::Status(code, _)) => Ok(code),
            Err(error) => Err(error.kind().to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skipped {
    NoDsn,
    NoConsent,
    Backoff,
    RateLimited,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Outcome {
    pub sent: usize,
    pub failed: usize,
    pub dropped: usize,
    pub skipped: Option<Skipped>,
}

impl Outcome {
    fn skipped(why: Skipped) -> Outcome {
        Outcome {
            sent: 0,
            failed: 0,
            dropped: 0,
            skipped: Some(why),
        }
    }
}

/// Uploads pending reports. Without a DSN, or without consent, nothing is
/// read, built or sent.
pub fn upload_pending(
    spool: &Spool,
    dsn: Option<&Dsn>,
    consent: &Consent,
    transport: &dyn Transport,
    now: i64,
) -> Outcome {
    let Some(dsn) = dsn else {
        return Outcome::skipped(Skipped::NoDsn);
    };
    if !consent.crash_reports {
        return Outcome::skipped(Skipped::NoConsent);
    }
    spool.recover_stale();
    let mut state = spool.load_state();
    if now < state.retry_after {
        return Outcome::skipped(Skipped::Backoff);
    }
    state.sent.retain(|t| now - t < 24 * 3600);
    let mut out = Outcome {
        sent: 0,
        failed: 0,
        dropped: 0,
        skipped: None,
    };
    let url = dsn.envelope_url();
    for entry in spool.list() {
        if state.sent.len() >= MAX_PER_DAY || out.sent >= MAX_PER_RUN {
            out.skipped = Some(Skipped::RateLimited);
            break;
        }
        let Some(claimed) = spool.claim(&entry.path) else {
            continue;
        };
        let body = envelope::build(&entry.report, Some(dsn), now);
        let request = Request {
            url: &url,
            content_type: envelope::CONTENT_TYPE,
            auth: Some(dsn.auth_header()),
            body: &body,
        };
        match transport.post(&request) {
            Ok(200..=299) => {
                spool.finish(&claimed);
                state.sent.push(now);
                out.sent += 1;
            }
            // The server refused this report for good; sending it again won't help.
            Ok(code) if (400..500).contains(&code) && code != 429 => {
                spool.finish(&claimed);
                out.dropped += 1;
            }
            Ok(_) | Err(_) => {
                spool.release(&claimed);
                state.retry_after = now + BACKOFF_SECS;
                out.failed += 1;
                break;
            }
        }
    }
    spool.save_state(&state);
    out
}
