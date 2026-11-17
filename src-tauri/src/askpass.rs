//! Password, passphrase and host-key prompts for `sftp`, which runs without a
//! terminal. OpenSSH runs the program named in `SSH_ASKPASS` for each prompt;
//! that program is this executable in helper mode. The helper forwards the
//! prompt to the running app over a loopback socket guarded by a random
//! token, the app shows a dialog, and the answer goes back to OpenSSH on the
//! helper's standard output. Answers are never stored, unless the user ticks
//! "Remember in vault" on a saved host's password or key-passphrase prompt.
//!
//! This is the in-process bridge, used when `easysshd` is not available. With
//! the daemon running, the same helper talks to the daemon's bridge instead
//! (`easysshd::askpass`) and `env_for` points OpenSSH there. Both bridges
//! follow the same rule for the vault: a prompt is answered from it only when
//! the OpenSSH process was bound to a saved host and the prompt is exactly
//! that host's password or key passphrase prompt (`easyssh_vault::prompt`).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::daemon::DaemonState;
use crate::vault::{app_answers, app_vault};
use easyssh_vault::{match_prompt, Answer, Binding, PromptAnswers, SecretKind};
pub use easysshd::askpass::run_helper_if_requested;
use easysshd::askpass::{ENV_CONTEXT, ENV_ENDPOINT, ENV_HOST};

const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_BINDINGS: usize = 4096;

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
    /// The saved host's password or key passphrase: offer "Remember in vault".
    rememberable: bool,
    /// A saved answer exists but the vault is locked.
    vault_locked: bool,
}

struct Waiting {
    sender: mpsc::Sender<Option<String>>,
    matched: Option<(Binding, SecretKind)>,
}

#[derive(Default)]
pub struct Askpass {
    endpoint: Mutex<Option<String>>,
    pending: Mutex<HashMap<u64, Waiting>>,
    /// Which saved host each `sftp`/`ssh` process belongs to.
    bindings: Mutex<HashMap<String, Binding>>,
    /// `(context, is_password)` already answered from the vault once.
    auto_answered: Mutex<HashSet<(String, bool)>>,
    /// Processes whose prompt was cancelled; their later prompts fail at once.
    cancelled: Mutex<HashSet<String>>,
    next: AtomicU64,
}

/// Environment for an `sftp` process so its prompts come to this app. With a
/// `host_id` of a saved host, its saved password and key passphrase may
/// answer that process's matching prompts.
pub fn env_for(
    app: &AppHandle,
    host: &str,
    host_id: Option<&str>,
) -> Result<Vec<(String, String)>, String> {
    if let Some(link) = app.state::<DaemonState>().link() {
        let helper = std::env::current_exe().map_err(io_error)?;
        let context = uuid::Uuid::new_v4().simple().to_string();
        if let Some(host_id) = host_id {
            // Queued ahead of anything the caller does next.
            let _ = link.askpass_bind(&context, host_id);
        }
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
    if let Some(host_id) = host_id {
        askpass.bind(&context, host_id);
    }
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
    /// Binds `context` to a saved host, if the database knows it.
    fn bind(&self, context: &str, host_id: &str) {
        let Ok(store) = crate::hosts::open_store() else {
            return;
        };
        let Ok(Some(record)) = store.get_host(host_id) else {
            return;
        };
        let flags = store.host_secret_flags(host_id).unwrap_or_default();
        let mut binding = Binding::for_connection(&record.item.connection, None);
        binding.passphrase_expected = flags.passphrase;
        if let Ok(mut bindings) = self.bindings.lock() {
            if bindings.len() >= MAX_BINDINGS {
                bindings.clear();
            }
            bindings.insert(context.to_owned(), binding);
        }
    }

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
        ask(app, &askpass, request)?
    };
    let mut writer = stream;
    serde_json::to_writer(&mut writer, &answer).ok()?;
    writer.write_all(b"\n").ok()
}

/// Answers from the vault when allowed, else asks the user. `None` (outer)
/// means the app could not even show a dialog.
fn ask(app: &AppHandle, askpass: &Askpass, request: Request) -> Option<Option<String>> {
    let matched = askpass
        .bindings
        .lock()
        .ok()?
        .get(&request.context)
        .cloned()
        .and_then(|binding| match_prompt(&binding, &request.prompt).map(|kind| (binding, kind)));
    let mut vault_locked = false;
    if let Some((binding, kind)) = &matched {
        let key = (request.context.clone(), *kind == SecretKind::Password);
        // Once per process and kind: asking again means the saved one was wrong.
        if askpass.auto_answered.lock().ok()?.insert(key.clone()) {
            match app_answers().answer(binding, *kind) {
                Answer::Secret(secret) => return Some(Some(secret.expose().to_owned())),
                Answer::Locked => {
                    vault_locked = true;
                    askpass.auto_answered.lock().ok()?.remove(&key);
                }
                Answer::None => {}
            }
        }
    }
    let id = askpass.next.fetch_add(1, Ordering::Relaxed);
    let (sender, receiver) = mpsc::channel();
    let rememberable = matched.is_some();
    askpass
        .pending
        .lock()
        .ok()?
        .insert(id, Waiting { sender, matched });
    let event = PromptEvent {
        id,
        host: request.host,
        confirm: request.prompt.contains("(yes/no"),
        prompt: request.prompt,
        rememberable,
        vault_locked,
    };
    app.emit("askpass", event).ok()?;
    let answer = receiver.recv_timeout(ANSWER_TIMEOUT).ok().flatten();
    askpass.pending.lock().ok()?.remove(&id);
    if answer.is_none() {
        askpass.cancelled.lock().ok()?.insert(request.context);
    }
    Some(answer)
}

/// The dialog's answer; `None` when the user cancelled. With `remember` a
/// saved host's password or key passphrase is also saved in the vault.
#[tauri::command]
pub fn askpass_reply(
    askpass: State<'_, Askpass>,
    daemon: State<'_, DaemonState>,
    id: u64,
    answer: Option<String>,
    remember: Option<bool>,
) -> Result<(), String> {
    let remember = remember.unwrap_or(false);
    if let Some(link) = daemon.link() {
        let _ = link.askpass_reply_with(id, answer.clone(), remember);
    }
    let waiting = askpass.pending.lock().map_err(lock_error)?.remove(&id);
    if let Some(waiting) = waiting {
        if let (true, Some(value), Some((binding, kind))) =
            (remember, answer.as_ref(), waiting.matched.as_ref())
        {
            // A failure (a locked vault) only means nothing was saved.
            let _ = app_answers().remember(
                binding,
                *kind,
                easyssh_crypto::SecretText::new(value.clone()),
            );
        }
        let _ = waiting.sender.send(answer);
    }
    Ok(())
}

/// Unlocks the vault from the prompt that needed it and answers that prompt
/// with the saved secret.
#[tauri::command]
pub async fn askpass_unlock(
    askpass: State<'_, Askpass>,
    daemon: State<'_, DaemonState>,
    id: u64,
    passphrase: String,
) -> Result<(), String> {
    if let Some(link) = daemon.link() {
        match link
            .request(
                "askpass.unlock",
                serde_json::json!({"id": id, "passphrase": passphrase}),
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) if crate::hosts::daemon_down(&error) => {}
            Err(error) => return Err(error.message),
        }
    }
    let passphrase = zeroize::Zeroizing::new(passphrase);
    let vault = app_vault();
    tauri::async_runtime::spawn_blocking(move || vault.unlock(&passphrase))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    let matched = askpass
        .pending
        .lock()
        .map_err(lock_error)?
        .get(&id)
        .and_then(|waiting| waiting.matched.clone());
    if let Some((binding, kind)) = matched {
        if let Answer::Secret(secret) = app_answers().answer(&binding, kind) {
            if let Some(waiting) = askpass.pending.lock().map_err(lock_error)?.remove(&id) {
                let _ = waiting.sender.send(Some(secret.expose().to_owned()));
            }
        }
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn lock_error<T>(_: T) -> String {
    "prompt state is unavailable".to_owned()
}
