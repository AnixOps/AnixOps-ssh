//! Password, passphrase and host-key prompts for OpenSSH processes that run
//! without a terminal (`sftp`, `scp`, tunnels).
//!
//! OpenSSH runs the program named in `SSH_ASKPASS` for each prompt. That
//! program is the application's own executable in helper mode (see
//! [`run_helper_if_requested`]). The helper sends the prompt to the daemon's
//! loopback listener, guarded by a random token; the daemon passes it to the
//! attached UI as an `askpass.prompt` event and returns the answer from
//! `askpass.reply` on the helper's standard output.
//!
//! Answers are held in memory only until they are handed to the helper. They
//! are never stored and never logged, except that the user may tick
//! "Remember in vault" on a password or key-passphrase prompt of a saved host,
//! which saves it encrypted (see `easyssh-vault`).
//!
//! The daemon may also answer from the vault itself, without a dialog. That
//! needs the process to be *bound* to a saved host ([`Hub::bind_process`]) and is
//! limited to the exact password and passphrase prompts of that host and
//! user (`easyssh_vault::match_prompt`); host-key questions and anything else
//! always go to the user. A saved answer is tried once per process and kind:
//! if OpenSSH asks again the stored secret was wrong, and the user is asked
//! instead of the server being hit with the same wrong password again.

use easyssh_vault::{match_prompt, Answer, Binding, PromptAnswers, SecretKind};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Semaphore};
use zeroize::Zeroizing;

/// `"<port> <token>"`: where the helper connects and what it must present.
pub const ENV_ENDPOINT: &str = "ANIXOPS_SSH_ASKPASS";
/// Identifies one OpenSSH process, so that after a cancelled prompt its later
/// prompts fail at once instead of asking again.
pub const ENV_CONTEXT: &str = "ANIXOPS_SSH_ASKPASS_CONTEXT";
pub const ENV_HOST: &str = "ANIXOPS_SSH_ASKPASS_HOST";

pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REQUEST: u64 = 64 * 1024;
const MAX_CONCURRENT: usize = 32;
const MAX_BINDINGS: usize = 4096;

#[derive(Serialize, Deserialize)]
struct Request {
    token: String,
    context: String,
    host: String,
    prompt: String,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Runs this executable as the askpass helper when OpenSSH started it for a
/// prompt, and exits. Returns normally on ordinary startup. Call it first
/// thing in `main`.
pub fn run_helper_if_requested() {
    let Ok(endpoint) = std::env::var(ENV_ENDPOINT) else {
        return;
    };
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let context = std::env::var(ENV_CONTEXT).unwrap_or_default();
    let host = std::env::var(ENV_HOST).unwrap_or_default();
    let code = match ask(&endpoint, &context, &host, &prompt) {
        Some(answer) => {
            let mut stdout = std::io::stdout();
            let _ = writeln!(stdout, "{answer}");
            let _ = stdout.flush();
            0
        }
        None => 1,
    };
    std::process::exit(code);
}

/// Sends one prompt to the daemon and waits for the answer (blocking).
/// `None` when the user cancelled or the daemon cannot be reached.
pub fn ask(endpoint: &str, context: &str, host: &str, prompt: &str) -> Option<String> {
    let (port, token) = endpoint.split_once(' ')?;
    let port: u16 = port.parse().ok()?;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    let request = Request {
        token: token.to_owned(),
        context: context.to_owned(),
        host: host.to_owned(),
        prompt: prompt.to_owned(),
    };
    serde_json::to_writer(&mut stream, &request).ok()?;
    stream.write_all(b"\n").ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    serde_json::from_str::<Option<String>>(line.trim()).ok()?
}

/// Environment for an OpenSSH process so its prompts reach the daemon.
/// `helper` is the executable that calls [`run_helper_if_requested`].
pub fn env(endpoint: &str, helper: &Path, context: &str, host: &str) -> Vec<(String, String)> {
    let mut env = vec![
        (
            "SSH_ASKPASS".to_owned(),
            helper.to_string_lossy().into_owned(),
        ),
        ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
        (ENV_ENDPOINT.to_owned(), endpoint.to_owned()),
        (ENV_CONTEXT.to_owned(), context.to_owned()),
        (ENV_HOST.to_owned(), host.to_owned()),
    ];
    // Older OpenSSH releases only use SSH_ASKPASS when DISPLAY is set.
    if std::env::var_os("DISPLAY").is_none() {
        env.push(("DISPLAY".to_owned(), ":0".to_owned()));
    }
    env
}

/// A prompt waiting for an answer.
#[derive(Clone, Debug)]
pub struct Prompt {
    pub id: u64,
    pub host: String,
    pub prompt: String,
    /// A yes/no question, such as accepting a new host key.
    pub confirm: bool,
    /// What a prompt of the built-in engine asks for (`password`,
    /// `passphrase`, `keyboard_interactive`); `None` for OpenSSH's.
    pub kind: Option<&'static str>,
    /// Whether the answer may be shown while typing.
    pub echo: Option<bool>,
    /// The prompt is the saved password or key passphrase of a saved host,
    /// so the dialog may offer "Remember in vault".
    pub rememberable: bool,
    /// A saved answer exists but the vault is locked.
    pub vault_locked: bool,
}

/// Delivers a prompt to whoever can answer it; `false` when nobody can.
pub type Notify = Arc<dyn Fn(Prompt) -> bool + Send + Sync>;

/// Told when a prompt is over (answered, cancelled or timed out), so that
/// every client that was shown it can close its dialog.
pub type Resolved = Arc<dyn Fn(u64) + Send + Sync>;

/// A prompt shown to the user and what it may be remembered as.
struct Waiting {
    sender: oneshot::Sender<Option<String>>,
    matched: Option<(Binding, SecretKind)>,
}

/// The daemon side: a loopback listener plus the table of open prompts.
pub struct Hub {
    port: u16,
    token: String,
    notify: Notify,
    resolved: Resolved,
    pending: Mutex<HashMap<u64, Waiting>>,
    answers: OnceLock<Arc<dyn PromptAnswers>>,
    /// Which saved host each OpenSSH process (askpass context) belongs to.
    bindings: Mutex<HashMap<String, Binding>>,
    /// `(context, kind)` pairs already answered from the vault.
    auto_answered: Mutex<HashSet<(String, bool)>>,
    /// Processes whose prompt was cancelled; their later prompts fail at once.
    cancelled: Mutex<HashSet<String>>,
    next: AtomicU64,
    answer_timeout: Duration,
}

impl Hub {
    /// Bind `127.0.0.1:<random port>` and start serving helpers.
    pub async fn bind(notify: Notify, resolved: Resolved) -> std::io::Result<Arc<Hub>> {
        Self::bind_with_timeout(notify, resolved, ANSWER_TIMEOUT).await
    }

    pub async fn bind_with_timeout(
        notify: Notify,
        resolved: Resolved,
        answer_timeout: Duration,
    ) -> std::io::Result<Arc<Hub>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let hub = Arc::new(Hub {
            port: listener.local_addr()?.port(),
            token: uuid::Uuid::new_v4().simple().to_string(),
            notify,
            resolved,
            pending: Mutex::new(HashMap::new()),
            answers: OnceLock::new(),
            bindings: Mutex::new(HashMap::new()),
            auto_answered: Mutex::new(HashSet::new()),
            cancelled: Mutex::new(HashSet::new()),
            next: AtomicU64::new(1),
            answer_timeout,
        });
        let limit = Arc::new(Semaphore::new(MAX_CONCURRENT));
        let serving = hub.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    continue;
                };
                let Ok(permit) = limit.clone().try_acquire_owned() else {
                    continue;
                };
                let hub = serving.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let _ = hub.serve(stream).await;
                });
            }
        });
        Ok(hub)
    }

    /// `"<port> <token>"` for [`ENV_ENDPOINT`].
    pub fn endpoint(&self) -> String {
        format!("{} {}", self.port, self.token)
    }

    /// A prompt of the built-in engine: shown to the attached clients like an
    /// OpenSSH one, answered with `askpass.reply`. `None` when the user
    /// cancelled, nobody could be asked, or nobody answered in time.
    pub async fn prompt(
        &self,
        host: &str,
        prompt: &str,
        kind: &'static str,
        echo: bool,
    ) -> Option<String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(
            id,
            Waiting {
                sender,
                matched: None,
            },
        );
        let shown = (self.notify)(Prompt {
            id,
            host: host.to_owned(),
            prompt: prompt.to_owned(),
            confirm: false,
            kind: Some(kind),
            echo: Some(echo),
            rememberable: false,
            vault_locked: false,
        });
        if !shown {
            lock(&self.pending).remove(&id);
            return None;
        }
        let answer = tokio::time::timeout(self.answer_timeout, receiver)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
        lock(&self.pending).remove(&id);
        (self.resolved)(id);
        answer
    }

    /// Allocates an id that no askpass prompt uses, for other kinds of
    /// questions (host keys) that share the clients' dialogs.
    pub fn next_id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Lets the hub answer from, and save to, the vault. Set once.
    pub fn set_answers(&self, answers: Arc<dyn PromptAnswers>) {
        let _ = self.answers.set(answers);
    }

    /// Declares that the OpenSSH process using askpass `context` connects to
    /// the host `binding` describes. See the module documentation.
    pub fn bind_process(&self, context: &str, binding: Binding) {
        let mut bindings = lock(&self.bindings);
        // Processes that never report their end (file transfers) must not
        // make this grow for ever.
        if bindings.len() >= MAX_BINDINGS {
            bindings.clear();
        }
        bindings.insert(context.to_owned(), binding);
    }

    /// The process is over.
    pub fn unbind_process(&self, context: &str) {
        lock(&self.bindings).remove(context);
        lock(&self.auto_answered).retain(|(c, _)| c != context);
    }

    /// `askpass.reply`: deliver the user's answer (`None` cancels). Unknown or
    /// already answered ids are ignored. With `remember`, a password or key
    /// passphrase prompt of a saved host also saves the answer in the vault.
    pub fn reply(&self, id: u64, answer: Option<String>, remember: bool) {
        let Some(waiting) = lock(&self.pending).remove(&id) else {
            return;
        };
        if let (true, Some(value), Some((binding, kind)), Some(answers)) = (
            remember,
            answer.as_ref(),
            waiting.matched.as_ref(),
            self.answers.get(),
        ) {
            // A failure (a locked vault) only means nothing was saved.
            let _ = answers.remember(
                binding,
                *kind,
                easyssh_crypto::SecretText::new(value.clone()),
            );
        }
        let _ = waiting.sender.send(answer);
    }

    /// `askpass.unlock`, after the vault was unlocked: answer prompt `id`
    /// from it. Returns whether the prompt got its saved answer.
    pub fn answer_from_vault(&self, id: u64) -> bool {
        let Some(answers) = self.answers.get() else {
            return false;
        };
        let matched = lock(&self.pending)
            .get(&id)
            .and_then(|waiting| waiting.matched.clone());
        let Some((binding, kind)) = matched else {
            return false;
        };
        let Answer::Secret(secret) = answers.answer(&binding, kind) else {
            return false;
        };
        match lock(&self.pending).remove(&id) {
            Some(waiting) => {
                let _ = waiting.sender.send(Some(secret.expose().to_owned()));
                true
            }
            None => false,
        }
    }

    async fn serve(&self, stream: tokio::net::TcpStream) -> Option<()> {
        let (read, mut write) = stream.into_split();
        let mut reader = tokio::io::BufReader::new(read.take(MAX_REQUEST));
        let mut line = String::new();
        tokio::time::timeout(REQUEST_TIMEOUT, reader.read_line(&mut line))
            .await
            .ok()?
            .ok()?;
        let request: Request = serde_json::from_str(&line).ok()?;
        if !same(request.token.as_bytes(), self.token.as_bytes()) {
            return None;
        }
        let answer = self.ask(request).await.map(Zeroizing::new);
        let mut out = Zeroizing::new(serde_json::to_vec(&answer.as_deref()).ok()?);
        out.push(b'\n');
        write.write_all(&out).await.ok()
    }

    async fn ask(&self, request: Request) -> Option<String> {
        if lock(&self.cancelled).contains(&request.context) {
            return None;
        }
        let matched = lock(&self.bindings)
            .get(&request.context)
            .cloned()
            .and_then(|binding| {
                match_prompt(&binding, &request.prompt).map(|kind| (binding, kind))
            });
        let mut vault_locked = false;
        if let (Some((binding, kind)), Some(answers)) = (&matched, self.answers.get()) {
            let key = (request.context.clone(), *kind == SecretKind::Password);
            if lock(&self.auto_answered).insert(key) {
                let (answers, binding, kind) = (answers.clone(), binding.clone(), *kind);
                let found = tokio::task::spawn_blocking(move || answers.answer(&binding, kind))
                    .await
                    .unwrap_or(Answer::None);
                match found {
                    Answer::Secret(secret) => return Some(secret.expose().to_owned()),
                    Answer::Locked => {
                        vault_locked = true;
                        // Not used up: the unlock below may still answer it.
                        lock(&self.auto_answered)
                            .remove(&(request.context.clone(), kind == SecretKind::Password));
                    }
                    Answer::None => {}
                }
            }
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        let rememberable = matched.is_some() && self.answers.get().is_some();
        lock(&self.pending).insert(id, Waiting { sender, matched });
        let prompt = Prompt {
            id,
            host: request.host,
            confirm: request.prompt.contains("(yes/no"),
            prompt: request.prompt,
            kind: None,
            echo: None,
            rememberable,
            vault_locked,
        };
        if !(self.notify)(prompt) {
            lock(&self.pending).remove(&id);
            return None;
        }
        let answer = tokio::time::timeout(self.answer_timeout, receiver)
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
        lock(&self.pending).remove(&id);
        if answer.is_none() {
            lock(&self.cancelled).insert(request.context);
        }
        (self.resolved)(id);
        answer
    }
}

/// Constant-time equality for the token.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
