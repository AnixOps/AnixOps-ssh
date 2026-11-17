//! The link to `easysshd`, the background process that owns terminal sessions
//! so they survive a window reload or close.
//!
//! The daemon is this same executable started with the hidden `--easysshd`
//! flag. A supervisor (`easysshd::supervisor`) connects to a running daemon or
//! starts one detached, reconnects with backoff when the connection drops and
//! re-attaches live sessions, and hands over from a daemon of another build.
//! While there is no connection, hosts and new sessions use the in-process
//! paths, exactly as before the daemon existed.

use easysshd::daemon::{run_blocking, Options};
use easysshd::launch::spawn_detached;
use easysshd::link::{AskpassPrompt, Link, LinkHandler};
use easysshd::supervisor::{Backoff, Supervisor, SupervisorConfig};
use easysshd::DaemonConfig;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter};

const FLAG: &str = "--easysshd";
/// Overrides the runtime directory (development and tests).
const ENV_RUNTIME_DIR: &str = "EASYSSHD_RUNTIME_DIR";
const START_TIMEOUT: Duration = Duration::from_secs(5);

fn runtime_dir() -> PathBuf {
    std::env::var_os(ENV_RUNTIME_DIR)
        .map(PathBuf::from)
        .unwrap_or_else(easysshd::default_runtime_dir)
}

/// Runs the daemon, and never returns, when started with `--easysshd`.
/// Returns normally on ordinary app startup.
pub fn run_if_requested() {
    if std::env::args().nth(1).as_deref() != Some(FLAG) {
        return;
    }
    let code = match DaemonConfig::for_current_user() {
        Ok(config) => match run_blocking(Options {
            runtime_dir: runtime_dir(),
            config,
            build_version: Some(env!("CARGO_PKG_VERSION").to_owned()),
        }) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("easysshd: {error}");
                1
            }
        },
        Err(error) => {
            eprintln!("easysshd: {error}");
            1
        }
    };
    std::process::exit(code);
}

/// The supervised connection to the daemon, when it was set up.
#[derive(Default)]
pub struct DaemonState(Mutex<Option<Arc<Supervisor>>>);

impl DaemonState {
    pub fn supervisor(&self) -> Option<Arc<Supervisor>> {
        self.0.lock().ok()?.clone()
    }

    /// The live link, or `None` when there is no daemon connection right now
    /// (never connected, or reconnecting): callers use the in-process paths.
    pub fn link(&self) -> Option<Link> {
        self.supervisor()?.link()
    }

    fn set(&self, supervisor: Arc<Supervisor>) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(supervisor);
        }
    }
}

struct Handler {
    app: AppHandle,
}

impl LinkHandler for Handler {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        // Same event and shape as the in-process bridge (askpass.rs).
        let _ = self.app.emit(
            "askpass",
            serde_json::json!({
                "id": prompt.id,
                "host": prompt.host,
                "prompt": prompt.prompt,
                "confirm": prompt.confirm,
                "rememberable": prompt.rememberable,
                "vaultLocked": prompt.vault_locked,
            }),
        );
    }

    fn askpass_resolved(&self, id: u64) {
        let _ = self
            .app
            .emit("askpass-resolved", serde_json::json!({ "id": id }));
    }

    fn hosts_changed(&self) {
        let _ = self.app.emit("hosts-changed", ());
    }

    fn vault_changed(&self, unlocked: bool) {
        let _ = self
            .app
            .emit("vault-changed", serde_json::json!({ "unlocked": unlocked }));
    }

    fn lost(&self) {
        eprintln!("EasySSH: lost the connection to easysshd; reconnecting");
    }
}

/// Connects to the daemon, starting it if needed. Blocks for at most a few
/// seconds; on failure logs why, keeps retrying in the background and leaves
/// the app on in-process sessions and hosts meanwhile.
pub fn start(app: &AppHandle) {
    use tauri::Manager;
    let config = SupervisorConfig {
        runtime_dir: runtime_dir(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
        spawn: Arc::new(|| {
            let exe = std::env::current_exe()?;
            spawn_detached(&exe, &[OsString::from(FLAG)])
        }),
        start_wait: START_TIMEOUT,
        backoff: Backoff::default(),
    };
    let supervisor = Supervisor::new(config, Arc::new(Handler { app: app.clone() }));
    app.state::<DaemonState>().set(supervisor.clone());
    if let Err(reason) = tauri::async_runtime::block_on(supervisor.start()) {
        eprintln!("EasySSH: easysshd unavailable, running in-process until it is: {reason}");
    }
}
