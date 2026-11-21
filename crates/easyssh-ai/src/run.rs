//! The HTTP call: one streamed request, with timeouts, cancel and messages a
//! person can act on. Blocking; callers run it on their own thread.

use crate::provider::{read_line, HttpRequest, Piece, Provider};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How often a waiting stream looks at the cancel flag and the deadline.
pub const POLL: Duration = Duration::from_millis(50);
/// The largest single line (event) accepted from the provider.
pub const MAX_LINE: usize = 1024 * 1024;
const CHUNK: usize = 16 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub connect: Duration,
    /// How long the server may stay silent in the middle of an answer.
    pub idle: Duration,
    /// The whole answer.
    pub total: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            connect: Duration::from_secs(10),
            idle: Duration::from_secs(60),
            total: Duration::from_secs(300),
        }
    }
}

/// Stops a running [`stream`] within [`POLL`], even in the middle of a read.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn new() -> Cancel {
        Cancel::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AiError {
    #[error("{0}")]
    Config(String),
    #[error("cancelled")]
    Cancelled,
    /// The idle limit or the whole-answer limit ran out.
    #[error("{0}")]
    Timeout(String),
    /// The provider sent something that is not a valid stream.
    #[error("{0}")]
    Malformed(String),
    #[error("{0}")]
    Failed(String),
}

impl AiError {
    pub fn is_cancelled(&self) -> bool {
        matches!(self, AiError::Cancelled)
    }
}

fn clean(message: &str, key: Option<&str>) -> String {
    let mut text = message.trim().to_owned();
    if let Some(key) = key.map(str::trim).filter(|k| k.len() >= 4) {
        text = text.replace(key, "[key]");
    }
    if text.chars().count() > 400 {
        text = text.chars().take(400).collect::<String>() + "…";
    }
    text
}

/// What the provider said about an error status, from a JSON body when it
/// has one.
fn error_text(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .pointer("/error/message")
        .or_else(|| value.get("error"))
        .or_else(|| value.get("message"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
}

fn status_message(status: u16, provider: Provider, detail: Option<String>) -> String {
    let what = match status {
        400 => "The provider did not accept the request (check the model name)",
        401 | 403 => "The provider rejected the API key",
        404 => match provider {
            Provider::Ollama => "The model or the address was not found (is the model pulled?)",
            _ => "The address or the model was not found",
        },
        408 | 504 => "The provider timed out",
        413 => "The request is too large for this model",
        429 => "The provider says there are too many requests or the quota is used up",
        500..=599 => "The provider had an error",
        _ => "The provider answered with an error",
    };
    match detail {
        Some(detail) if !detail.is_empty() => format!("{what} (HTTP {status}): {detail}"),
        _ => format!("{what} (HTTP {status})"),
    }
}

fn transport_message(error: &ureq::Transport, base: &str) -> String {
    use ureq::ErrorKind::*;
    match error.kind() {
        Dns => format!("Cannot find {base}: check the address and your network"),
        ConnectionFailed => format!("Cannot connect to {base}: is the server running?"),
        InsecureRequestHttpsOnly | InvalidUrl | UnknownScheme => {
            "The address is not valid".to_owned()
        }
        TooManyRedirects => "The server redirected the request, which is not followed".to_owned(),
        Io => "The connection was lost or timed out".to_owned(),
        _ => "The request could not be sent".to_owned(),
    }
}

/// Sends `request` and calls `on_text` with each piece of the answer as it
/// arrives. `key`, when given, is removed from any message returned.
pub fn stream(
    provider: Provider,
    request: &HttpRequest,
    key: Option<&str>,
    limits: Limits,
    cancel: &Cancel,
    on_text: &mut dyn FnMut(&str),
) -> Result<(), AiError> {
    if cancel.is_cancelled() {
        return Err(AiError::Cancelled);
    }
    let started = Instant::now();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(limits.connect)
        .timeout_read(limits.idle)
        .timeout_write(limits.connect)
        // A redirect could carry the key somewhere the user did not name.
        .redirects(0)
        .user_agent(concat!("easyssh-ai/", env!("CARGO_PKG_VERSION")))
        .build();
    let mut call = agent.post(&request.url);
    for (name, value) in &request.headers {
        call = call.set(name, value);
    }
    let base = request
        .url
        .split("://")
        .nth(1)
        .and_then(|rest| rest.split('/').next())
        .unwrap_or("the server")
        .to_owned();
    let response = match call.send_string(&request.body.to_string()) {
        Ok(response) => response,
        Err(ureq::Error::Status(status, response)) => {
            let mut body = String::new();
            let _ = response.into_reader().take(8192).read_to_string(&mut body);
            let detail = error_text(&body).map(|d| clean(&d, key));
            return Err(AiError::Failed(status_message(status, provider, detail)));
        }
        Err(ureq::Error::Transport(transport)) => {
            return Err(AiError::Failed(clean(
                &transport_message(&transport, &base),
                key,
            )));
        }
    };
    if !(200..300).contains(&response.status()) {
        // Redirects are not followed, so a 3xx arrives here as a response.
        return Err(AiError::Failed(status_message(
            response.status(),
            provider,
            None,
        )));
    }
    pump(
        provider,
        response.into_reader(),
        key,
        limits,
        started,
        cancel,
        on_text,
    )
}

type Chunk = std::io::Result<Vec<u8>>;

/// Reads `source` on a helper thread in bounded chunks so that cancel, the
/// idle limit and the absolute deadline are honoured while a read is blocked
/// or while bytes trickle in without a newline. The helper ends when the read
/// fails, ends, or the receiver is dropped.
fn pump<R: Read + Send + 'static>(
    provider: Provider,
    mut source: R,
    key: Option<&str>,
    limits: Limits,
    started: Instant,
    cancel: &Cancel,
    on_text: &mut dyn FnMut(&str),
) -> Result<(), AiError> {
    let (tx, rx) = sync_channel::<Chunk>(4);
    std::thread::spawn(move || {
        let mut buf = vec![0u8; CHUNK];
        loop {
            let message = match source.read(&mut buf) {
                Ok(0) => Ok(Vec::new()),
                Ok(n) => Ok(buf[..n].to_vec()),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => Err(e),
            };
            let end = !matches!(&message, Ok(v) if !v.is_empty());
            if tx.send(message).is_err() || end {
                return;
            }
        }
    });
    let mut pending: Vec<u8> = Vec::new();
    let mut got_text = false;
    let mut last_data = Instant::now();
    let mut handle = |raw: &[u8], got_text: &mut bool| -> Result<bool, AiError> {
        let line = std::str::from_utf8(raw).map_err(|_| {
            AiError::Malformed("The provider sent text that is not valid UTF-8".to_owned())
        })?;
        match read_line(provider, line) {
            Piece::Text(text) => {
                *got_text = true;
                on_text(&text);
            }
            Piece::Done => return Ok(true),
            Piece::Failed(message) => return Err(AiError::Failed(clean(&message, key))),
            Piece::Nothing => {}
        }
        Ok(false)
    };
    loop {
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        let elapsed = started.elapsed();
        if elapsed >= limits.total {
            return Err(AiError::Timeout(format!(
                "The answer took longer than {} seconds and was stopped",
                limits.total.as_secs()
            )));
        }
        if last_data.elapsed() >= limits.idle {
            return Err(AiError::Timeout(format!(
                "The provider sent nothing for {} seconds",
                limits.idle.as_secs()
            )));
        }
        let wait = POLL.min(limits.total - elapsed);
        let chunk = match rx.recv_timeout(wait) {
            Ok(chunk) => chunk,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let bytes = match chunk {
            Ok(bytes) => bytes,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(AiError::Timeout(format!(
                    "The provider sent nothing for {} seconds",
                    limits.idle.as_secs()
                )));
            }
            Err(_) => {
                return Err(AiError::Failed(
                    "The connection was lost during the answer".to_owned(),
                ));
            }
        };
        if bytes.is_empty() {
            break;
        }
        last_data = Instant::now();
        // Lines are split on raw bytes, so a multi-byte character cut by a
        // chunk boundary is whole again by the time the line is decoded.
        let mut rest = &bytes[..];
        while let Some(at) = rest.iter().position(|b| *b == b'\n') {
            pending.extend_from_slice(&rest[..at]);
            rest = &rest[at + 1..];
            if pending.len() > MAX_LINE {
                return Err(too_long());
            }
            let done = handle(&pending, &mut got_text)?;
            pending.clear();
            if done {
                return Ok(());
            }
            if cancel.is_cancelled() {
                return Err(AiError::Cancelled);
            }
        }
        pending.extend_from_slice(rest);
        if pending.len() > MAX_LINE {
            return Err(too_long());
        }
    }
    if !pending.is_empty() && handle(&pending, &mut got_text)? {
        return Ok(());
    }
    if got_text {
        // The server closed without a final marker; what arrived is the answer.
        Ok(())
    } else {
        Err(AiError::Failed(
            "The provider ended the answer without any text".to_owned(),
        ))
    }
}

fn too_long() -> AiError {
    AiError::Malformed(format!(
        "The provider sent a single event larger than {} KiB",
        MAX_LINE / 1024
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn limits(idle_ms: u64, total_ms: u64) -> Limits {
        Limits {
            connect: Duration::from_secs(1),
            idle: Duration::from_millis(idle_ms),
            total: Duration::from_millis(total_ms),
        }
    }

    fn run<R: Read + Send + 'static>(
        reader: R,
        limits: Limits,
        cancel: &Cancel,
    ) -> (Result<(), AiError>, String) {
        let mut out = String::new();
        let result = pump(
            Provider::Anthropic,
            reader,
            None,
            limits,
            Instant::now(),
            cancel,
            &mut |t| out.push_str(t),
        );
        (result, out)
    }

    fn event(text: &str) -> String {
        format!("data: {{\"type\":\"content_block_delta\",\"delta\":{{\"text\":\"{text}\"}}}}\n\n")
    }

    /// Yields the given pieces one `read` at a time.
    struct Pieces(std::vec::IntoIter<Vec<u8>>);
    impl Read for Pieces {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.0.next() {
                Some(p) => {
                    buf[..p.len()].copy_from_slice(&p);
                    Ok(p.len())
                }
                None => Ok(0),
            }
        }
    }

    /// One byte every few milliseconds, never a newline.
    struct Trickle;
    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_millis(5));
            buf[0] = b'x';
            Ok(1)
        }
    }

    /// A read that blocks far longer than any limit under test.
    struct Stuck;
    impl Read for Stuck {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_secs(5));
            Ok(0)
        }
    }

    #[test]
    fn events_and_multibyte_text_survive_any_chunking() {
        let body = format!("{}{}{}", event("héllo "), event("世界 🎉"), event("!"));
        let bytes = body.into_bytes();
        for size in [1, 2, 3, 7, 64] {
            let pieces: Vec<Vec<u8>> = bytes.chunks(size).map(<[u8]>::to_vec).collect();
            let (result, out) = run(
                Pieces(pieces.into_iter()),
                limits(1000, 5000),
                &Cancel::new(),
            );
            assert_eq!(result, Ok(()), "chunk size {size}");
            assert_eq!(out, "héllo 世界 🎉!", "chunk size {size}");
        }
    }

    #[test]
    fn a_last_line_without_newline_is_read() {
        let body = event("a").trim_end().to_owned();
        let (result, out) = run(
            Cursor::new(body.into_bytes()),
            limits(1000, 5000),
            &Cancel::new(),
        );
        assert_eq!(result, Ok(()));
        assert_eq!(out, "a");
    }

    #[test]
    fn cancel_interrupts_a_blocked_read() {
        let cancel = Cancel::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            c.cancel();
        });
        let started = Instant::now();
        let (result, _) = run(Stuck, limits(10_000, 10_000), &cancel);
        assert_eq!(result, Err(AiError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn cancel_interrupts_a_trickle_without_newline() {
        let cancel = Cancel::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            c.cancel();
        });
        let started = Instant::now();
        let (result, _) = run(Trickle, limits(10_000, 10_000), &cancel);
        assert_eq!(result, Err(AiError::Cancelled));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn the_total_deadline_holds_while_data_trickles() {
        let started = Instant::now();
        let (result, _) = run(Trickle, limits(10_000, 300), &Cancel::new());
        assert!(matches!(result, Err(AiError::Timeout(_))), "{result:?}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_deadline_holds_in_a_blocked_read() {
        let started = Instant::now();
        let (result, _) = run(Stuck, limits(10_000, 200), &Cancel::new());
        assert!(matches!(result, Err(AiError::Timeout(m)) if m.contains("longer than")));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn the_idle_limit_holds_in_a_blocked_read() {
        let (result, _) = run(Stuck, limits(150, 10_000), &Cancel::new());
        assert!(matches!(result, Err(AiError::Timeout(m)) if m.contains("sent nothing")));
    }

    #[test]
    fn a_line_over_the_cap_is_a_format_error() {
        let big = vec![b'a'; MAX_LINE + 10];
        let (result, _) = run(Cursor::new(big), limits(1000, 5000), &Cancel::new());
        assert!(matches!(result, Err(AiError::Malformed(_))), "{result:?}");
        let mut big = vec![b'a'; MAX_LINE + 10];
        big.push(b'\n');
        let (result, _) = run(Cursor::new(big), limits(1000, 5000), &Cancel::new());
        assert!(matches!(result, Err(AiError::Malformed(_))), "{result:?}");
    }

    #[test]
    fn invalid_utf8_is_a_format_error() {
        let (result, _) = run(
            Cursor::new(vec![b'd', b'a', 0xff, 0xfe, b'\n']),
            limits(1000, 5000),
            &Cancel::new(),
        );
        assert!(matches!(result, Err(AiError::Malformed(_))), "{result:?}");
    }
}
