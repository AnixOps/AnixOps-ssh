//! Requests against a local mock HTTP server: what each provider receives,
//! how its stream is read, and what errors look like. No real network.

use easyssh_ai::prompt::{prepare, ContextKind, ContextPiece};
use easyssh_ai::provider::build_request;
use easyssh_ai::{stream, AiConfig, AiError, Cancel, Limits, Provider};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Mock {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    fn base(&self) -> String {
        format!("http://{}", self.addr)
    }
    fn last(&self) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .last()
            .cloned()
            .expect("a request")
    }
}

/// Answers every request with `status`, then writes each body piece after
/// its delay and closes the connection.
fn serve(status: u16, extra_header: &'static str, pieces: Vec<(u64, String)>) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let pieces = pieces.clone();
            let log = log.clone();
            std::thread::spawn(move || {
                let Some(request) = read_request(&mut stream) else {
                    return;
                };
                log.lock().unwrap().push(request);
                let head =
                    format!("HTTP/1.1 {status} X\r\nConnection: close\r\n{extra_header}\r\n");
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for (delay, piece) in pieces {
                    std::thread::sleep(Duration::from_millis(delay));
                    if stream.write_all(piece.as_bytes()).is_err() || stream.flush().is_err() {
                        return;
                    }
                }
            });
        }
    });
    Mock { addr, seen }
}

fn read_request(stream: &mut TcpStream) -> Option<Seen> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut first = String::new();
    reader.read_line(&mut first).ok()?;
    let path = first.split_whitespace().nth(1)?.to_owned();
    let mut headers = Vec::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok()?;
        }
        headers.push((name.to_owned(), value.trim().to_owned()));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Seen {
        path,
        headers,
        body: String::from_utf8(body).ok()?,
    })
}

fn sse(data: &str) -> (u64, String) {
    (0, format!("data: {data}\n\n"))
}

fn config(provider: Provider, base: &str) -> AiConfig {
    AiConfig {
        provider,
        model: "test-model".into(),
        base_url: Some(base.to_owned()),
    }
}

fn ask(
    provider: Provider,
    base: &str,
    key: Option<&str>,
    limits: Limits,
    cancel: &Cancel,
    on_text: &mut dyn FnMut(&str),
) -> Result<(), AiError> {
    let prepared = prepare(
        "why did it fail?",
        &[ContextPiece {
            kind: ContextKind::Command,
            text: "$ make\nerror: boom".into(),
        }],
    )
    .unwrap();
    let request = build_request(&config(provider, base), key, &prepared.messages()).unwrap();
    stream(provider, &request, key, limits, cancel, on_text)
}

fn collect(provider: Provider, base: &str, key: Option<&str>) -> Result<String, AiError> {
    let mut text = String::new();
    ask(
        provider,
        base,
        key,
        Limits::default(),
        &Cancel::new(),
        &mut |piece| text.push_str(piece),
    )
    .map(|()| text)
}

#[test]
fn anthropic_request_and_stream() {
    let mock = serve(
        200,
        "Content-Type: text/event-stream\r\n",
        vec![
            (
                0,
                "event: message_start\ndata: {\"type\":\"message_start\"}\n\n".into(),
            ),
            sse(r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Run "}}"#),
            (20, "event: content_block_delta\n".into()),
            sse(
                r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"`make -j1`."}}"#,
            ),
            sse(r#"{"type":"message_stop"}"#),
        ],
    );
    let text = collect(Provider::Anthropic, &mock.base(), Some("sk-ant-test-key-1")).unwrap();
    assert_eq!(text, "Run `make -j1`.");
    let seen = mock.last();
    assert_eq!(seen.path, "/v1/messages");
    assert_eq!(seen.header("x-api-key"), Some("sk-ant-test-key-1"));
    assert_eq!(seen.header("anthropic-version"), Some("2023-06-01"));
    let body: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert!(body["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("error: boom"));
    assert!(!seen.body.contains("sk-ant-test-key-1"), "key in the body");
}

#[test]
fn openai_compatible_request_and_stream() {
    let mock = serve(
        200,
        "Content-Type: text/event-stream\r\n",
        vec![
            sse(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#),
            sse(r#"{"choices":[{"delta":{"content":"df "}}]}"#),
            sse(r#"{"choices":[{"delta":{"content":"-h"},"finish_reason":null}]}"#),
            sse(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            (0, "data: [DONE]\n\n".into()),
        ],
    );
    let base = format!("{}/v1", mock.base());
    let text = collect(Provider::Openai, &base, Some("key-openai-1")).unwrap();
    assert_eq!(text, "df -h");
    let seen = mock.last();
    assert_eq!(seen.path, "/v1/chat/completions");
    assert_eq!(seen.header("authorization"), Some("Bearer key-openai-1"));
    let body: serde_json::Value = serde_json::from_str(&seen.body).unwrap();
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["stream"], true);
}

#[test]
fn ollama_request_and_stream_need_no_key() {
    let mock = serve(
        200,
        "Content-Type: application/x-ndjson\r\n",
        vec![
            (
                0,
                "{\"message\":{\"role\":\"assistant\",\"content\":\"uptime\"},\"done\":false}\n"
                    .into(),
            ),
            (
                0,
                "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true}\n".into(),
            ),
        ],
    );
    let text = collect(Provider::Ollama, &mock.base(), None).unwrap();
    assert_eq!(text, "uptime");
    let seen = mock.last();
    assert_eq!(seen.path, "/api/chat");
    assert!(seen.header("authorization").is_none());
    assert!(seen.header("x-api-key").is_none());
}

#[test]
fn a_rejected_key_gives_a_clear_message_without_the_key() {
    let key = "sk-ant-very-secret-key";
    let body = format!(r#"{{"error":{{"message":"invalid x-api-key {key}"}}}}"#);
    let mock = serve(401, "Content-Type: application/json\r\n", vec![(0, body)]);
    let error = collect(Provider::Anthropic, &mock.base(), Some(key)).unwrap_err();
    let AiError::Failed(message) = &error else {
        panic!("{error:?}")
    };
    assert!(message.contains("rejected the API key"), "{message}");
    assert!(message.contains("401"), "{message}");
    assert!(!message.contains(key), "the key leaked: {message}");
    assert!(!format!("{error:?}").contains(key));
}

#[test]
fn other_statuses_have_their_own_words() {
    for (status, needle) in [
        (404, "not found"),
        (429, "too many requests"),
        (503, "had an error"),
    ] {
        let mock = serve(status, "", vec![(0, String::new())]);
        let error = collect(Provider::Ollama, &mock.base(), None).unwrap_err();
        assert!(error.to_string().contains(needle), "{status}: {error}");
        assert!(error.to_string().contains(&status.to_string()));
    }
}

#[test]
fn an_error_event_in_the_stream_is_reported() {
    let mock = serve(
        200,
        "Content-Type: text/event-stream\r\n",
        vec![
            sse(r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"x"}}"#),
            sse(r#"{"type":"error","error":{"message":"Overloaded"}}"#),
        ],
    );
    let error = collect(Provider::Anthropic, &mock.base(), Some("k-123456")).unwrap_err();
    assert_eq!(error, AiError::Failed("Overloaded".into()));
}

#[test]
fn a_silent_server_times_out() {
    let mock = serve(
        200,
        "Content-Type: text/event-stream\r\n",
        vec![(2500, "data: {}\n\n".into())],
    );
    let limits = Limits {
        connect: Duration::from_secs(2),
        idle: Duration::from_millis(300),
        total: Duration::from_secs(10),
    };
    let started = Instant::now();
    let error = ask(
        Provider::Anthropic,
        &mock.base(),
        Some("k-123456"),
        limits,
        &Cancel::new(),
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.to_string().contains("sent nothing"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn cancel_stops_the_answer() {
    let mock = serve(
        200,
        "Content-Type: text/event-stream\r\n",
        (0..50)
            .map(|_| {
                (
                    20,
                    "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"a\"}}\n\n"
                        .to_owned(),
                )
            })
            .collect(),
    );
    let cancel = Cancel::new();
    let mut received = 0;
    let error = ask(
        Provider::Anthropic,
        &mock.base(),
        Some("k-123456"),
        Limits::default(),
        &cancel,
        &mut |_| {
            received += 1;
            if received == 3 {
                cancel.cancel();
            }
        },
    )
    .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(received, 3);
    // Cancelled before it starts: nothing is sent.
    let before = mock.seen.lock().unwrap().len();
    let error = ask(
        Provider::Anthropic,
        &mock.base(),
        Some("k-123456"),
        Limits::default(),
        &cancel,
        &mut |_| {},
    )
    .unwrap_err();
    assert!(error.is_cancelled());
    assert_eq!(mock.seen.lock().unwrap().len(), before);
}

#[test]
fn a_closed_port_says_it_cannot_connect() {
    let port = {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let error = collect(Provider::Ollama, &format!("http://127.0.0.1:{port}"), None).unwrap_err();
    assert!(error.to_string().contains("Cannot connect"), "{error}");
}

#[test]
fn redirects_are_not_followed() {
    let other = serve(
        200,
        "",
        vec![(0, "data: {\"type\":\"message_stop\"}\n\n".into())],
    );
    let target = format!("{}/v1/messages", other.base());
    let header: &'static str = Box::leak(format!("Location: {target}\r\n").into_boxed_str());
    let mock = serve(307, header, vec![(0, String::new())]);
    let error = collect(Provider::Anthropic, &mock.base(), Some("k-123456")).unwrap_err();
    assert!(error.to_string().contains("307"), "{error}");
    assert!(
        other.seen.lock().unwrap().is_empty(),
        "the redirect was followed"
    );
}

#[test]
fn what_is_sent_is_the_scrubbed_prompt() {
    let mock = serve(
        200,
        "",
        vec![(0, "data: {\"type\":\"content_block_delta\",\"delta\":{\"text\":\"ok\"}}\n\ndata: {\"type\":\"message_stop\"}\n\n".into())],
    );
    let prepared = prepare(
        "what does this do?",
        &[ContextPiece {
            kind: ContextKind::Lines,
            text: "export AWS_SECRET_ACCESS_KEY=abcd1234efgh5678\nssh deploy@10.0.0.7".into(),
        }],
    )
    .unwrap();
    let request = build_request(
        &config(Provider::Anthropic, &mock.base()),
        Some("k-123456"),
        &prepared.messages(),
    )
    .unwrap();
    // The review text and the request body carry the same prompt.
    assert_eq!(
        request.body["messages"][0]["content"],
        prepared.user.as_str()
    );
    stream(
        Provider::Anthropic,
        &request,
        Some("k-123456"),
        Limits::default(),
        &Cancel::new(),
        &mut |_| {},
    )
    .unwrap();
    let sent = mock.last().body;
    assert!(!sent.contains("abcd1234efgh5678"), "{sent}");
    assert!(sent.contains("[REDACTED:secret-value]"));
    assert!(
        sent.contains("deploy@10.0.0.7"),
        "hosts are kept for the model"
    );
}
