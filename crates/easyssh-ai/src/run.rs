//! The HTTP call: one streamed request, with timeouts, cancel and messages a
//! person can act on. Blocking; callers run it on their own thread.

use crate::provider::{read_line, HttpRequest, Piece, Provider};
use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Stops a running [`stream`] at its next line.
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
    let mut reader = BufReader::new(response.into_reader());
    let mut line = String::new();
    let mut got_text = false;
    loop {
        if cancel.is_cancelled() {
            return Err(AiError::Cancelled);
        }
        if started.elapsed() > limits.total {
            return Err(AiError::Failed(format!(
                "The answer took longer than {} seconds and was stopped",
                limits.total.as_secs()
            )));
        }
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(AiError::Failed(format!(
                    "The provider sent nothing for {} seconds",
                    limits.idle.as_secs()
                )));
            }
            Err(_) => {
                return Err(AiError::Failed(
                    "The connection was lost during the answer".to_owned(),
                ));
            }
        }
        match read_line(provider, &line) {
            Piece::Text(text) => {
                got_text = true;
                on_text(&text);
            }
            Piece::Done => return Ok(()),
            Piece::Failed(message) => return Err(AiError::Failed(clean(&message, key))),
            Piece::Nothing => {}
        }
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
