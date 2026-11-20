//! Server monitoring: one fixed read-only script over an exec channel of the
//! connection a terminal already has, plus a keepalive round trip for the
//! latency. The script prints raw `/proc` and `df` text; the UI parses it
//! and turns counters into rates (`src/lib/monitor.ts`), so the same output
//! serves the desktop and the phone.

use easyssh_ssh::{Error as SshError, Session};
use std::time::{Duration, Instant};

/// How long one sample may take before it is given up.
pub const SAMPLE_TIMEOUT: Duration = Duration::from_secs(15);

/// The basic sample: enough for the side panel. Only reads; prints
/// `@section` markers the parser splits on. Non-Linux hosts print `@os` and
/// stop.
const BASIC: &str = r#"export LC_ALL=C
os=$(uname -s 2>/dev/null)
echo "@os $os"
[ "$os" = Linux ] || exit 0
echo @hostname; cat /proc/sys/kernel/hostname
echo @uptime; cat /proc/uptime
echo @loadavg; cat /proc/loadavg
echo @nproc; nproc 2>/dev/null || grep -c '^processor' /proc/cpuinfo
echo @hz; getconf CLK_TCK 2>/dev/null || echo 100
echo @page; getconf PAGESIZE 2>/dev/null || echo 4096
echo @stat; grep '^cpu ' /proc/stat
echo @meminfo; grep -E '^(MemTotal|MemFree|MemAvailable|Buffers|Cached|SwapTotal|SwapFree):' /proc/meminfo
echo @netdev; tail -n +3 /proc/net/dev
echo @df; df -kP 2>/dev/null
echo @pstat; cat /proc/[0-9]*/stat 2>/dev/null
"#;

/// Extra sections for the full process list: owner, command line and the
/// executable (only readable for the login user's own processes, or all of
/// them as root).
const DETAIL: &str = r#"echo @ps; ps -eo pid=,user:32=,rss=,args= 2>/dev/null || ps -o pid,user,rss,args 2>/dev/null
echo @exe; ls -l /proc/[0-9]*/exe 2>/dev/null
"#;

/// The script for one sample, on one line: the login shell may be csh,
/// which does not take a newline inside quotes.
pub fn script(detail: bool) -> String {
    let mut lines: Vec<&str> = BASIC.lines().collect();
    if detail {
        lines.extend(DETAIL.lines());
    }
    lines.push("echo @end");
    lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The command that runs the script under `sh`, whatever the login shell.
pub fn command(detail: bool) -> String {
    format!("sh -c '{}'", script(detail).replace('\'', r"'\''"))
}

/// One raw sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    /// What the script printed.
    pub output: String,
    /// The keepalive round trip, in milliseconds; `None` when the server did
    /// not answer it.
    pub latency_ms: Option<f64>,
}

/// Measures the latency and runs the sample script on `session`.
pub async fn sample(session: &Session, detail: bool) -> Result<Sample, SshError> {
    let started = Instant::now();
    let latency_ms = match tokio::time::timeout(Duration::from_secs(5), session.ping()).await {
        Ok(Ok(())) => Some(started.elapsed().as_secs_f64() * 1000.0),
        _ => None,
    };
    let output = tokio::time::timeout(SAMPLE_TIMEOUT, session.exec(&command(detail)))
        .await
        .map_err(|_| SshError::Channel("the server took too long to answer".into()))??;
    Ok(Sample {
        output: output.stdout_text(),
        latency_ms,
    })
}

/// The signals the process list can send.
pub fn signal_name(signal: &str) -> Option<&'static str> {
    match signal {
        "TERM" => Some("TERM"),
        "KILL" => Some("KILL"),
        "HUP" => Some("HUP"),
        "INT" => Some("INT"),
        _ => None,
    }
}

/// Sends `signal` to `pid` as the login user. The error carries what `kill`
/// said (for example "Operation not permitted").
pub async fn signal(session: &Session, pid: u32, signal: &str) -> Result<(), SshError> {
    let name =
        signal_name(signal).ok_or_else(|| SshError::Channel(format!("unknown signal {signal}")))?;
    let command = format!("kill -s {name} {pid}");
    let output = tokio::time::timeout(Duration::from_secs(10), session.exec(&command))
        .await
        .map_err(|_| SshError::Channel("the server took too long to answer".into()))??;
    if output.exit.success() {
        Ok(())
    } else {
        let text = output.stderr_text();
        let text = text.trim();
        Err(SshError::Channel(if text.is_empty() {
            format!("kill {pid} failed")
        } else {
            text.to_owned()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_adds_the_process_sections() {
        assert!(!script(false).contains("echo @ps;"));
        assert!(script(true).contains("@ps"));
        assert!(script(true).contains("@exe"));
        assert!(script(false).ends_with("echo @end"));
        assert!(!script(true).contains('\n'));
    }

    #[test]
    fn the_command_runs_the_script_under_sh() {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(command(false))
            .output();
        // Where there is a Unix sh, the quoting must survive it.
        if let Ok(output) = output {
            let text = String::from_utf8_lossy(&output.stdout);
            assert!(text.starts_with("@os "), "{text}");
            assert!(
                text.trim_end().ends_with("@end") || !text.contains("Linux"),
                "{text}"
            );
        }
    }

    #[test]
    fn only_known_signals_are_sent() {
        assert_eq!(signal_name("TERM"), Some("TERM"));
        assert_eq!(signal_name("KILL"), Some("KILL"));
        assert_eq!(signal_name("9; rm -rf /"), None);
    }
}
