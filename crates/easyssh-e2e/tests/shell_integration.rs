//! Opt-in shell integration through the daemon (see `shell-integration/README.md`).
//!
//! `container_*` run a real `ssh` against the e2e containers, whose users have
//! bash, zsh and fish as their login shell. `local_*` use a stand-in `ssh` that
//! starts a local shell instead, so the delivery (waiting for the prompt,
//! hiding the typed script, the hooks) is also tested without Docker, on any
//! Unix machine that has the shell; a missing shell is skipped.
#![cfg(unix)]

use easyssh_core::{ConfigStore, Connection};
use easyssh_e2e::E2e;
use easysshd::link::{AskpassPrompt, Link, LinkHandler, SessionMsg, SessionSink};
use easysshd::{Daemon, DaemonConfig, Server};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct NoPrompts;

impl LinkHandler for NoPrompts {
    fn askpass_prompt(&self, _prompt: AskpassPrompt) {}
}

type Seen = Arc<Mutex<Vec<SessionMsg>>>;

fn collector() -> (Seen, SessionSink) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    (
        seen,
        Box::new(move |message| {
            sink.lock().unwrap().push(message);
            true
        }),
    )
}

fn text(seen: &Seen) -> String {
    seen.lock()
        .unwrap()
        .iter()
        .filter_map(|message| match message {
            SessionMsg::Data { data, .. } => Some(data.as_str()),
            SessionMsg::Exit { .. } => None,
        })
        .collect()
}

async fn eventually(what: &str, seen: &Seen, mut done: impl FnMut(&str) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let output = text(seen);
        if done(&output) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}; output so far:\n{output:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// An OSC 6973 sequence as the scripts print it.
fn osc(kind: &str, payload: &str) -> String {
    if payload.is_empty() {
        format!("\x1b]6973;{kind}\x07")
    } else {
        format!("\x1b]6973;{kind};{payload}\x07")
    }
}

struct Running {
    runtime: PathBuf,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<()>,
    daemon: Arc<Daemon>,
}

async fn start_daemon(dir: &Path) -> Running {
    let config = DaemonConfig {
        config_store: ConfigStore::at(dir.join("connections.json")),
        local_shell: None,
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let daemon = Daemon::new(config).await.expect("daemon");
    let runtime = dir.join("run");
    let server = Server::start_service(&runtime, daemon.clone()).expect("start server");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let serving = tokio::spawn(async move {
        server
            .serve(async {
                let _ = stopped.await;
            })
            .await
            .expect("serve")
    });
    Running {
        runtime,
        stop,
        serving,
        daemon,
    }
}

impl Running {
    async fn finish(self) {
        let _ = self.stop.send(());
        self.serving.await.expect("server task");
        self.daemon.shutdown();
    }
}

/// Saves a host with shell integration set as given, opens a session, and
/// returns the link, the session id and what it prints.
async fn open(
    running: &Running,
    mut connection: Connection,
    integration: bool,
) -> (Link, String, Seen) {
    connection.shell_integration = integration;
    let link = Link::connect(&running.runtime, Arc::new(NoPrompts))
        .await
        .expect("connect");
    link.request(
        "hosts.save",
        json!({"connection": serde_json::to_value(&connection).unwrap()}),
    )
    .await
    .expect("hosts.save");
    let (seen, sink) = collector();
    let id = link
        .open_session(Some(&connection.id), 100, 30, sink)
        .await
        .expect("session.open");
    (link, id, seen)
}

/// What the integration must have reported by the time `false` and `cd /tmp`
/// ran, and that the delivery left no trace on screen.
async fn check_reports(link: &Link, id: &str, seen: &Seen, shell: &str) {
    eventually("the ready sequence", seen, |t| t.contains(&osc("R", shell))).await;
    // The first prompt carries the markers.
    eventually("a marked prompt", seen, |t| t.contains(&osc("A", ""))).await;
    link.write_session(id, "false\r").unwrap();
    eventually("exit status 1", seen, |t| t.contains(&osc("D", "1"))).await;
    link.write_session(id, "cd /tmp\r").unwrap();
    eventually("the new directory", seen, |t| t.contains(&osc("P", "/tmp"))).await;

    let all = text(seen);
    assert!(
        all.contains(&osc("C", "false")),
        "command line missing:\n{all:?}"
    );
    assert!(
        all.contains(&osc("C", "cd%20/tmp")),
        "command line missing:\n{all:?}"
    );
    // The typed script is never shown, nor kept in the replayed scrollback.
    for leak in ["__easyssh_", "eval '", "easyssh-si-bootstrap", "6973;E"] {
        assert!(
            !all.contains(leak),
            "{leak:?} leaked into the output:\n{all:?}"
        );
    }
    // Commands were reported in order: false (C, D;1), then cd.
    let c_false = all.find(&osc("C", "false")).unwrap();
    let d_one = all.find(&osc("D", "1")).unwrap();
    let c_cd = all.find(&osc("C", "cd%20/tmp")).unwrap();
    assert!(c_false < d_one && d_one < c_cd, "out of order:\n{all:?}");

    // The history of the shell has nothing of the delivery (leading space,
    // history cleanup); bash and fish are checked, zsh keeps one short line
    // when HIST_IGNORE_SPACE is off.
    if shell != "zsh" {
        let probe = if shell == "fish" {
            "echo n-(history | string match -r 'eas[y]ssh' | count)-n\r"
        } else {
            "echo n-$(history | grep -c 'eas[y]ssh')-n\r"
        };
        link.write_session(id, probe).unwrap();
        eventually("an empty history", seen, |t| t.contains("n-0-n")).await;
    }
}

async fn exit_session(link: &Link, id: &str, seen: &Seen) {
    link.write_session(id, "exit\r").unwrap();
    eventually("the session to end", seen, |_| {
        seen.lock()
            .unwrap()
            .iter()
            .any(|message| matches!(message, SessionMsg::Exit { .. }))
    })
    .await;
    link.close_session(id);
}

// ---------------------------------------------------------------- containers

async fn container(alias: &str, shell: &str) {
    let e2e = E2e::new();
    let running = start_daemon(e2e.dir.path()).await;
    let (link, id, seen) = open(&running, e2e.alias(alias), true).await;
    check_reports(&link, &id, &seen, shell).await;
    exit_session(&link, &id, &seen).await;
    drop(link);
    running.finish().await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn container_bash_reports_exit_status_and_cwd() {
    container("e2e-target", "bash").await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn container_zsh_reports_exit_status_and_cwd() {
    container("e2e-zsh", "zsh").await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn container_fish_reports_exit_status_and_cwd() {
    container("e2e-fish", "fish").await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn container_host_without_integration_sees_no_sequences() {
    let e2e = E2e::new();
    let running = start_daemon(e2e.dir.path()).await;
    let (link, id, seen) = open(&running, e2e.alias("e2e-target"), false).await;
    link.write_session(&id, "echo plain-$((1+1))\r").unwrap();
    eventually("the command's output", &seen, |t| t.contains("plain-2")).await;
    // Give a (wrongly started) delivery time to show itself.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert!(!text(&seen).contains("6973"), "{:?}", text(&seen));
    exit_session(&link, &id, &seen).await;
    drop(link);
    running.finish().await;
}

// ------------------------------------------------------------- local shells

/// Puts a stand-in `ssh` first on `PATH` that runs `command` instead.
struct FakeSsh {
    saved: Option<std::ffi::OsString>,
}

impl FakeSsh {
    fn install(dir: &Path, command: &str) -> Self {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = bin.join("ssh");
        std::fs::write(&script, format!("#!/bin/sh\nexec {command}\n")).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let saved = std::env::var_os("PATH");
        let mut path = std::ffi::OsString::from(&bin);
        path.push(":");
        path.push(saved.clone().unwrap_or_default());
        std::env::set_var("PATH", path);
        FakeSsh { saved }
    }
}

impl Drop for FakeSsh {
    fn drop(&mut self) {
        match &self.saved {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

fn have(program: &str) -> bool {
    std::process::Command::new(program)
        .arg("--version")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// bash before 4.4 has no `PS0`, so it cannot report the command start.
fn bash_is_recent() -> bool {
    std::process::Command::new("bash")
        .args([
            "-c",
            "[ \"${BASH_VERSINFO[0]}${BASH_VERSINFO[1]}\" -ge 44 ]",
        ])
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

static LOCAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn local(shell: &str, command: &str, setup: impl FnOnce(&Path)) {
    let _guard = LOCAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    setup(&home);
    let command = command.replace("{home}", &home.display().to_string());
    let _ssh = FakeSsh::install(dir.path(), &command);
    let running = start_daemon(dir.path()).await;
    let (link, id, seen) = open(&running, Connection::alias("local", "fake"), true).await;
    check_reports(&link, &id, &seen, shell).await;
    exit_session(&link, &id, &seen).await;
    drop(link);
    running.finish().await;
}

#[tokio::test]
async fn local_bash_reports_exit_status_and_cwd() {
    if !have("bash") || !bash_is_recent() {
        eprintln!("skipped: bash 4.4 or later is not installed");
        return;
    }
    local(
        "bash",
        "env HOME='{home}' HISTFILE=/dev/null bash --norc --noprofile -i",
        |_| {},
    )
    .await;
}

#[tokio::test]
async fn local_zsh_reports_exit_status_and_cwd() {
    if !have("zsh") {
        eprintln!("skipped: zsh is not installed");
        return;
    }
    local(
        "zsh",
        "env HOME='{home}' ZDOTDIR='{home}' zsh -f -i",
        |_| {},
    )
    .await;
}

#[tokio::test]
async fn local_fish_reports_exit_status_and_cwd() {
    if !have("fish") {
        eprintln!("skipped: fish is not installed");
        return;
    }
    local(
        "fish",
        "env HOME='{home}' XDG_CONFIG_HOME='{home}/.config' XDG_DATA_HOME='{home}/.local/share' fish --no-config -i",
        |_| {},
    )
    .await;
}

#[tokio::test]
async fn local_unknown_shell_stays_quiet_and_usable() {
    if !have("dash") {
        eprintln!("skipped: dash is not installed");
        return;
    }
    let _guard = LOCAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let _ssh = FakeSsh::install(dir.path(), "env PS1='$ ' dash -i");
    let running = start_daemon(dir.path()).await;
    let (link, id, seen) = open(&running, Connection::alias("local", "fake"), true).await;
    // dash knows nothing of the scripts: after the delivery the shell still
    // runs commands, and nothing of it stays on screen.
    tokio::time::sleep(Duration::from_secs(3)).await;
    link.write_session(&id, "echo plain-$((1+1))\r").unwrap();
    eventually("the command's output", &seen, |t| t.contains("plain-2")).await;
    let all = text(&seen);
    assert!(!all.contains("__easyssh_"), "{all:?}");
    assert!(!all.contains("6973"), "{all:?}");
    exit_session(&link, &id, &seen).await;
    drop(link);
    running.finish().await;
}
