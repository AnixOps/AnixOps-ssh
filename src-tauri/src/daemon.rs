//! The link to `easysshd`, the background process that owns terminal sessions
//! so they survive a window reload or close.
//!
//! The daemon is this same executable started with the hidden `--easysshd`
//! flag. At startup the app connects to a running daemon or starts one
//! detached. When that fails, the reason is logged and the app keeps working
//! with in-process sessions, exactly as before.

use easysshd::daemon::{run_blocking, Options};
use easysshd::launch::{connect_or_spawn, spawn_detached};
use easysshd::link::{AskpassPrompt, Link, LinkHandler};
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

/// The connection to the daemon, when there is one.
#[derive(Default)]
pub struct DaemonState(Mutex<Option<Link>>);

impl DaemonState {
    /// The live link, or `None` when sessions run in-process.
    pub fn link(&self) -> Option<Link> {
        let link = self.0.lock().ok()?.clone()?;
        link.is_alive().then_some(link)
    }

    fn set(&self, link: Link) {
        if let Ok(mut slot) = self.0.lock() {
            *slot = Some(link);
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
            }),
        );
    }

    fn lost(&self) {
        eprintln!("EasySSH: lost the connection to easysshd; new sessions run in-process");
    }
}

/// Connects to the daemon, starting it if needed. Blocks for at most a few
/// seconds; on failure logs why and leaves the app on in-process sessions.
pub fn start(app: &AppHandle) {
    let dir = runtime_dir();
    let handler = Arc::new(Handler { app: app.clone() });
    let result = tauri::async_runtime::block_on(async {
        let exe =
            std::env::current_exe().map_err(|error| format!("no executable path: {error}"))?;
        let args = vec![OsString::from(FLAG)];
        connect_or_spawn(&dir, handler, || spawn_detached(&exe, &args), START_TIMEOUT).await
    });
    match result {
        Ok(link) => {
            use tauri::Manager;
            app.state::<DaemonState>().set(link);
        }
        Err(reason) => {
            eprintln!("EasySSH: easysshd unavailable, running sessions in-process: {reason}");
        }
    }
}
