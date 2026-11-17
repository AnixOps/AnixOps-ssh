//! Crash reports and usage statistics: the app's side. The work is in
//! `easyssh-telemetry`; this file reads the consent from the settings, wires
//! the panic hook and answers the Privacy section of the settings.
//!
//! Nothing is recorded or sent unless the build names a DSN (or usage URL) and
//! the user said yes. See docs/telemetry.md.

use crate::hosts::load_config;
use easyssh_core::domain::TelemetryPreferences;
use easyssh_telemetry::{envelope, hook, report, usage, Consent, Spool};
use serde::Serialize;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The global SSH engine setting, as the usage ping names it.
fn engine() -> String {
    match load_config()
        .ok()
        .and_then(|c| c.ssh_engine)
        .unwrap_or_default()
    {
        easyssh_core::SshEngine::Openssh => "openssh",
        easyssh_core::SshEngine::Russh => "russh",
    }
    .to_owned()
}
/// Input from the page is cut before scrubbing, whatever it sends.
const MAX_INPUT: usize = 8 * 1024;

fn spool() -> Spool {
    match std::env::var_os("EASYSSH_SPOOL_DIR") {
        Some(dir) => Spool::new(dir),
        None => Spool::new(Spool::default_dir()),
    }
}

/// What may happen given the saved choice and what this build can do.
pub fn consent_from(prefs: &TelemetryPreferences, dsn: bool, usage_url: bool) -> Consent {
    Consent {
        crash_reports: dsn && prefs.crash_reports == Some(true),
        usage_stats: usage_url && prefs.usage_stats,
    }
}

fn current_consent() -> Consent {
    let prefs = load_config().map(|c| c.telemetry).unwrap_or_default();
    consent_from(
        &prefs,
        easyssh_telemetry::dsn_configured(),
        easyssh_telemetry::usage_configured(),
    )
}

/// Installs the panic hook, for the app and for the daemon this executable can
/// become with `--easysshd`.
pub fn init() {
    let component = if std::env::args().nth(1).as_deref() == Some("--easysshd") {
        "daemon"
    } else {
        "app"
    };
    hook::install(component, VERSION, spool());
    hook::set_capture(current_consent().crash_reports);
}

/// Uploads what an earlier run left behind; never waits for the network.
pub fn start_background() {
    let hosts = load_config().map(|c| c.connections.len()).unwrap_or(0);
    easyssh_telemetry::spawn_startup_tasks(
        spool(),
        current_consent(),
        VERSION.to_owned(),
        engine(),
        hosts,
    );
}

/// The settings changed: capture follows the new choice at once.
pub fn apply(prefs: &TelemetryPreferences) {
    let consent = consent_from(
        prefs,
        easyssh_telemetry::dsn_configured(),
        easyssh_telemetry::usage_configured(),
    );
    hook::set_capture(consent.crash_reports);
    if !consent.crash_reports {
        // Declining after the fact also removes what was already waiting.
        spool().clear();
    }
}

pub fn record_session() {
    let consent = current_consent();
    if consent.usage_stats {
        usage::record_session(
            &spool(),
            easyssh_telemetry::build_usage_url(),
            &consent,
            &easyssh_telemetry::today(),
        );
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryStatus {
    /// The build has a DSN: crash reports can be turned on.
    crash_available: bool,
    /// The build has a usage URL: statistics can be turned on.
    usage_available: bool,
    pending: usize,
}

#[derive(Serialize)]
pub struct PendingReport {
    id: String,
    /// The exact bytes that would be posted.
    text: String,
}

#[derive(Serialize)]
pub struct Pending {
    reports: Vec<PendingReport>,
    /// The exact body of the next usage ping, when one is due.
    usage: Option<String>,
}

#[tauri::command]
pub fn telemetry_status() -> TelemetryStatus {
    TelemetryStatus {
        crash_available: easyssh_telemetry::dsn_configured(),
        usage_available: easyssh_telemetry::usage_configured(),
        pending: spool().list().len(),
    }
}

#[tauri::command]
pub fn telemetry_pending() -> Pending {
    let dsn = easyssh_telemetry::build_dsn();
    let now = report::now();
    let reports = spool()
        .list()
        .into_iter()
        .map(|entry| PendingReport {
            text: String::from_utf8_lossy(&envelope::build(&entry.report, dsn.as_ref(), now))
                .into_owned(),
            id: entry.report.id,
        })
        .collect();
    let prefs = load_config().map(|c| c.telemetry).unwrap_or_default();
    // The ping is shown whenever it is switched on, whatever the day's state.
    let consent = consent_from(&prefs, true, true);
    let hosts = load_config().map(|c| c.connections.len()).unwrap_or(0);
    let usage = usage::pending(
        &spool(),
        easyssh_telemetry::build_usage_url(),
        &consent,
        VERSION,
        &engine(),
        hosts,
        &easyssh_telemetry::today(),
    )
    .map(|v| serde_json::to_string_pretty(&v).unwrap_or_default());
    Pending { reports, usage }
}

#[tauri::command]
pub fn telemetry_delete_pending() -> usize {
    spool().clear()
}

/// `window.onerror` and `unhandledrejection` from the page. Scrubbed here.
#[tauri::command]
pub fn telemetry_report_frontend(kind: String, message: String, stack: String) {
    let cut = |s: &str| s.chars().take(MAX_INPUT).collect::<String>();
    hook::capture_frontend(&spool(), VERSION, &cut(&kind), &cut(&message), &cut(&stack));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefs(crash: Option<bool>, usage: bool) -> TelemetryPreferences {
        TelemetryPreferences {
            crash_reports: crash,
            usage_stats: usage,
        }
    }

    #[test]
    fn nothing_is_on_by_default_or_before_the_question_is_answered() {
        assert_eq!(
            consent_from(&TelemetryPreferences::default(), true, true),
            Consent::default()
        );
        assert!(!consent_from(&prefs(None, false), true, true).crash_reports);
    }

    #[test]
    fn a_build_without_endpoints_is_inert() {
        assert_eq!(
            consent_from(&prefs(Some(true), true), false, false),
            Consent::default()
        );
        let c = consent_from(&prefs(Some(true), true), true, false);
        assert!(c.crash_reports && !c.usage_stats);
    }

    #[test]
    fn each_choice_is_independent() {
        let c = consent_from(&prefs(Some(false), true), true, true);
        assert!(!c.crash_reports && c.usage_stats);
    }
}
