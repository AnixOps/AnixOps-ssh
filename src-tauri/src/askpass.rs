//! Password, passphrase and host-key prompts for `sftp`, which runs without a
//! terminal. OpenSSH runs the program named in `SSH_ASKPASS` for each prompt;
//! that program is this executable in helper mode. The helper forwards the
//! prompt to the running app over a loopback socket guarded by a random
//! token, the app shows a dialog, and the answer goes back to OpenSSH on the
//! helper's standard output. Answers are never stored.
//!
//! This is the in-process bridge, used when `easysshd` is not available. With
//! the daemon running, the same helper talks to the daemon's bridge instead
//! (`easysshd::askpass`) and `env_for` points OpenSSH there.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::daemon::DaemonState;
pub use easysshd::askpass::run_helper_if_requested;
use easysshd::askpass::{ENV_CONTEXT, ENV_ENDPOINT, ENV_HOST};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    context: String,
    host: String,
    prompt: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PromptEvent {
    id: u64,
    host: String,
    prompt: String,
    /// A yes/no question, such as accepting a new host key.
    confirm: bool,
}

#[derive(Default)]
pub struct Askpass {
    endpoint: Mutex<Option<String>>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Option<String>>>>,
    /// Processes whose prompt was cancelled; their later prompts fail at once.
    cancelled: Mutex<HashSet<String>>,
    next: AtomicU64,
}

/// Environment for an `sftp` process so its prompts come to this app.
pub fn env_for(app: &AppHandle, host: &str) -> Result<Vec<(String, String)>, String> {
    if let Some(link) = app.state::<DaemonState>().link() {
        let helper = std::env::current_exe().map_err(io_error)?;
        let context = uuid::Uuid::new_v4().simple().to_string();
        return Ok(easysshd::askpass::env(
            link.askpass_endpoint(),
            &helper,
            &context,
            host,
        ));
    }
    let askpass = app.state::<Askpass>();
    let endpoint = askpass.endpoint(app)?;
    let helper = std::env::current_exe().map_err(io_error)?;
    let context = askpass.next.fetch_add(1, Ordering::Relaxed).to_string();
    let mut env = vec![
        (
            "SSH_ASKPASS".to_owned(),
            helper.to_string_lossy().into_owned(),
        ),
        ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
        (ENV_ENDPOINT.to_owned(), endpoint),
        (ENV_CONTEXT.to_owned(), context),
        (ENV_HOST.to_owned(), host.to_owned()),
    ];
    // Older OpenSSH releases only use SSH_ASKPASS when DISPLAY is set.
    if std::env::var_os("DISPLAY").is_none() {
        env.push(("DISPLAY".to_owned(), ":0".to_owned()));
    }
    Ok(env)
}

impl Askpass {
    /// Starts the loopback listener on first use and returns "port token".
    fn endpoint(&self, app: &AppHandle) -> Result<String, String> {
        let mut endpoint = self.endpoint.lock().map_err(lock_error)?;
        if let Some(endpoint) = endpoint.as_ref() {
            return Ok(endpoint.clone());
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).map_err(io_error)?;
        let port = listener.local_addr().map_err(io_error)?.port();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let app = app.clone();
        let expected = token.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let app = app.clone();
                let expected = expected.clone();
                std::thread::spawn(move || serve(&app, &stream, &expected));
            }
        });
        let value = format!("{port} {token}");
        *endpoint = Some(value.clone());
        Ok(value)
    }
}

fn serve(app: &AppHandle, stream: &TcpStream, token: &str) -> Option<()> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let request: Request = serde_json::from_str(&line).ok()?;
    if request.token != token {
        return None;
    }
    let askpass = app.state::<Askpass>();
    let cancelled = askpass.cancelled.lock().ok()?.contains(&request.context);
    let answer = if cancelled {
        None
    } else {
        let id = askpass.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        askpass.pending.lock().ok()?.insert(id, sender);
        let event = PromptEvent {
            id,
            host: request.host,
            confirm: request.prompt.contains("(yes/no"),
            prompt: request.prompt,
        };
        app.emit("askpass", event).ok()?;
        let answer = receiver.recv_timeout(ANSWER_TIMEOUT).ok().flatten();
        askpass.pending.lock().ok()?.remove(&id);
        if answer.is_none() {
            askpass.cancelled.lock().ok()?.insert(request.context);
        }
        answer
    };
    let mut writer = stream;
    serde_json::to_writer(&mut writer, &answer).ok()?;
    writer.write_all(b"\n").ok()
}

/// The dialog's answer; `None` when the user cancelled.
#[tauri::command]
pub fn askpass_reply(
    askpass: State<'_, Askpass>,
    daemon: State<'_, DaemonState>,
    id: u64,
    answer: Option<String>,
) -> Result<(), String> {
    if let Some(link) = daemon.link() {
        let _ = link.askpass_reply(id, answer.clone());
    }
    if let Some(sender) = askpass.pending.lock().map_err(lock_error)?.remove(&id) {
        let _ = sender.send(answer);
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn lock_error<T>(_: T) -> String {
    "prompt state is unavailable".to_owned()
}
