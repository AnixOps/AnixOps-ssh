//! Interactive sessions: a PTY running the system `ssh` (or a local shell),
//! streamed to the frontend over a Tauri channel. The app only relays bytes;
//! authentication stays with OpenSSH and the SSH agent.
//!
//! Normally the PTYs live in `easysshd` (see daemon.rs) and these commands are
//! thin proxies, so sessions survive a reload of the window. When the daemon
//! is unavailable (or draining for a newer build) new sessions fall back to
//! PTYs owned by this process. If the connection to the daemon drops, the
//! supervisor reconnects and re-attaches the live sessions; their tabs get
//! `reconnecting` and `resumed` events meanwhile.

use crate::daemon::DaemonState;
use crate::hosts;
use easyssh_engine::Utf8Stream;
use easysshd::link::{ExitReason, SessionMsg};
use easysshd::supervisor::{SupervisedMsg, SupervisedSink};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use tauri::{ipc::Channel, AppHandle, Manager, State};

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum SessionEvent {
    Data {
        data: String,
    },
    Exit {
        code: Option<u32>,
        /// `exit`, `signal` or `disconnected`.
        reason: &'static str,
        /// The signal's name (`TERM`) when `reason` is `signal`.
        #[serde(skip_serializing_if = "Option::is_none")]
        signal: Option<String>,
    },
    /// The connection to the daemon dropped; output resumes after `Resumed`.
    Reconnecting,
    Resumed,
}

struct Session {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, Session>>);

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn command_for(host_id: Option<&str>) -> Result<CommandBuilder, String> {
    let mut command = match host_id {
        Some(host_id) => {
            let (_, invocation) = hosts::invocation(host_id)?;
            let mut command = CommandBuilder::new(invocation.executable);
            command.args(invocation.args);
            command
        }
        None => local_shell(),
    };
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    if let Some(home) = dirs::home_dir() {
        command.cwd(home);
    }
    Ok(command)
}

#[cfg(windows)]
fn local_shell() -> CommandBuilder {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["pwsh.exe", "powershell.exe"] {
        if let Some(found) = std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
        {
            let mut command = CommandBuilder::new(found);
            command.arg("-NoLogo");
            return command;
        }
    }
    CommandBuilder::new_default_prog()
}

#[cfg(not(windows))]
fn local_shell() -> CommandBuilder {
    CommandBuilder::new_default_prog()
}

fn lock_error<T>(_: T) -> String {
    "session state is unavailable".to_owned()
}

/// Forwards daemon messages for one session to a webview channel. Dropping
/// the sink (channel gone, e.g. after a reload) leaves the session running.
fn sink_for(events: Channel<SessionEvent>) -> SupervisedSink {
    Box::new(move |message| match message {
        SupervisedMsg::Session(SessionMsg::Data { data, .. }) => {
            events.send(SessionEvent::Data { data }).is_ok()
        }
        SupervisedMsg::Session(SessionMsg::Exit { code, reason }) => {
            let _ = events.send(exit_event(code, &reason));
            false
        }
        SupervisedMsg::Reconnecting => events.send(SessionEvent::Reconnecting).is_ok(),
        SupervisedMsg::Resumed => events.send(SessionEvent::Resumed).is_ok(),
    })
}

fn exit_event(code: Option<u32>, reason: &ExitReason) -> SessionEvent {
    SessionEvent::Exit {
        code,
        reason: reason.kind(),
        signal: reason.signal().map(str::to_owned),
    }
}

/// Whether `id` names a session owned by this process.
fn is_local(sessions: &Sessions, id: &str) -> bool {
    sessions
        .0
        .lock()
        .map(|sessions| sessions.contains_key(id))
        .unwrap_or(false)
}

/// What `session_open` answers: the session and the engine it runs on.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedSession {
    id: String,
    /// `russh` or `openssh`; `None` for a local shell.
    engine: Option<String>,
    engine_reason: Option<String>,
}

/// Starts a session for `host_id`, or a local shell when it is `None`, and
/// returns its id and engine. Output and the exit status arrive on `events`.
#[tauri::command]
pub async fn session_open(
    app: AppHandle,
    sessions: State<'_, Sessions>,
    daemon: State<'_, DaemonState>,
    host_id: Option<String>,
    cols: u16,
    rows: u16,
    events: Channel<SessionEvent>,
) -> Result<OpenedSession, String> {
    crate::telemetry::record_session();
    if let Some(supervisor) = daemon.supervisor().filter(|s| s.accepts_new_sessions()) {
        match supervisor
            .open_session_info(host_id.as_deref(), cols, rows, sink_for(events.clone()))
            .await
        {
            Ok(opened) => {
                return Ok(OpenedSession {
                    id: opened.session_id,
                    engine: opened.engine,
                    engine_reason: opened.engine_reason,
                })
            }
            // The daemon went away or started draining since the check:
            // this session runs in-process instead.
            Err(error)
                if matches!(
                    error.code.as_str(),
                    "draining" | "unavailable" | "disconnected"
                ) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let local = host_id.is_none();
    let id = open_in_process(app, &sessions, host_id, cols, rows, events)?;
    Ok(OpenedSession {
        id,
        engine: (!local).then(|| "openssh".to_owned()),
        engine_reason: (!local)
            .then(|| "the session daemon is not running, so OpenSSH runs this host".to_owned()),
    })
}

/// A session the daemon holds, as the UI needs it to rebuild its tabs.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSession {
    id: String,
    host_id: Option<String>,
    cols: u16,
    rows: u16,
    running: bool,
    exit_code: Option<i64>,
    engine: Option<String>,
    engine_reason: Option<String>,
}

/// Sessions that outlived the window. Empty without a daemon.
#[tauri::command]
pub async fn sessions_list(daemon: State<'_, DaemonState>) -> Result<Vec<LiveSession>, String> {
    let Some(supervisor) = daemon.supervisor() else {
        return Ok(Vec::new());
    };
    let sessions = match supervisor.list_sessions().await {
        Ok(sessions) => sessions,
        // Reconnecting: the tabs that exist are already being re-attached.
        Err(error) if matches!(error.code.as_str(), "unavailable" | "disconnected") => {
            return Ok(Vec::new())
        }
        Err(error) => return Err(error.to_string()),
    };
    Ok(sessions
        .into_iter()
        .map(|info| LiveSession {
            id: info.session_id,
            host_id: info.host_id,
            cols: info.cols,
            rows: info.rows,
            running: info.running,
            exit_code: info.exit_code,
            engine: info
                .engine
                .and_then(|engine| serde_json::to_value(engine).ok())
                .and_then(|value| value.as_str().map(str::to_owned)),
            engine_reason: info.engine_reason,
        })
        .collect())
}

/// Reattaches `events` to a session: its buffered output is replayed first,
/// then live output follows.
#[tauri::command]
pub async fn session_attach(
    daemon: State<'_, DaemonState>,
    id: String,
    events: Channel<SessionEvent>,
) -> Result<AttachedSession, String> {
    let supervisor = daemon
        .supervisor()
        .ok_or("the session daemon is not running")?;
    supervisor
        .attach_session(&id, None, sink_for(events))
        .await
        .map_err(|error| error.to_string())?;
    // The engine is part of the session's record, not of its output.
    let info = supervisor
        .list_sessions()
        .await
        .ok()
        .and_then(|sessions| sessions.into_iter().find(|info| info.session_id == id));
    Ok(AttachedSession {
        engine: info
            .as_ref()
            .and_then(|info| serde_json::to_value(info.engine).ok())
            .and_then(|value| value.as_str().map(str::to_owned)),
        engine_reason: info.and_then(|info| info.engine_reason),
    })
}

/// What `session_attach` answers beyond the replay.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachedSession {
    engine: Option<String>,
    engine_reason: Option<String>,
}

/// Stops sending a session's output to this window; the session keeps running.
#[tauri::command]
pub fn session_detach(daemon: State<'_, DaemonState>, id: String) {
    if let Some(supervisor) = daemon.supervisor() {
        supervisor.detach_session(&id);
    }
}

fn open_in_process(
    app: AppHandle,
    sessions: &Sessions,
    host_id: Option<String>,
    cols: u16,
    rows: u16,
    events: Channel<SessionEvent>,
) -> Result<String, String> {
    let command = command_for(host_id.as_deref())?;
    let pair = native_pty_system()
        .openpty(pty_size(cols, rows))
        .map_err(|error| error.to_string())?;
    let mut child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| error.to_string())?;
    drop(pair.slave);
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| error.to_string())?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| error.to_string())?;
    let killer = child.clone_killer();

    let id = NEXT_SESSION.fetch_add(1, Ordering::Relaxed).to_string();
    sessions.0.lock().map_err(lock_error)?.insert(
        id.clone(),
        Session {
            master: pair.master,
            writer,
            killer,
        },
    );

    let (exit_sender, exit_receiver) = mpsc::channel();
    let session_id = id.clone();
    std::thread::spawn(move || {
        let code = child.wait().ok().map(|status| status.exit_code());
        let _ = exit_sender.send(code);
        // On Windows the ConPTY keeps the reader open after the child exits;
        // dropping the PTY ends it.
        if let Ok(mut sessions) = app.state::<Sessions>().0.lock() {
            sessions.remove(&session_id);
        }
    });
    std::thread::spawn(move || pump(reader, &events, &exit_receiver));
    Ok(id)
}

fn pump(
    reader: Box<dyn Read + Send>,
    events: &Channel<SessionEvent>,
    exit: &mpsc::Receiver<Option<u32>>,
) {
    // The blocking read runs on its own thread so a held-back lead byte can
    // be released after a quiet moment (a binary transfer must not wait on it).
    let (chunks, incoming) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 16 * 1024];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 || chunks.send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });
    let mut decoder = Utf8Stream::default();
    loop {
        let wait = if decoder.has_pending() {
            Duration::from_millis(25)
        } else {
            Duration::from_secs(3600)
        };
        let data = match incoming.recv_timeout(wait) {
            Ok(bytes) => decoder.push(&bytes),
            Err(mpsc::RecvTimeoutError::Timeout) => decoder.flush(),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if !data.is_empty() && events.send(SessionEvent::Data { data }).is_err() {
            break;
        }
    }
    let code = exit.recv_timeout(Duration::from_secs(3)).ok().flatten();
    let _ = events.send(exit_event(code, &ExitReason::from_code(code)));
}

#[tauri::command]
pub fn session_write(
    sessions: State<'_, Sessions>,
    daemon: State<'_, DaemonState>,
    id: String,
    data: String,
) -> Result<(), String> {
    if !is_local(&sessions, &id) {
        let supervisor = daemon.supervisor().ok_or("session has ended")?;
        return supervisor
            .write_session(&id, &data)
            .map_err(|error| error.to_string());
    }
    let mut sessions = sessions.0.lock().map_err(lock_error)?;
    let session = sessions.get_mut(&id).ok_or("session has ended")?;
    session
        .writer
        .write_all(&easyssh_engine::text_to_bytes(&data))
        .and_then(|()| session.writer.flush())
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn session_resize(
    sessions: State<'_, Sessions>,
    daemon: State<'_, DaemonState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    if !is_local(&sessions, &id) {
        if let Some(supervisor) = daemon.supervisor() {
            // While reconnecting the size is re-sent by the tab when it resumes.
            let _ = supervisor.resize_session(&id, cols, rows);
            return Ok(());
        }
        return Ok(());
    }
    let sessions = sessions.0.lock().map_err(lock_error)?;
    let Some(session) = sessions.get(&id) else {
        return Ok(());
    };
    session
        .master
        .resize(pty_size(cols, rows))
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub fn session_close(
    sessions: State<'_, Sessions>,
    daemon: State<'_, DaemonState>,
    id: String,
) -> Result<(), String> {
    if !is_local(&sessions, &id) {
        if let Some(supervisor) = daemon.supervisor() {
            supervisor.close_session(&id);
        }
        return Ok(());
    }
    let session = sessions.0.lock().map_err(lock_error)?.remove(&id);
    if let Some(mut session) = session {
        let _ = session.killer.kill();
    }
    Ok(())
}
