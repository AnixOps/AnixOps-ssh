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
//! are never stored and never logged.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, Semaphore};

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
}

/// Delivers a prompt to whoever can answer it; `false` when nobody can.
pub type Notify = Arc<dyn Fn(Prompt) -> bool + Send + Sync>;

/// Told when a prompt is over (answered, cancelled or timed out), so that
/// every client that was shown it can close its dialog.
pub type Resolved = Arc<dyn Fn(u64) + Send + Sync>;

/// The daemon side: a loopback listener plus the table of open prompts.
pub struct Hub {
    port: u16,
    token: String,
    notify: Notify,
    resolved: Resolved,
    pending: Mutex<HashMap<u64, oneshot::Sender<Option<String>>>>,
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

    /// `askpass.reply`: deliver the user's answer (`None` cancels). Unknown or
    /// already answered ids are ignored.
    pub fn reply(&self, id: u64, answer: Option<String>) {
        if let Some(sender) = lock(&self.pending).remove(&id) {
            let _ = sender.send(answer);
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
        let answer = self.ask(request).await;
        let mut out = serde_json::to_vec(&answer).ok()?;
        out.push(b'\n');
        write.write_all(&out).await.ok()
    }

    async fn ask(&self, request: Request) -> Option<String> {
        if lock(&self.cancelled).contains(&request.context) {
            return None;
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();
        lock(&self.pending).insert(id, sender);
        let prompt = Prompt {
            id,
            host: request.host,
            confirm: request.prompt.contains("(yes/no"),
            prompt: request.prompt,
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
