//! Running the daemon: bind, serve, exit on a signal or when idle.

use crate::error::Result;
use crate::service::{Daemon, DaemonConfig};
use crate::Server;
use std::path::PathBuf;

pub struct Options {
    pub runtime_dir: PathBuf,
    pub config: DaemonConfig,
}

/// Serve until Ctrl+C / SIGTERM, or until the daemon has had no clients and no
/// running sessions for `config.idle_timeout`. Returns `AlreadyRunning` when
/// another daemon owns the runtime directory.
pub async fn run(options: Options) -> Result<()> {
    let daemon = Daemon::new(options.config).await?;
    let server = Server::start_service(&options.runtime_dir, daemon.clone())?;
    let idle = daemon.clone();
    let shutdown = async move {
        tokio::select! {
            _ = signal() => {}
            _ = idle.wait_idle() => {}
        }
    };
    let result = server.serve(shutdown).await;
    daemon.shutdown();
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
