//! Providers: how a request is built and how a streamed answer is read.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-5-5";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Upper bound of an answer. A shell answer is short; this stops a runaway.
pub const MAX_ANSWER_TOKENS: u32 = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    #[default]
    Anthropic,
    /// Any server that speaks the OpenAI chat completions API.
    Openai,
    /// A local Ollama server.
    Ollama,
}

impl Provider {
    pub fn id(self) -> &'static str {
        match self {
            Provider::Anthropic => "anthropic",
            Provider::Openai => "openai",
            Provider::Ollama => "ollama",
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::Openai => "https://api.openai.com/v1",
            Provider::Ollama => "http://localhost:11434",
        }
    }

    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Anthropic => DEFAULT_ANTHROPIC_MODEL,
            Provider::Openai => "gpt-4o-mini",
            Provider::Ollama => "llama3.1",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfig {
    pub provider: Provider,
    pub model: String,
    /// Replaces the provider's default address (needed for OpenAI-compatible
    /// servers other than OpenAI). `None` or empty uses the default.
    #[serde(default)]
    pub base_url: Option<String>,
}

impl AiConfig {
    pub fn base_url(&self) -> String {
        match self.base_url.as_deref().map(str::trim) {
            Some(url) if !url.is_empty() => url.trim_end_matches('/').to_owned(),
            _ => self.provider.default_base_url().to_owned(),
        }
    }

    /// Whether a request needs an API key.
    pub fn needs_key(&self) -> bool {
        match self.provider {
            Provider::Anthropic => true,
            Provider::Ollama => false,
            // Servers of your own often have none; OpenAI itself does.
            Provider::Openai => self.base_url() == Provider::Openai.default_base_url(),
        }
    }

    /// Checks the address and model; the error is for the settings form.
    pub fn validate(&self) -> Result<(), String> {
        if self.model.trim().is_empty() {
            return Err("enter a model name".to_owned());
        }
        check_url(&self.base_url())
    }
}

/// Plain http is accepted only for this machine and private networks, so an
/// API key never travels in the clear across the internet.
fn check_url(url: &str) -> Result<(), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "the address must start with http:// or https://".to_owned())?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') {
        return Err("the address must be a host name, without a user name".to_owned());
    }
    match scheme {
        "https" => Ok(()),
        "http" if is_private_host(host_of(authority)) => Ok(()),
        "http" => {
            Err("use https:// unless the server is on this machine or your own network".to_owned())
        }
        _ => Err("the address must start with http:// or https://".to_owned()),
    }
}

fn host_of(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    authority.rsplit_once(':').map_or(authority, |(h, _)| h)
}

fn is_private_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") {
        return true;
    }
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return match ip {
            std::net::IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            std::net::IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
        };
    }
    false
}

/// The system prompt and the user message of one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Messages {
    pub system: String,
    pub user: String,
}

/// One HTTP POST, ready to send. `headers` may hold the API key, so this
/// type has no `Debug` output of them.
#[derive(Clone, PartialEq)]
pub struct HttpRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("headers", &"[REDACTED]")
            .field("body", &self.body)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum BuildError {
    #[error("no API key is saved for this provider")]
    NoKey,
    #[error("{0}")]
    Config(String),
}

/// Builds the streaming request of `provider`. The key goes only into
/// headers.
pub fn build_request(
    config: &AiConfig,
    key: Option<&str>,
    messages: &Messages,
) -> Result<HttpRequest, BuildError> {
    config.validate().map_err(BuildError::Config)?;
    let key = key.map(str::trim).filter(|k| !k.is_empty());
    if config.needs_key() && key.is_none() {
        return Err(BuildError::NoKey);
    }
    let base = config.base_url();
    let model = config.model.trim();
    let mut headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
    let (url, body) = match config.provider {
        Provider::Anthropic => {
            headers.push(("x-api-key".to_owned(), key.unwrap_or_default().to_owned()));
            headers.push(("anthropic-version".to_owned(), ANTHROPIC_VERSION.to_owned()));
            (
                format!("{base}/v1/messages"),
                json!({
                    "model": model,
                    "max_tokens": MAX_ANSWER_TOKENS,
                    "stream": true,
                    "system": messages.system,
                    "messages": [{"role": "user", "content": messages.user}],
                }),
            )
        }
        Provider::Openai => {
            if let Some(key) = key {
                headers.push(("Authorization".to_owned(), format!("Bearer {key}")));
            }
            (
                format!("{base}/chat/completions"),
                json!({
                    "model": model,
                    "stream": true,
                    "max_tokens": MAX_ANSWER_TOKENS,
                    "messages": [
                        {"role": "system", "content": messages.system},
                        {"role": "user", "content": messages.user},
                    ],
                }),
            )
        }
        Provider::Ollama => (
            format!("{base}/api/chat"),
            json!({
                "model": model,
                "stream": true,
                "messages": [
                    {"role": "system", "content": messages.system},
                    {"role": "user", "content": messages.user},
                ],
            }),
        ),
    };
    Ok(HttpRequest { url, headers, body })
}

/// What one line of a streamed answer means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Text(String),
    Done,
    /// The provider reported an error inside the stream.
    Failed(String),
    Nothing,
}

/// Reads one line of the response body. Anthropic and OpenAI stream server
/// sent events (`data: {...}` lines, blank separators and `event:` lines are
/// ignored); Ollama streams one JSON object per line.
pub fn read_line(provider: Provider, line: &str) -> Piece {
    let line = line.trim();
    let json = match provider {
        Provider::Ollama => line,
        _ => match line.strip_prefix("data:") {
            Some(data) => data.trim(),
            None => return Piece::Nothing,
        },
    };
    if json.is_empty() {
        return Piece::Nothing;
    }
    if json == "[DONE]" {
        return Piece::Done;
    }
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Piece::Nothing;
    };
    if let Some(message) = value
        .pointer("/error/message")
        .and_then(Value::as_str)
        .or_else(|| value.get("error").and_then(Value::as_str))
    {
        return Piece::Failed(message.to_owned());
    }
    match provider {
        Provider::Anthropic => match value.get("type").and_then(Value::as_str) {
            Some("content_block_delta") => value
                .pointer("/delta/text")
                .and_then(Value::as_str)
                .map_or(Piece::Nothing, |t| Piece::Text(t.to_owned())),
            Some("message_stop") => Piece::Done,
            _ => Piece::Nothing,
        },
        Provider::Openai => {
            let choice = value.pointer("/choices/0");
            let text = choice
                .and_then(|c| c.pointer("/delta/content"))
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty());
            match text {
                Some(t) => Piece::Text(t.to_owned()),
                None if choice
                    .and_then(|c| c.get("finish_reason"))
                    .is_some_and(|r| !r.is_null()) =>
                {
                    Piece::Done
                }
                None => Piece::Nothing,
            }
        }
        Provider::Ollama => {
            let text = value
                .pointer("/message/content")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty());
            match text {
                Some(t) => Piece::Text(t.to_owned()),
                None if value.get("done").and_then(Value::as_bool) == Some(true) => Piece::Done,
                None => Piece::Nothing,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(provider: Provider, base: Option<&str>) -> AiConfig {
        AiConfig {
            provider,
            model: "m1".into(),
            base_url: base.map(str::to_owned),
        }
    }

    fn messages() -> Messages {
        Messages {
            system: "sys".into(),
            user: "usr".into(),
        }
    }

    #[test]
    fn anthropic_request_shape() {
        let r = build_request(&config(Provider::Anthropic, None), Some("k1"), &messages()).unwrap();
        assert_eq!(r.url, "https://api.anthropic.com/v1/messages");
        assert!(r.headers.contains(&("x-api-key".into(), "k1".into())));
        assert!(r
            .headers
            .contains(&("anthropic-version".into(), ANTHROPIC_VERSION.into())));
        assert_eq!(r.body["model"], "m1");
        assert_eq!(r.body["stream"], true);
        assert_eq!(r.body["system"], "sys");
        assert_eq!(r.body["messages"][0]["content"], "usr");
        // The key is in a header and nowhere else.
        assert!(!r.body.to_string().contains("k1"));
        assert!(!r.url.contains("k1"));
        assert!(!format!("{r:?}").contains("k1"));
    }

    #[test]
    fn openai_compatible_uses_the_custom_base_url_and_bearer() {
        let r = build_request(
            &config(Provider::Openai, Some("https://llm.example.test/v1/")),
            Some("k2"),
            &messages(),
        )
        .unwrap();
        assert_eq!(r.url, "https://llm.example.test/v1/chat/completions");
        assert!(r
            .headers
            .contains(&("Authorization".into(), "Bearer k2".into())));
        assert_eq!(r.body["messages"][0]["role"], "system");
        assert_eq!(r.body["messages"][1]["content"], "usr");
        assert!(!r.body.to_string().contains("k2"));
    }

    #[test]
    fn a_custom_openai_server_may_have_no_key_but_openai_itself_needs_one() {
        let own = config(Provider::Openai, Some("http://127.0.0.1:8000/v1"));
        let r = build_request(&own, None, &messages()).unwrap();
        assert!(!r.headers.iter().any(|(n, _)| n == "Authorization"));
        assert_eq!(
            build_request(&config(Provider::Openai, None), None, &messages()),
            Err(BuildError::NoKey)
        );
        assert_eq!(
            build_request(&config(Provider::Anthropic, None), Some("  "), &messages()),
            Err(BuildError::NoKey)
        );
    }

    #[test]
    fn ollama_is_local_and_keyless() {
        let r = build_request(&config(Provider::Ollama, None), None, &messages()).unwrap();
        assert_eq!(r.url, "http://localhost:11434/api/chat");
        assert_eq!(r.headers.len(), 1);
        assert_eq!(r.body["model"], "m1");
    }

    #[test]
    fn plain_http_is_for_private_hosts_only() {
        for ok in [
            "http://localhost:11434",
            "http://127.0.0.1:1234/v1",
            "http://192.168.1.5:8080",
            "http://10.0.0.2",
            "http://[::1]:11434",
            "http://box.local:1",
            "https://api.example.com/v1",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://api.example.com",
            "http://8.8.8.8",
            "ftp://x",
            "api.example.com",
            "https://user:pw@example.com",
            "https://",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
        assert!(build_request(
            &config(Provider::Openai, Some("http://example.com/v1")),
            Some("k"),
            &messages()
        )
        .is_err());
        let mut c = config(Provider::Anthropic, None);
        c.model = " ".into();
        assert!(c.validate().is_err());
    }

    #[test]
    fn reads_anthropic_events() {
        let p = Provider::Anthropic;
        assert_eq!(read_line(p, "event: content_block_delta"), Piece::Nothing);
        assert_eq!(
            read_line(
                p,
                r#"data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"ls -la"}}"#
            ),
            Piece::Text("ls -la".into())
        );
        assert_eq!(
            read_line(p, r#"data: {"type":"message_stop"}"#),
            Piece::Done
        );
        assert_eq!(read_line(p, r#"data: {"type":"ping"}"#), Piece::Nothing);
        assert_eq!(
            read_line(
                p,
                r#"data: {"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
            ),
            Piece::Failed("Overloaded".into())
        );
    }

    #[test]
    fn reads_openai_chunks() {
        let p = Provider::Openai;
        assert_eq!(
            read_line(p, r#"data: {"choices":[{"delta":{"content":"hi"}}]}"#),
            Piece::Text("hi".into())
        );
        assert_eq!(
            read_line(p, r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#),
            Piece::Nothing
        );
        assert_eq!(
            read_line(
                p,
                r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}]}"#
            ),
            Piece::Done
        );
        assert_eq!(read_line(p, "data: [DONE]"), Piece::Done);
        assert_eq!(
            read_line(p, r#"data: {"error":{"message":"bad key"}}"#),
            Piece::Failed("bad key".into())
        );
    }

    #[test]
    fn reads_ollama_lines() {
        let p = Provider::Ollama;
        assert_eq!(
            read_line(
                p,
                r#"{"message":{"role":"assistant","content":"df"},"done":false}"#
            ),
            Piece::Text("df".into())
        );
        assert_eq!(
            read_line(p, r#"{"message":{"content":""},"done":true}"#),
            Piece::Done
        );
        assert_eq!(
            read_line(p, r#"{"error":"model 'x' not found"}"#),
            Piece::Failed("model 'x' not found".into())
        );
        assert_eq!(read_line(p, "not json"), Piece::Nothing);
    }
}
