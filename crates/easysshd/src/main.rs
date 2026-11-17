use easysshd::{builtin_handler, default_runtime_dir, Server};
use std::path::PathBuf;
use std::process::ExitCode;

fn parse_args() -> Result<PathBuf, String> {
    let mut dir = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--runtime-dir") => {
                dir = Some(PathBuf::from(
                    args.next().ok_or("--runtime-dir needs a path")?,
                ));
            }
            Some("-h") | Some("--help") => {
                return Err("usage: easysshd [--runtime-dir <path>]".into());
            }
            _ => {
                return Err(format!(
                    "unknown argument {a:?}\nusage: easysshd [--runtime-dir <path>]"
                ))
            }
        }
    }
    Ok(dir.unwrap_or_else(default_runtime_dir))
}

#[tokio::main]
async fn main() -> ExitCode {
    let dir = match parse_args() {
        Ok(d) => d,
        Err(m) => {
            eprintln!("{m}");
            return ExitCode::from(2);
        }
    };
    let server = match Server::start(&dir, builtin_handler()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("easysshd: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("easysshd: listening, runtime dir {}", dir.display());
    let shutdown = async {
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
    };
    match server.serve(shutdown).await {
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
