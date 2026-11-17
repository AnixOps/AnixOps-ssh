//! Opt-in crash reports and anonymous usage statistics for AnixOps SSH.
//!
//! - Crash reports are Sentry envelopes, written by hand ([`envelope`]) and
//!   posted over HTTPS to the DSN named at build time in `EASYSSH_SENTRY_DSN`
//!   (a self-hosted GlitchTip). No DSN compiled in means everything here is
//!   inert and [`dsn_configured`] says so.
//! - A panic hook ([`hook::install`]) and the frontend entry
//!   ([`hook::capture_frontend`]) only write scrubbed reports into the
//!   [`Spool`]. Uploading happens on a later start, on a background thread
//!   ([`spawn_startup_tasks`]), and only with consent.
//! - Everything free-text goes through [`scrub::scrub`].
//! - [`usage`] is a separate, daily, bucketed ping to `EASYSSH_USAGE_URL`.
//!
//! The decision on why this is a small hand-written client and not the Sentry
//! SDK is in the crate README.

pub mod dsn;
pub mod envelope;
pub mod hook;
pub mod report;
pub mod scrub;
pub mod spool;
pub mod upload;
pub mod usage;

pub use dsn::Dsn;
pub use report::Report;
pub use spool::Spool;
pub use upload::{HttpTransport, Transport};

use std::thread::JoinHandle;

/// The DSN compiled in, if any.
pub const BUILD_DSN: Option<&str> = option_env!("EASYSSH_SENTRY_DSN");
/// The usage endpoint compiled in, if any.
pub const BUILD_USAGE_URL: Option<&str> = option_env!("EASYSSH_USAGE_URL");

/// What the user agreed to. The default is nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Consent {
    pub crash_reports: bool,
    pub usage_stats: bool,
}

/// The compiled-in DSN, when it is present and valid.
pub fn build_dsn() -> Option<Dsn> {
    Dsn::parse(BUILD_DSN?).ok()
}

pub fn dsn_configured() -> bool {
    build_dsn().is_some()
}

/// The compiled-in usage URL, when it is present and allowed (HTTPS).
pub fn build_usage_url() -> Option<&'static str> {
    BUILD_USAGE_URL.filter(|u| dsn::check_url(u).is_ok())
}

pub fn usage_configured() -> bool {
    build_usage_url().is_some()
}

/// `YYYY-MM-DD`, UTC.
pub fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Uploads pending reports and, if due, the usage ping, on a background
/// thread: start-up never waits for the network, and a failure is silent.
pub fn spawn_startup_tasks(
    spool: Spool,
    consent: Consent,
    version: String,
    engine: String,
    hosts: usize,
) -> Option<JoinHandle<()>> {
    if !consent.crash_reports && !consent.usage_stats {
        return None;
    }
    std::thread::Builder::new()
        .name("telemetry".into())
        .spawn(move || {
            let transport = HttpTransport;
            let dsn = build_dsn();
            let _ =
                upload::upload_pending(&spool, dsn.as_ref(), &consent, &transport, report::now());
            let _ = usage::send_if_due(
                &spool,
                build_usage_url(),
                &consent,
                &transport,
                &version,
                &engine,
                hosts,
                &today(),
            );
        })
        .ok()
}
