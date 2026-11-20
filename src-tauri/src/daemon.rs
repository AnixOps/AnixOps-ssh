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
use easysshd::link::{AskpassPrompt, HostKeyPrompt, Link, LinkHandler, TunnelStatus};
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
        // Same event and shape as the in-process bridge (askpass.rs), plus
        // what the built-in engine adds: what is asked and whether to echo.
        let mut payload = serde_json::json!({
            "id": prompt.id,
            "host": prompt.host,
            "prompt": prompt.prompt,
            "confirm": prompt.confirm,
            "rememberable": prompt.rememberable,
            "vaultLocked": prompt.vault_locked,
        });
        if let Some(kind) = prompt.kind {
            payload["kind"] = serde_json::json!(kind);
        }
        if let Some(echo) = prompt.echo {
            payload["echo"] = serde_json::json!(echo);
        }
        let _ = self.app.emit("askpass", payload);
    }

    fn hostkey_prompt(&self, prompt: HostKeyPrompt) {
        let _ = self.app.emit(
            "hostkey",
            serde_json::json!({
                "id": prompt.id,
                "host": prompt.host,
                "port": prompt.port,
                "algorithm": prompt.algorithm,
                "fingerprint": prompt.fingerprint,
                "status": prompt.status,
                "expected": prompt.expected,
            }),
        );
    }

    fn hostkey_resolved(&self, id: u64) {
        let _ = self
            .app
            .emit("hostkey-resolved", serde_json::json!({ "id": id }));
    }

    fn transfer_started(&self, transfer_id: &str) {
        crate::files::remote_event(&self.app, transfer_id, crate::files::TransferEvent::Started);
    }

    fn transfer_progress(&self, transfer_id: &str, bytes: u64, total: Option<u64>) {
        crate::files::remote_event(
            &self.app,
            transfer_id,
            crate::files::TransferEvent::Progress { bytes, total },
        );
    }

    fn transfer_done(&self, transfer_id: &str, status: &str, error: Option<&str>) {
        let event = match status {
            "completed" => crate::files::TransferEvent::Done,
            "cancelled" => crate::files::TransferEvent::Cancelled,
            _ => crate::files::TransferEvent::Failed {
                message: error.unwrap_or("the transfer failed").to_owned(),
            },
        };
        crate::files::remote_event(&self.app, transfer_id, event);
    }

    fn tunnel_status(&self, status: TunnelStatus) {
        crate::tunnels::remote_status(&self.app, status);
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

    fn sync_state(&self, status: serde_json::Value) {
        let _ = self.app.emit("sync-state", status);
    }

    fn lost(&self) {
        eprintln!("EasySSH: lost the connection to easysshd; reconnecting");
    }

    fn resumed(&self) {
        // A restarted daemon has none of the old one's tunnels.
        crate::tunnels::resync_remote(&self.app);
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

/// The daemon link when `host_id` runs on the built-in engine, so file and
/// tunnel commands go through the daemon (`sftp.*`, `tunnel.*`) and share the
/// terminal's connection. `None` means the OpenSSH paths apply: no daemon,
/// the host is on OpenSSH, or the daemon could not tell.
pub async fn russh_link(daemon: &DaemonState, host_id: &str) -> Option<Link> {
    let link = daemon.link()?;
    let answer = link
        .request("engine.resolve", serde_json::json!({ "host_id": host_id }))
        .await
        .ok()?;
    (answer["engine"].as_str() == Some("russh")).then_some(link)
}

/// Whether a daemon error means "use the OpenSSH path instead".
pub fn use_openssh(error: &easysshd::link::LinkError) -> bool {
    matches!(
        error.code.as_str(),
        "unsupported" | "wrong_engine" | "unavailable" | "disconnected"
    )
}

/// `hostkey.reply`: the user's decision about a host key.
#[tauri::command]
pub fn hostkey_reply(
    daemon: tauri::State<'_, DaemonState>,
    id: u64,
    decision: String,
) -> Result<(), String> {
    if !matches!(
        decision.as_str(),
        "reject" | "accept_once" | "accept_and_save" | "replace_and_save"
    ) {
        return Err("unknown decision".to_owned());
    }
    let link = daemon.link().ok_or("the session daemon is not running")?;
    link.hostkey_reply(id, &decision)
        .map_err(|error| error.to_string())
}
