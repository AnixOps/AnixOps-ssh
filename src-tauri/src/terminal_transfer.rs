//! Local file access for `rz`/`sz` in the terminal. The web view runs the
//! ZMODEM protocol (src/lib/terminalTransfer.ts) and reads or writes the disk
//! through these commands; the bookkeeping is `easyssh_engine::terminal_files`.

use easyssh_engine::local_files::safe_file_name;
use easyssh_engine::terminal_files::{SourceInfo, TerminalFiles};
use serde::Serialize;
use std::path::PathBuf;
use tauri::ipc::{InvokeBody, Request, Response};
use tauri::State;

#[derive(Serialize)]
pub struct Sink {
    id: u64,
    path: String,
}

/// Opens a local file for sending.
#[tauri::command]
pub fn terminal_file_open_read(
    files: State<'_, TerminalFiles>,
    path: String,
) -> Result<SourceInfo, String> {
    files.open_source(&PathBuf::from(path))
}

/// The next chunk of a file being sent, as raw bytes; empty at the end.
#[tauri::command]
pub fn terminal_file_read(
    files: State<'_, TerminalFiles>,
    id: u64,
    len: usize,
) -> Result<Response, String> {
    files.read(id, len.min(1 << 20)).map(Response::new)
}

/// Starts receiving a file: at `path` when the user picked one, otherwise in
/// the Downloads folder under `name` (cleaned, and made unique).
#[tauri::command]
pub fn terminal_file_open_write(
    files: State<'_, TerminalFiles>,
    name: String,
    path: Option<String>,
) -> Result<Sink, String> {
    let (id, target) = match path {
        Some(path) => files.open_sink(&PathBuf::from(path))?,
        None => {
            let dir = dirs::download_dir()
                .or_else(dirs::home_dir)
                .ok_or("no Downloads folder was found")?;
            // The name is reserved (not just checked), so two receives of the
            // same name cannot end up in one file.
            files.open_sink_unique(&dir, &safe_file_name(&name))?
        }
    };
    Ok(Sink {
        id,
        path: target.to_string_lossy().into_owned(),
    })
}

/// Appends raw bytes (the request body) to a file being received; the id is
/// in the `x-file-id` header.
#[tauri::command]
pub fn terminal_file_write(
    files: State<'_, TerminalFiles>,
    request: Request<'_>,
) -> Result<(), String> {
    let id = request
        .headers()
        .get("x-file-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or("missing file id")?;
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("expected raw bytes".to_owned());
    };
    files.write(id, bytes)
}

/// Ends a file; a received one is kept when `keep`, removed otherwise.
#[tauri::command]
pub fn terminal_file_close(
    files: State<'_, TerminalFiles>,
    id: u64,
    keep: bool,
) -> Result<(), String> {
    files.close(id, keep)
}
