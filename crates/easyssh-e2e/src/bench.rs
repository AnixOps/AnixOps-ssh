//! The pure parts of the benchmark harness (`examples/bench.rs`, described in
//! `docs/benchmarks.md`): settings, statistics, result types and the Markdown
//! and JSON reports. Nothing here touches the network, so it is unit tested
//! without Docker.
//!
//! Conventions, fixed here so every run reads the same way:
//!
//! * median: the middle sample, the mean of the two middle ones for an even
//!   count;
//! * P95: the nearest-rank percentile. Sort ascending, take the sample at
//!   1-based rank `ceil(0.95 * n)`;
//! * a throughput sample is `bytes / seconds` from the start of the transfer
//!   (the connection included) to its confirmed completion.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;

const MIB: f64 = 1024.0 * 1024.0;

// ---------------------------------------------------------------- statistics

/// Median of `samples` (unsorted). `None` for no samples.
pub fn median(samples: &[f64]) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let sorted = sorted(samples);
    let n = sorted.len();
    Some(if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    })
}

/// Nearest-rank P95 of `samples` (unsorted). `None` for no samples.
pub fn p95(samples: &[f64]) -> Option<f64> {
    percentile(samples, 0.95)
}

/// Nearest-rank percentile: the sample at 1-based rank `ceil(p * n)`.
pub fn percentile(samples: &[f64], p: f64) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    let sorted = sorted(samples);
    // The small subtraction keeps 0.95 * 20 (=19.000000000000004 in floating
    // point) at rank 19.
    let rank = (p * sorted.len() as f64 - 1e-9).ceil().max(1.0) as usize;
    Some(sorted[rank.min(sorted.len()) - 1])
}

fn sorted(samples: &[f64]) -> Vec<f64> {
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    sorted
}

// ------------------------------------------------------------------ settings

/// Everything the harness reads from the environment, with the defaults the
/// pull request run uses (the nightly and release runs raise the sizes).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Settings {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key: String,
    /// M1/M2: serial connections per profile.
    pub connections: u32,
    /// M6: file size on the normal profile, MiB.
    pub sftp_mib: u64,
    /// M6: file size on the weak profile, MiB. 1 GiB would take hours at 1 %
    /// loss and 150 ms RTT, see `docs/benchmarks.md`.
    pub sftp_mib_weak: u64,
    pub sftp_repeats: u32,
    /// M5: outages per profile.
    pub outage_runs: u32,
    pub outage_secs: u64,
    /// M3: echo samples.
    pub echo_samples: u32,
    /// M4: sessions, minutes, heartbeat seconds.
    pub soak_sessions: u32,
    pub soak_minutes: u64,
    pub soak_heartbeat_secs: u64,
    /// `e2e/netem.sh`; without it the network profile is not applied and
    /// M5 is skipped.
    pub netem_script: Option<String>,
    /// The container whose network namespace `netem.sh` shapes.
    pub container: String,
    pub cipher: String,
    pub kex: String,
    /// Exit non-zero when a target is missed.
    pub enforce: bool,
}

impl Settings {
    /// Reads settings through `get` (the process environment in the harness,
    /// a map in the tests). Bad numbers are an error, not a silent default.
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        fn num<T: std::str::FromStr>(
            get: &impl Fn(&str) -> Option<String>,
            name: &str,
            default: T,
        ) -> Result<T, String> {
            match get(name).filter(|v| !v.trim().is_empty()) {
                None => Ok(default),
                Some(v) => v
                    .trim()
                    .parse()
                    .map_err(|_| format!("{name}={v:?} is not a valid number")),
            }
        }
        let text = |name: &str, default: &str| {
            get(name)
                .filter(|v| !v.trim().is_empty())
                .unwrap_or_else(|| default.to_owned())
        };
        let settings = Self {
            host: text("BENCH_HOST", "127.0.0.1"),
            port: num(&get, "BENCH_PORT", 2224)?,
            user: text("BENCH_USER", "keyuser"),
            key: text("BENCH_KEY", ""),
            connections: num(&get, "BENCH_CONNECTIONS", 200)?,
            sftp_mib: num(&get, "BENCH_SFTP_MIB", 256)?,
            sftp_mib_weak: num(&get, "BENCH_SFTP_MIB_WEAK", 32)?,
            sftp_repeats: num(&get, "BENCH_SFTP_REPEATS", 5)?,
            outage_runs: num(&get, "BENCH_OUTAGE_RUNS", 30)?,
            outage_secs: num(&get, "BENCH_OUTAGE_SECS", 10)?,
            echo_samples: num(&get, "BENCH_ECHO_SAMPLES", 500)?,
            soak_sessions: num(&get, "BENCH_SOAK_SESSIONS", 25)?,
            soak_minutes: num(&get, "BENCH_SOAK_MINUTES", 60)?,
            soak_heartbeat_secs: num(&get, "BENCH_SOAK_HEARTBEAT_SECS", 30)?,
            netem_script: get("BENCH_NETEM").filter(|v| !v.trim().is_empty()),
            container: text("BENCH_CONTAINER", "easyssh-e2e-bench"),
            cipher: text("BENCH_CIPHER", "chacha20-poly1305@openssh.com"),
            kex: text("BENCH_KEX", "curve25519-sha256"),
            enforce: matches!(get("BENCH_ENFORCE").as_deref(), Some("1" | "true")),
        };
        for (name, value) in [
            ("BENCH_CONNECTIONS", settings.connections as u64),
            ("BENCH_SFTP_MIB", settings.sftp_mib),
            ("BENCH_SFTP_MIB_WEAK", settings.sftp_mib_weak),
            ("BENCH_SFTP_REPEATS", settings.sftp_repeats as u64),
            ("BENCH_OUTAGE_RUNS", settings.outage_runs as u64),
            ("BENCH_ECHO_SAMPLES", settings.echo_samples as u64),
            ("BENCH_SOAK_SESSIONS", settings.soak_sessions as u64),
            ("BENCH_SOAK_MINUTES", settings.soak_minutes),
            ("BENCH_SOAK_HEARTBEAT_SECS", settings.soak_heartbeat_secs),
        ] {
            if value == 0 {
                return Err(format!("{name} must be at least 1"));
            }
        }
        Ok(settings)
    }
}

// ------------------------------------------------------------------ profiles

/// A network profile. `netem_name` is the argument of `e2e/netem.sh`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Profile {
    pub name: &'static str,
    pub rtt_ms: u32,
    /// Per direction; `None` is unlimited.
    pub rate_mbit: Option<u32>,
    /// Per direction.
    pub loss_pct: f64,
    /// The targets of the evaluation report are defined for the normal
    /// profile; the weak one is reported without a verdict.
    pub has_targets: bool,
}

impl Profile {
    pub fn normal() -> Self {
        Self {
            name: "normal",
            rtt_ms: 20,
            rate_mbit: Some(100),
            loss_pct: 0.0,
            has_targets: true,
        }
    }

    pub fn weak() -> Self {
        Self {
            name: "weak",
            rtt_ms: 150,
            rate_mbit: None,
            loss_pct: 1.0,
            has_targets: false,
        }
    }

    pub fn parse(name: &str) -> Result<Self, String> {
        match name.trim() {
            "normal" => Ok(Self::normal()),
            "weak" => Ok(Self::weak()),
            other => Err(format!("unknown profile {other:?} (normal or weak)")),
        }
    }

    pub fn describe(&self) -> String {
        let rate = self
            .rate_mbit
            .map_or("unlimited".to_owned(), |r| format!("{r} Mbit/s each way"));
        format!(
            "RTT {} ms, {rate}, {}% loss each way",
            self.rtt_ms, self.loss_pct
        )
    }
}

// ------------------------------------------------------------------- results

/// M1 and M2 come from the same loop: N serial cold connections, each ending
/// at the first reply of a readiness probe.
#[derive(Debug, Clone, Serialize)]
pub struct Connects {
    pub attempts: u32,
    pub successes: u32,
    /// Attempts that hit the 10 s limit.
    pub timeouts: u32,
    /// Attempts that failed before the limit (refused, auth, wrong reply).
    pub errors: u32,
    /// Seconds from starting the connection to the probe reply, successes only.
    pub ready_secs: Vec<f64>,
    pub median_secs: Option<f64>,
    pub p95_secs: Option<f64>,
    /// The first few failure messages.
    pub failures: Vec<String>,
}

impl Connects {
    pub fn new(
        attempts: u32,
        timeouts: u32,
        errors: u32,
        ready_secs: Vec<f64>,
        failures: Vec<String>,
    ) -> Self {
        Self {
            attempts,
            successes: ready_secs.len() as u32,
            timeouts,
            errors,
            median_secs: median(&ready_secs),
            p95_secs: p95(&ready_secs),
            ready_secs,
            failures,
        }
    }

    pub fn success_rate(&self) -> f64 {
        if self.attempts == 0 {
            0.0
        } else {
            self.successes as f64 / self.attempts as f64
        }
    }
}

/// M3, engine level: a byte written to the PTY channel until the terminal's
/// echo of it arrives. No rendering, no UI.
#[derive(Debug, Clone, Serialize)]
pub struct Echo {
    pub requested: u32,
    pub echo_secs: Vec<f64>,
    /// Samples whose echo did not arrive within the limit.
    pub missing: u32,
    pub median_secs: Option<f64>,
    pub p95_secs: Option<f64>,
}

impl Echo {
    pub fn new(requested: u32, echo_secs: Vec<f64>, missing: u32) -> Self {
        Self {
            requested,
            missing,
            median_secs: median(&echo_secs),
            p95_secs: p95(&echo_secs),
            echo_secs,
        }
    }
}

/// M5: reconnect after an outage.
#[derive(Debug, Clone, Serialize)]
pub struct Outage {
    pub runs: u32,
    pub outage_secs: u64,
    pub window_secs: u64,
    pub recovered: u32,
    /// Per run: seconds from the network's return to the first reply on a
    /// new session, `None` when it did not come within the window.
    pub recovery_secs: Vec<Option<f64>>,
    /// Runs in which the old session was still usable after the outage
    /// (information only; the metric is about a new session).
    pub old_session_survived: u32,
    pub median_recovery_secs: Option<f64>,
}

impl Outage {
    pub fn new(
        runs: u32,
        outage_secs: u64,
        window_secs: u64,
        recovery_secs: Vec<Option<f64>>,
        old_session_survived: u32,
    ) -> Self {
        let done: Vec<f64> = recovery_secs.iter().flatten().copied().collect();
        Self {
            runs,
            outage_secs,
            window_secs,
            recovered: done.len() as u32,
            median_recovery_secs: median(&done),
            recovery_secs,
            old_session_survived,
        }
    }

    /// The target is 29 of 30; for another run count it scales, rounded up.
    pub fn required(&self) -> u32 {
        (self.runs as f64 * 29.0 / 30.0).ceil() as u32
    }
}

/// One tool, one direction of M6.
#[derive(Debug, Clone, Serialize)]
pub struct Throughput {
    pub bytes_per_sec: Vec<f64>,
    pub median_bytes_per_sec: Option<f64>,
}

impl Throughput {
    pub fn new(bytes_per_sec: Vec<f64>) -> Self {
        Self {
            median_bytes_per_sec: median(&bytes_per_sec),
            bytes_per_sec,
        }
    }
}

/// M6: SFTP throughput, the engine against OpenSSH `sftp`.
#[derive(Debug, Clone, Serialize)]
pub struct Sftp {
    pub size_bytes: u64,
    pub repeats: u32,
    pub sha256: String,
    pub engine_upload: Throughput,
    pub engine_download: Throughput,
    pub openssh_upload: Throughput,
    pub openssh_download: Throughput,
    /// Every transferred file had the expected SHA-256 (the harness stops
    /// with an error otherwise; this stays true in a finished report).
    pub sha256_verified: bool,
    pub upload_ratio: Option<f64>,
    pub download_ratio: Option<f64>,
}

impl Sftp {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        size_bytes: u64,
        repeats: u32,
        sha256: String,
        engine_upload: Vec<f64>,
        engine_download: Vec<f64>,
        openssh_upload: Vec<f64>,
        openssh_download: Vec<f64>,
    ) -> Self {
        let engine_upload = Throughput::new(engine_upload);
        let engine_download = Throughput::new(engine_download);
        let openssh_upload = Throughput::new(openssh_upload);
        let openssh_download = Throughput::new(openssh_download);
        let ratio =
            |a: &Throughput, b: &Throughput| match (a.median_bytes_per_sec, b.median_bytes_per_sec)
            {
                (Some(a), Some(b)) if b > 0.0 => Some(a / b),
                _ => None,
            };
        Self {
            size_bytes,
            repeats,
            sha256,
            upload_ratio: ratio(&engine_upload, &openssh_upload),
            download_ratio: ratio(&engine_download, &openssh_download),
            engine_upload,
            engine_download,
            openssh_upload,
            openssh_download,
            sha256_verified: true,
        }
    }
}

/// Everything measured on one profile. A metric that was not asked for, or
/// could not run, is `None` with its reason in `skipped`.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileResult {
    pub profile: Profile,
    pub connects: Option<Connects>,
    pub echo: Option<Echo>,
    pub outage: Option<Outage>,
    pub sftp: Option<Sftp>,
    pub skipped: BTreeMap<String, String>,
}

impl ProfileResult {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            connects: None,
            echo: None,
            outage: None,
            sftp: None,
            skipped: BTreeMap::new(),
        }
    }
}

/// M4: many long-lived sessions.
#[derive(Debug, Clone, Serialize)]
pub struct Soak {
    pub profile: String,
    pub sessions: u32,
    pub duration_secs: u64,
    pub heartbeat_secs: u64,
    pub heartbeats_ok: u64,
    pub unexpected_disconnects: u32,
    pub session_hours: f64,
    pub disconnects_per_100_session_hours: f64,
    /// Session index, seconds since the start, reason.
    pub events: Vec<(u32, f64, String)>,
}

impl Soak {
    pub fn new(
        profile: &str,
        sessions: u32,
        duration_secs: u64,
        heartbeat_secs: u64,
        heartbeats_ok: u64,
        events: Vec<(u32, f64, String)>,
    ) -> Self {
        let session_hours = sessions as f64 * duration_secs as f64 / 3600.0;
        let unexpected_disconnects = events.len() as u32;
        Self {
            profile: profile.to_owned(),
            sessions,
            duration_secs,
            heartbeat_secs,
            heartbeats_ok,
            unexpected_disconnects,
            session_hours,
            disconnects_per_100_session_hours: if session_hours > 0.0 {
                unexpected_disconnects as f64 * 100.0 / session_hours
            } else {
                0.0
            },
            events,
        }
    }
}

/// What the run was measured on.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Environment {
    pub commit: String,
    pub engine_version: String,
    pub client_openssh: String,
    pub server_banner: String,
    pub cipher: String,
    pub kex: String,
    pub cpu_model: String,
    pub cpu_count: usize,
    pub kernel: String,
    pub network_shaping: String,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub schema: u32,
    pub generated_at_unix: u64,
    pub environment: Environment,
    pub settings: Settings,
    pub profiles: Vec<ProfileResult>,
    pub soak: Option<Soak>,
}

impl Report {
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("report serialises") + "\n"
    }
}

// ------------------------------------------------------------------- verdict

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Miss,
    /// Reported without a target (weak profile, M3, M4).
    Info,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Miss => "MISS",
            Verdict::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub profile: String,
    pub metric: String,
    pub value: String,
    pub target: String,
    pub verdict: Verdict,
}

fn secs(value: Option<f64>) -> String {
    value.map_or("n/a".to_owned(), |v| format!("{v:.3} s"))
}

fn mib_s(value: Option<f64>) -> String {
    value.map_or("n/a".to_owned(), |v| format!("{:.1} MiB/s", v / MIB))
}

fn pct(value: Option<f64>) -> String {
    value.map_or("n/a".to_owned(), |v| format!("{:.1}%", v * 100.0))
}

fn judge(has_targets: bool, ok: bool) -> Verdict {
    match (has_targets, ok) {
        (false, _) => Verdict::Info,
        (true, true) => Verdict::Pass,
        (true, false) => Verdict::Miss,
    }
}

/// The table rows of one profile, in metric order.
pub fn rows(result: &ProfileResult) -> Vec<Row> {
    let profile = &result.profile;
    let name = profile.name.to_owned();
    let t = profile.has_targets;
    let mut rows = Vec::new();
    let mut push = |metric: &str, value: String, target: &str, verdict: Verdict| {
        rows.push(Row {
            profile: name.clone(),
            metric: metric.to_owned(),
            value,
            target: target.to_owned(),
            verdict,
        });
    };
    if let Some(c) = &result.connects {
        push(
            "M1 connection success rate",
            format!(
                "{}/{} = {} ({} timeouts, {} errors)",
                c.successes,
                c.attempts,
                pct(Some(c.success_rate())),
                c.timeouts,
                c.errors
            ),
            ">= 99.5%",
            judge(t, c.success_rate() >= 0.995),
        );
        push(
            "M2 cold connect ready time, median",
            secs(c.median_secs),
            "report only",
            Verdict::Info,
        );
        push(
            "M2 cold connect ready time, P95",
            format!("{} ({} timeouts)", secs(c.p95_secs), c.timeouts),
            "<= 1.5 s",
            judge(t, c.p95_secs.is_some_and(|v| v <= 1.5)),
        );
    }
    if let Some(e) = &result.echo {
        push(
            "M3 echo latency (engine level), median",
            secs(e.median_secs),
            "report only",
            Verdict::Info,
        );
        push(
            "M3 echo latency (engine level), P95",
            format!("{} ({} missing)", secs(e.p95_secs), e.missing),
            "report only",
            Verdict::Info,
        );
    }
    if let Some(o) = &result.outage {
        push(
            "M5 reconnect after a network outage",
            format!(
                "{}/{} within {} s after a {} s outage (median {}; old session survived {}x)",
                o.recovered,
                o.runs,
                o.window_secs,
                o.outage_secs,
                secs(o.median_recovery_secs),
                o.old_session_survived
            ),
            &format!(">= {}/{}", o.required(), o.runs),
            judge(t, o.recovered >= o.required()),
        );
    }
    if let Some(s) = &result.sftp {
        let size = format!("{} MiB x{}", s.size_bytes / (1024 * 1024), s.repeats);
        let direction = |label: &str, engine: &Throughput, openssh: &Throughput, ratio| {
            (
                format!("M6 SFTP {label} ({size})"),
                format!(
                    "engine {} vs OpenSSH {}: ratio {}",
                    mib_s(engine.median_bytes_per_sec),
                    mib_s(openssh.median_bytes_per_sec),
                    pct(ratio)
                ),
                judge(t, ratio.is_some_and(|r| r >= 0.85)),
            )
        };
        for (metric, value, verdict) in [
            direction(
                "upload",
                &s.engine_upload,
                &s.openssh_upload,
                s.upload_ratio,
            ),
            direction(
                "download",
                &s.engine_download,
                &s.openssh_download,
                s.download_ratio,
            ),
        ] {
            push(&metric, value, ">= 85% of OpenSSH", verdict);
        }
    }
    rows
}

pub fn soak_row(soak: &Soak) -> Row {
    Row {
        profile: soak.profile.clone(),
        metric: format!(
            "M4 {} sessions x {:.0} min: unexpected disconnects per 100 session-hours",
            soak.sessions,
            soak.duration_secs as f64 / 60.0
        ),
        value: format!(
            "{:.2} ({} disconnects in {:.1} session-hours)",
            soak.disconnects_per_100_session_hours, soak.unexpected_disconnects, soak.session_hours
        ),
        target: "report only".to_owned(),
        verdict: Verdict::Info,
    }
}

/// Every row of the report.
pub fn all_rows(report: &Report) -> Vec<Row> {
    let mut all: Vec<Row> = report.profiles.iter().flat_map(rows).collect();
    if let Some(soak) = &report.soak {
        all.push(soak_row(soak));
    }
    all
}

/// Rows whose target was missed.
pub fn misses(report: &Report) -> Vec<Row> {
    all_rows(report)
        .into_iter()
        .filter(|r| r.verdict == Verdict::Miss)
        .collect()
}

// ------------------------------------------------------------------ markdown

/// The Markdown report (job summary, release asset).
pub fn render_markdown(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Benchmarks (built-in engine)\n");
    let env = &report.environment;
    let _ = writeln!(out, "- Commit: `{}`", env.commit);
    let _ = writeln!(out, "- Engine: easyssh-ssh {}", env.engine_version);
    let _ = writeln!(out, "- OpenSSH client: {}", env.client_openssh);
    let _ = writeln!(out, "- Server: {}", env.server_banner);
    let _ = writeln!(
        out,
        "- Cipher / key exchange (pinned on the server): `{}` / `{}`",
        env.cipher, env.kex
    );
    let _ = writeln!(out, "- CPU: {} ({} threads)", env.cpu_model, env.cpu_count);
    let _ = writeln!(out, "- Kernel: {}", env.kernel);
    let _ = writeln!(out, "- Network shaping: {}", env.network_shaping);
    for note in &env.notes {
        let _ = writeln!(out, "- Note: {note}");
    }
    let _ = writeln!(out);

    for result in &report.profiles {
        let _ = writeln!(
            out,
            "### Profile `{}`: {}{}\n",
            result.profile.name,
            result.profile.describe(),
            if result.profile.has_targets {
                ""
            } else {
                " (no targets, reported only)"
            }
        );
        let table = rows(result);
        if table.is_empty() {
            let _ = writeln!(out, "_No metric ran on this profile._\n");
        } else {
            let _ = writeln!(out, "| Metric | Result | Target | |");
            let _ = writeln!(out, "| --- | --- | --- | --- |");
            for row in table {
                let _ = writeln!(
                    out,
                    "| {} | {} | {} | {} |",
                    row.metric,
                    row.value,
                    row.target,
                    row.verdict.label()
                );
            }
            let _ = writeln!(out);
        }
        for (metric, why) in &result.skipped {
            let _ = writeln!(out, "- {metric} skipped: {why}");
        }
        if let Some(c) = &result.connects {
            for failure in &c.failures {
                let _ = writeln!(out, "- connection failure: {failure}");
            }
        }
        if let Some(s) = &result.sftp {
            let _ = writeln!(
                out,
                "- SFTP: SHA-256 `{}` verified for every transfer (engine and OpenSSH, both directions).",
                s.sha256
            );
        }
        let _ = writeln!(out);
    }

    if let Some(soak) = &report.soak {
        let _ = writeln!(out, "### Stability (M4), profile `{}`\n", soak.profile);
        let _ = writeln!(out, "| Metric | Result | Target | |");
        let _ = writeln!(out, "| --- | --- | --- | --- |");
        let row = soak_row(soak);
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} |\n",
            row.metric,
            row.value,
            row.target,
            row.verdict.label()
        );
        let _ = writeln!(
            out,
            "Heartbeat every {} s per session, {} answered.",
            soak.heartbeat_secs, soak.heartbeats_ok
        );
        for (session, at, why) in soak.events.iter().take(20) {
            let _ = writeln!(out, "- session {session} at {at:.0} s: {why}");
        }
        let _ = writeln!(out);
    }

    let missed = misses(report);
    if missed.is_empty() {
        let _ = writeln!(out, "All targets that apply were met.");
    } else {
        let _ = writeln!(out, "Targets missed ({}):", missed.len());
        for row in &missed {
            let _ = writeln!(out, "- `{}` {}: {}", row.profile, row.metric, row.value);
        }
    }
    let _ = writeln!(
        out,
        "\nMethod, conditions and how to read this: docs/benchmarks.md. M3 is the engine-level echo of a PTY byte, not a rendered keystroke."
    );
    out
}

/// A short block for the release notes.
pub fn render_release_summary(report: &Report) -> String {
    let mut out = String::from("### Benchmarks\n\n");
    let normal: Vec<Row> = all_rows(report)
        .into_iter()
        .filter(|r| {
            r.profile == "normal" && !r.metric.starts_with("M2 cold connect ready time, median")
        })
        .collect();
    if normal.is_empty() {
        out.push_str("No results.\n");
    }
    for row in normal {
        let _ = writeln!(
            out,
            "- {} ({}): {} [{}]",
            row.metric,
            row.profile,
            row.value,
            row.verdict.label()
        );
    }
    out.push_str("\nFull report: `bench-report.md`, raw samples: `bench.json` (release assets).\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_even_empty() {
        assert_eq!(median(&[]), None);
        assert_eq!(median(&[3.0, 1.0, 2.0]), Some(2.0));
        assert_eq!(median(&[4.0, 1.0, 2.0, 3.0]), Some(2.5));
    }

    #[test]
    fn p95_is_nearest_rank() {
        assert_eq!(p95(&[]), None);
        assert_eq!(p95(&[7.0]), Some(7.0));
        // n = 20: rank ceil(19.0) = 19.
        let twenty: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(p95(&twenty), Some(19.0));
        // n = 100: rank 95.
        let hundred: Vec<f64> = (1..=100).rev().map(f64::from).collect();
        assert_eq!(p95(&hundred), Some(95.0));
        // n = 200: rank 190; n = 1000: rank 950.
        let n200: Vec<f64> = (1..=200).map(f64::from).collect();
        assert_eq!(p95(&n200), Some(190.0));
        let n1000: Vec<f64> = (1..=1000).map(f64::from).collect();
        assert_eq!(p95(&n1000), Some(950.0));
        // n = 3: rank ceil(2.85) = 3, the largest.
        assert_eq!(p95(&[1.0, 9.0, 5.0]), Some(9.0));
    }

    fn lookup(pairs: &'static [(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        move |name| {
            pairs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        }
    }

    #[test]
    fn settings_defaults_and_overrides() {
        let s = Settings::from_lookup(lookup(&[])).unwrap();
        assert_eq!((s.connections, s.sftp_mib, s.outage_runs), (200, 256, 30));
        assert_eq!(s.port, 2224);
        assert!(!s.enforce && s.netem_script.is_none());
        let s = Settings::from_lookup(lookup(&[
            ("BENCH_CONNECTIONS", "1000"),
            ("BENCH_SFTP_MIB", "1024"),
            ("BENCH_NETEM", "e2e/netem.sh"),
            ("BENCH_ENFORCE", "1"),
        ]))
        .unwrap();
        assert_eq!((s.connections, s.sftp_mib), (1000, 1024));
        assert_eq!(s.netem_script.as_deref(), Some("e2e/netem.sh"));
        assert!(s.enforce);
    }

    #[test]
    fn settings_reject_bad_numbers() {
        assert!(Settings::from_lookup(lookup(&[("BENCH_CONNECTIONS", "many")])).is_err());
        assert!(Settings::from_lookup(lookup(&[("BENCH_SFTP_REPEATS", "0")])).is_err());
    }

    #[test]
    fn profiles_parse() {
        assert_eq!(Profile::parse("normal").unwrap().rtt_ms, 20);
        assert_eq!(Profile::parse(" weak ").unwrap().loss_pct, 1.0);
        assert!(Profile::parse("fast").is_err());
    }

    #[test]
    fn outage_threshold_scales() {
        let o = |runs| Outage::new(runs, 10, 10, vec![], 0);
        assert_eq!(o(30).required(), 29);
        assert_eq!(o(3).required(), 3);
        assert_eq!(o(15).required(), 15);
        assert_eq!(o(60).required(), 58);
    }

    fn report_with(profile: Profile, connects: Connects) -> Report {
        let mut result = ProfileResult::new(profile);
        result.connects = Some(connects);
        Report {
            schema: 1,
            generated_at_unix: 0,
            environment: Environment::default(),
            settings: Settings::from_lookup(lookup(&[])).unwrap(),
            profiles: vec![result],
            soak: None,
        }
    }

    #[test]
    fn verdicts_follow_the_targets() {
        let fast: Vec<f64> = vec![0.2; 200];
        let good = report_with(Profile::normal(), Connects::new(200, 0, 0, fast, vec![]));
        assert!(misses(&good).is_empty());
        let md = render_markdown(&good);
        assert!(md.contains("| M1 connection success rate | 200/200 = 100.0%"));
        assert!(md.contains("PASS"));
        assert!(md.contains("All targets that apply were met."));

        // 198/200 = 99%: below 99.5%. P95 above 1.5 s.
        let slow: Vec<f64> = vec![2.0; 198];
        let bad = report_with(
            Profile::normal(),
            Connects::new(200, 2, 0, slow, vec!["timeout".into()]),
        );
        let missed = misses(&bad);
        assert_eq!(missed.len(), 2, "{missed:?}");
        let md = render_markdown(&bad);
        assert!(md.contains("Targets missed (2)"));
        assert!(md.contains("connection failure: timeout"));

        // The weak profile never fails a target.
        let weak = report_with(
            Profile::weak(),
            Connects::new(200, 50, 0, vec![3.0; 150], vec![]),
        );
        assert!(misses(&weak).is_empty());
        assert!(render_markdown(&weak).contains("reported only"));
    }

    #[test]
    fn sftp_ratio_and_json() {
        let mib = 1024.0 * 1024.0;
        let sftp = Sftp::new(
            256 * 1024 * 1024,
            3,
            "ab".into(),
            vec![80.0 * mib, 82.0 * mib, 81.0 * mib],
            vec![50.0 * mib],
            vec![100.0 * mib],
            vec![100.0 * mib],
        );
        assert!((sftp.upload_ratio.unwrap() - 0.81).abs() < 1e-9);
        let mut result = ProfileResult::new(Profile::normal());
        result.sftp = Some(sftp);
        let table = rows(&result);
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].verdict, Verdict::Miss); // 81% < 85%
        assert_eq!(table[1].verdict, Verdict::Miss);
        let report = Report {
            schema: 1,
            generated_at_unix: 1,
            environment: Environment::default(),
            settings: Settings::from_lookup(lookup(&[])).unwrap(),
            profiles: vec![result],
            soak: Some(Soak::new(
                "normal",
                25,
                3600,
                30,
                100,
                vec![(3, 12.0, "closed".into())],
            )),
        };
        let json: serde_json::Value = serde_json::from_str(&report.to_json()).unwrap();
        assert_eq!(json["schema"], 1);
        assert_eq!(
            json["profiles"][0]["sftp"]["engine_upload"]["bytes_per_sec"][1],
            82.0 * mib
        );
        assert_eq!(json["soak"]["unexpected_disconnects"], 1);
        // 1 disconnect in 25 session-hours = 4 per 100.
        assert!(
            (json["soak"]["disconnects_per_100_session_hours"]
                .as_f64()
                .unwrap()
                - 4.0)
                .abs()
                < 1e-9
        );
        let md = render_markdown(&report);
        assert!(md.contains("M6 SFTP upload (256 MiB x3)"));
        assert!(md.contains("M4 25 sessions x 60 min"));
        assert!(render_release_summary(&report).contains("bench-report.md"));
    }

    #[test]
    fn outage_counts_recoveries() {
        let o = Outage::new(4, 10, 10, vec![Some(0.5), None, Some(1.5), Some(1.0)], 1);
        assert_eq!(o.recovered, 3);
        assert_eq!(o.median_recovery_secs, Some(1.0));
    }
}
