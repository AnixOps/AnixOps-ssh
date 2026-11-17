//! Finding a running daemon or starting one detached.

use crate::link::{Link, LinkHandler, LinkOptions};
use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a spawned daemon gets to answer before another is started.
const RESPAWN_AFTER: Duration = Duration::from_secs(1);

/// Connect to the daemon in `runtime_dir`; when none answers, call `spawn` to
/// start one and keep trying for `wait`. Two apps racing to start a daemon is
/// fine: the instance lock lets one win and both connect to it. A spawned
/// daemon that never comes up (it lost the lock to a predecessor that was
/// still exiting, or crashed) is replaced: `spawn` is called again every
/// [`RESPAWN_AFTER`] while nobody answers. The error says why the daemon
/// could not be reached.
pub async fn connect_or_spawn(
    runtime_dir: &Path,
    handler: Arc<dyn LinkHandler>,
    spawn: impl FnMut() -> std::io::Result<()>,
    wait: Duration,
) -> Result<Link, String> {
    connect_or_spawn_with(runtime_dir, handler, LinkOptions::default(), spawn, wait).await
}

/// [`connect_or_spawn`] with explicit [`LinkOptions`].
pub async fn connect_or_spawn_with(
    runtime_dir: &Path,
    handler: Arc<dyn LinkHandler>,
    options: LinkOptions,
    mut spawn: impl FnMut() -> std::io::Result<()>,
    wait: Duration,
) -> Result<Link, String> {
    match Link::connect_with(runtime_dir, handler.clone(), options.clone()).await {
        Ok(link) => return Ok(link),
        // A running daemon that refuses us is not one to start another next to.
        Err(e) if e.code == "schema_mismatch" => return Err(e.message),
        Err(_) => {}
    }
    spawn().map_err(|e| format!("could not start the daemon: {e}"))?;
    let mut spawned_at = Instant::now();
    let deadline = spawned_at + wait;
    loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        match Link::connect_with(runtime_dir, handler.clone(), options.clone()).await {
            Ok(link) => return Ok(link),
            Err(e) if e.code == "schema_mismatch" => return Err(e.message),
            Err(e) if Instant::now() >= deadline => {
                return Err(format!(
                    "the daemon did not accept a connection within {wait:?}: {e}"
                ))
            }
            Err(_) => {
                // A daemon started while its predecessor was still exiting
                // fails on the instance lock and is gone; start another.
                if spawned_at.elapsed() >= RESPAWN_AFTER {
                    spawn().map_err(|e| format!("could not start the daemon: {e}"))?;
                    spawned_at = Instant::now();
                }
            }
        }
    }
}

/// Start `exe args...` as a background process that outlives this one: no
/// terminal, no inherited standard streams, its own session or process group.
pub fn spawn_detached(exe: &Path, args: &[OsString]) -> std::io::Result<()> {
    let mut command = std::process::Command::new(exe);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    // The child is intentionally not waited for.
    command.spawn().map(drop)
}
