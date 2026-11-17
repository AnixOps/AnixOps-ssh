//! Encrypted session logs through the daemon: the setting (per host and
//! default), what is on disk, the key from the vault, and retention.
//!
//! `local_*` use a stand-in `ssh` that starts a local shell, so they run
//! anywhere with `sh`; `container_*` run a real `ssh` against the e2e
//! containers (see e2e/README.md).
#![cfg(unix)]

use easyssh_core::{ConfigStore, Connection};
use easyssh_crypto::{KdfParams, MemoryKeyStore};
use easyssh_e2e::E2e;
use easyssh_sessionlog::{decrypt, Decrypted, LogSettings, LogStore, KEY_INFO};
use easyssh_vault::Vault;
use easysshd::link::{AskpassPrompt, Link, LinkHandler, SessionMsg, SessionSink};
use easysshd::{Daemon, DaemonConfig, DaemonExtras, Server};
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

async fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !done() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct Running {
    runtime: PathBuf,
    data: PathBuf,
    config: PathBuf,
    vault: Arc<Vault>,
    stop: tokio::sync::oneshot::Sender<()>,
    serving: tokio::task::JoinHandle<()>,
    daemon: Arc<Daemon>,
}

fn test_vault() -> Arc<Vault> {
    Arc::new(Vault::with_kdf(
        Arc::new(MemoryKeyStore::new()),
        KdfParams::insecure_for_tests(),
    ))
}

async fn start_daemon(dir: &Path, vault: Arc<Vault>) -> Running {
    let config = dir.join("connections.json");
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(config.clone()),
        local_shell: None,
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let extras = DaemonExtras {
        vault: Some(vault.clone()),
        runtime_dir: Some(dir.join("run")),
        ..DaemonExtras::default()
    };
    let daemon = Daemon::new_with(cfg, extras).await.expect("daemon");
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
        data: dir.to_path_buf(),
        config,
        vault,
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

    fn logs(&self) -> LogStore {
        LogStore::in_data_dir(&self.data)
    }

    /// Changes the session log settings the way the app does: through its
    /// own connection to the same database.
    fn set_log_settings(&self, settings: &LogSettings) {
        let opened = easyssh_store::open_app_store_with(&self.config, self.vault.cipher())
            .expect("open the app database");
        opened
            .store
            .set_setting_as(LogSettings::KEY, settings)
            .expect("save settings");
    }

    fn decrypt(&self, id: &str) -> Decrypted {
        let bytes = self.logs().read(id, 1 << 24).expect("read the log");
        let derive = |salt: &[u8]| {
            self.vault
                .derive_key(KEY_INFO, salt)
                .map_err(|e| e.to_string())
        };
        decrypt(&bytes, &derive).expect("decrypt the log")
    }
}

async fn save_host(link: &Link, connection: &Connection) {
    link.request(
        "hosts.save",
        json!({"connection": serde_json::to_value(connection).unwrap()}),
    )
    .await
    .expect("hosts.save");
}

/// Opens a session on a saved host, runs `echo LOGMARKER-<n>`, and ends the
/// session. Returns what the terminal showed.
async fn run_marker(link: &Link, connection: &Connection, marker: &str) -> String {
    save_host(link, connection).await;
    let (seen, sink) = collector();
    let id = link
        .open_session(Some(&connection.id), 100, 30, sink)
        .await
        .expect("session.open");
    link.write_session(&id, &format!("echo {marker}-$((20+22))\r"))
        .unwrap();
    eventually("the output", || {
        text(&seen).contains(&format!("{marker}-42"))
    })
    .await;
    link.write_session(&id, "exit\r").unwrap();
    eventually("the session to end", || {
        seen.lock()
            .unwrap()
            .iter()
            .any(|m| matches!(m, SessionMsg::Exit { .. }))
    })
    .await;
    link.close_session(&id);
    text(&seen)
}

/// Every file under `dir` that contains `needle`.
fn files_containing(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                if bytes.windows(needle.len()).any(|w| w == needle.as_bytes()) {
                    found.push(path);
                }
            }
        }
    }
    found
}

// ------------------------------------------------------------- local shells

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

static LOCAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn have_sh() -> bool {
    std::process::Command::new("sh")
        .args(["-c", "true"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn named(name: &str) -> Connection {
    Connection::alias(name, "fake")
}

#[tokio::test]
async fn local_a_recorded_session_is_encrypted_on_disk_and_opens_with_the_vault_key() {
    if !have_sh() {
        return;
    }
    let _guard = LOCAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let _ssh = FakeSsh::install(dir.path(), "env PS1='$ ' sh -i");
    let running = start_daemon(dir.path(), test_vault()).await;
    let link = Link::connect(&running.runtime, Arc::new(NoPrompts))
        .await
        .expect("connect");
    let host = named("web 1");
    running.set_log_settings(&LogSettings {
        hosts: [(host.id.clone(), true)].into(),
        ..LogSettings::default()
    });

    let shown = run_marker(&link, &host, "LOGMARKER").await;
    assert!(shown.contains("LOGMARKER-42"), "{shown:?}");

    let logs = running.logs().list();
    assert_eq!(logs.len(), 1, "{logs:?}");
    assert_eq!(logs[0].host, "web_1");
    assert!(logs[0].id.ends_with(".esl"));
    let log = running.decrypt(&logs[0].id);
    assert!(log.complete, "the session ended, so the log is final");
    assert!(log.ended_ms.is_some());
    let plain = String::from_utf8_lossy(&log.data).into_owned();
    assert!(plain.contains("LOGMARKER-42"), "{plain:?}");

    // Nothing in the data directory holds the output in the clear.
    let leaks = files_containing(dir.path(), "LOGMARKER-4");
    assert!(
        leaks.is_empty(),
        "plain text found on disk in: {leaks:?} (the typed command and its result)"
    );
    // The file itself is private.
    let mode = std::fs::metadata(running.logs().resolve(&logs[0].id).unwrap())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);

    drop(link);
    running.finish().await;
}

#[tokio::test]
async fn local_logging_is_per_host_with_a_global_default_and_off_unless_asked() {
    if !have_sh() {
        return;
    }
    let _guard = LOCAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let _ssh = FakeSsh::install(dir.path(), "env PS1='$ ' sh -i");
    let running = start_daemon(dir.path(), test_vault()).await;
    let link = Link::connect(&running.runtime, Arc::new(NoPrompts))
        .await
        .expect("connect");
    let (quiet, loud, follower) = (named("quiet"), named("loud"), named("follower"));

    // Nothing is set: nothing is recorded.
    run_marker(&link, &quiet, "ONE").await;
    assert!(running.logs().list().is_empty());

    // One host on, one host explicitly off, one following a default of off.
    running.set_log_settings(&LogSettings {
        hosts: [(loud.id.clone(), true), (quiet.id.clone(), false)].into(),
        ..LogSettings::default()
    });
    run_marker(&link, &quiet, "TWO").await;
    run_marker(&link, &follower, "THREE").await;
    run_marker(&link, &loud, "FOUR").await;
    let hosts: Vec<_> = running.logs().list().into_iter().map(|e| e.host).collect();
    assert_eq!(hosts, ["loud"]);

    // A default of on records the host without an override and still honours
    // the explicit off.
    running.set_log_settings(&LogSettings {
        record_by_default: true,
        hosts: [(quiet.id.clone(), false)].into(),
        ..LogSettings::default()
    });
    run_marker(&link, &follower, "FIVE").await;
    run_marker(&link, &quiet, "SIX").await;
    let mut hosts: Vec<_> = running.logs().list().into_iter().map(|e| e.host).collect();
    hosts.sort();
    assert_eq!(hosts, ["follower", "loud"]);

    drop(link);
    running.finish().await;
}

#[tokio::test]
async fn local_a_locked_vault_stops_a_recorded_session_from_opening() {
    if !have_sh() {
        return;
    }
    let _guard = LOCAL.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let _ssh = FakeSsh::install(dir.path(), "env PS1='$ ' sh -i");
    let vault = test_vault();
    let running = start_daemon(dir.path(), vault.clone()).await;
    let link = Link::connect(&running.runtime, Arc::new(NoPrompts))
        .await
        .expect("connect");
    let host = named("guarded");
    save_host(&link, &host).await;
    running.set_log_settings(&LogSettings {
        record_by_default: true,
        ..LogSettings::default()
    });
    vault.set_passphrase("a long enough passphrase").unwrap();
    vault.lock();
    let (_seen, sink) = collector();
    let error = link
        .open_session(Some(&host.id), 80, 24, sink)
        .await
        .expect_err("a recorded session needs the vault");
    assert_eq!(error.code, "vault_locked", "{error:?}");
    assert!(running.logs().list().is_empty());

    vault.unlock("a long enough passphrase").unwrap();
    let shown = run_marker(&link, &host, "UNLOCKED").await;
    assert!(shown.contains("UNLOCKED-42"));
    assert_eq!(running.logs().list().len(), 1);
    drop(link);
    running.finish().await;
}

#[tokio::test]
async fn daemon_start_deletes_logs_past_the_retention() {
    let dir = tempfile::tempdir().unwrap();
    let vault = test_vault();
    let store = LogStore::in_data_dir(dir.path());
    let derive = |salt: &[u8]| vault.derive_key(KEY_INFO, salt).map_err(|e| e.to_string());
    let make = |host: &str, at: u64| {
        let (path, file) = store.create(host, at).unwrap();
        let mut w = easyssh_sessionlog::LogWriter::new(file, at, &derive).unwrap();
        w.write(b"old output").unwrap();
        w.finish(at + 1).unwrap();
        path
    };
    let old = make("oldhost", 1_000);
    let recent = make("newhost", 2_000);
    let aged = std::time::SystemTime::now() - Duration::from_secs(45 * 24 * 3600);
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(aged)
        .unwrap();
    // Default retention is 30 days.
    let running = start_daemon(dir.path(), vault.clone()).await;
    assert!(!old.exists(), "the 45 day old log is gone");
    assert!(recent.exists());
    running.finish().await;

    // A longer retention keeps it; start again with an older file.
    std::fs::File::options()
        .write(true)
        .open(&recent)
        .unwrap()
        .set_modified(aged)
        .unwrap();
    {
        let opened = easyssh_store::open_app_store_with(
            &dir.path().join("connections.json"),
            vault.cipher(),
        )
        .unwrap();
        opened
            .store
            .set_setting_as(
                LogSettings::KEY,
                &LogSettings {
                    retention_days: 90,
                    ..LogSettings::default()
                },
            )
            .unwrap();
    }
    let running = start_daemon(dir.path(), vault.clone()).await;
    assert!(recent.exists(), "kept under a 90 day retention");
    running.finish().await;
}

// --------------------------------------------------------------- containers

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn container_echo_marker_is_found_in_the_decrypted_log() {
    let e2e = E2e::new();
    let running = start_daemon(e2e.dir.path(), test_vault()).await;
    let link = Link::connect(&running.runtime, Arc::new(NoPrompts))
        .await
        .expect("connect");
    let host = e2e.alias("e2e-target");
    running.set_log_settings(&LogSettings {
        hosts: [(host.id.clone(), true)].into(),
        ..LogSettings::default()
    });
    let shown = run_marker(&link, &host, "LOGMARKER").await;
    assert!(shown.contains("LOGMARKER-42"), "{shown:?}");

    let logs = running.logs().list();
    assert_eq!(logs.len(), 1, "{logs:?}");
    let log = running.decrypt(&logs[0].id);
    assert!(log.complete);
    assert!(
        String::from_utf8_lossy(&log.data).contains("LOGMARKER-42"),
        "{:?}",
        String::from_utf8_lossy(&log.data)
    );
    assert!(files_containing(e2e.dir.path(), "LOGMARKER-4").is_empty());
    drop(link);
    running.finish().await;
}
