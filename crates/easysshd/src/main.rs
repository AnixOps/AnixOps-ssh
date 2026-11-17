use easysshd::daemon::{run_blocking, Options};
use easysshd::{default_runtime_dir, DaemonConfig};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = "usage: easysshd [--runtime-dir <path>] [--config <connections.json>] [--idle-timeout-secs <n>]";

fn parse_args() -> Result<Options, String> {
    let mut dir = None;
    let mut config = DaemonConfig::for_current_user().map_err(|e| e.to_string())?;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        let name = a.to_string_lossy().into_owned();
        match name.as_str() {
            "--runtime-dir" | "--config" | "--idle-timeout-secs" => {
                let v = args.next().ok_or(format!("{name} needs a value"))?;
                match name.as_str() {
                    "--runtime-dir" => dir = Some(PathBuf::from(v)),
                    "--config" => config.config_store = easyssh_core::ConfigStore::at(v),
                    _ => {
                        let secs: u64 = v
                            .to_str()
                            .and_then(|s| s.parse().ok())
                            .ok_or("--idle-timeout-secs needs a number")?;
                        config.idle_timeout = Duration::from_secs(secs);
                    }
                }
            }
            "-h" | "--help" => return Err(USAGE.into()),
            _ => return Err(format!("unknown argument {a:?}\n{USAGE}")),
        }
    }
    Ok(Options {
        runtime_dir: dir.unwrap_or_else(default_runtime_dir),
        config,
        build_version: None,
    })
}

fn main() -> ExitCode {
    // OpenSSH may start this executable as its askpass helper.
    easysshd::askpass::run_helper_if_requested();
    let options = match parse_args() {
        Ok(o) => o,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    eprintln!(
        "easysshd: starting, runtime dir {}",
        options.runtime_dir.display()
    );
    match run_blocking(options) {
        Ok(()) => {
            eprintln!("easysshd: shut down");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("easysshd: {e}");
            ExitCode::FAILURE
        }
    }
}
