//! Running the daemon: bind, serve, exit on a signal or when idle.

use crate::error::Result;
use crate::runtime::{self, Paths};
use crate::service::{Daemon, DaemonConfig, DaemonExtras};
use crate::Server;
use std::path::PathBuf;

pub struct Options {
    pub runtime_dir: PathBuf,
    pub config: DaemonConfig,
    /// Build version reported in `proto.hello`; this crate's when `None`.
    pub build_version: Option<String>,
}

/// Serve until Ctrl+C / SIGTERM, or until the daemon has had no clients and no
/// running sessions for `config.idle_timeout`. Returns `AlreadyRunning` when
/// another daemon owns the runtime directory.
pub async fn run(options: Options) -> Result<()> {
    // The log sits next to the socket, in the per-user runtime directory. Make
    // the directory (owner-only) before anything else creates it.
    runtime::prepare_dir(&Paths::new(&options.runtime_dir)?)?;
    let extras = DaemonExtras {
        build_version: options.build_version,
        log_dir: Some(options.runtime_dir.clone()),
        runtime_dir: Some(options.runtime_dir.clone()),
        ..DaemonExtras::default()
    };
    let daemon = Daemon::new_with(options.config, extras).await?;
    let server = match Server::start_service(&options.runtime_dir, daemon.clone()) {
        Ok(server) => server,
        Err(e) => {
            daemon.log().error(&format!("could not start serving: {e}"));
            return Err(e);
        }
    };
    // This daemon owns the runtime directory now, so key files an earlier,
    // crashed daemon left behind are safe to remove.
    let swept = easyssh_vault::sweep(&options.runtime_dir);
    if swept > 0 {
        daemon
            .log()
            .info(&format!("removed {swept} leftover key files"));
    }
    daemon.log().info("serving");
    let idle = daemon.clone();
    let shutdown = async move {
        tokio::select! {
            _ = signal() => idle.log().info("stopping: signal"),
            _ = idle.wait_idle() => idle.log().info("stopping: idle"),
        }
    };
    let result = server.serve(shutdown).await;
    daemon.shutdown();
    match &result {
        Ok(()) => daemon.log().info("stopped"),
        Err(e) => daemon.log().error(&format!("stopped with an error: {e}")),
    }
    result
}

/// Blocking entry point for callers without a tokio runtime (the app's
/// hidden `--easysshd` mode).
pub fn run_blocking(options: Options) -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run(options))
}

async fn signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut term) = signal(SignalKind::terminate()) {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
