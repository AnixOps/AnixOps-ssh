//! PuTTY sessions from a `.reg` export of
//! `HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions`.

use crate::model::{clean_path, ImportReport, ImportWarning, ImportedHost};
use crate::util::{decode_text, url_decode};
use std::collections::HashMap;
use std::path::PathBuf;

const SESSIONS_MARKER: &str = "\\putty\\sessions\\";

#[derive(Debug, Clone)]
enum Value {
    Str(String),
    Dword(u32),
}

impl Value {
    fn text(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            Value::Dword(_) => None,
        }
    }
}

struct Session {
    name: String,
    line: usize,
    values: HashMap<String, Value>,
}

pub fn detect(bytes: &[u8]) -> bool {
    let text = decode_text(bytes).to_ascii_lowercase();
    (text.contains("windows registry editor") || text.contains("regedit4"))
        && text.contains("simontatham")
}

pub fn parse(bytes: &[u8]) -> ImportReport {
    let text = decode_text(bytes);
    let mut report = ImportReport::default();
    let mut sessions: Vec<Session> = Vec::new();
    let mut in_session = false;
    let mut continuation = false;

    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if continuation {
            continuation = line.ends_with('\\');
            continue;
        }
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let path = rest.trim_end_matches(']');
            let lower = path.to_ascii_lowercase();
            in_session = false;
            if let Some(pos) = lower.find(SESSIONS_MARKER) {
                let encoded = &path[pos + SESSIONS_MARKER.len()..];
                if !encoded.is_empty() && !encoded.contains('\\') {
                    sessions.push(Session {
                        name: url_decode(encoded),
                        line: number,
                        values: HashMap::new(),
                    });
                    in_session = true;
                }
            }
            continue;
        }
        if !in_session {
            continue;
        }
        let Some((key, rest)) = parse_quoted(line) else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim();
        let parsed = if value.starts_with('"') {
            parse_quoted(value).map(|(s, _)| Value::Str(s))
        } else if let Some(hex) = value.strip_prefix("dword:") {
            u32::from_str_radix(hex.trim(), 16).ok().map(Value::Dword)
        } else {
            if value.starts_with("hex") {
                continuation = value.ends_with('\\');
            }
            None
        };
        if let (Some(parsed), Some(session)) = (parsed, sessions.last_mut()) {
            session.values.insert(key, parsed);
        }
    }

    if sessions.is_empty() {
        report.warn(ImportWarning::new(
            "no PuTTY sessions found; export HKCU\\Software\\SimonTatham\\PuTTY\\Sessions",
        ));
    }
    for session in sessions {
        convert(session, &mut report);
    }
    report.finish()
}

/// Reads a `"..."` token with `\\` and `\"` escapes; returns the rest.
fn parse_quoted(text: &str) -> Option<(String, &str)> {
    let body = text.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &body[i + 1..])),
            '\\' => match chars.next() {
                Some((_, escaped)) => out.push(escaped),
                None => return None,
            },
            _ => out.push(c),
        }
    }
    None
}

fn convert(session: Session, report: &mut ImportReport) {
    let name = session.name.clone();
    if name.eq_ignore_ascii_case("Default Settings") {
        return;
    }
    let text = |key: &str| {
        session
            .values
            .get(key)
            .and_then(Value::text)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let protocol = text("Protocol").unwrap_or("ssh");
    if !protocol.eq_ignore_ascii_case("ssh") {
        report.skipped += 1;
        report.warn(ImportWarning::for_record(
            &name,
            format!("protocol {protocol:?} is not SSH; skipped"),
        ));
        return;
    }
    let Some(hostname) = text("HostName") else {
        report.skipped += 1;
        report.warn(ImportWarning {
            line: Some(session.line),
            record: Some(name),
            message: "session has no HostName; skipped".into(),
        });
        return;
    };

    // Folder convention of PuTTY forks and managers: `Folder/Name`.
    let mut parts = clean_path(name.split('/'));
    let label = parts.pop().unwrap_or_else(|| name.clone());
    let mut host = ImportedHost::new(label, hostname);
    host.group_path = parts;

    if let Some((user, rest)) = hostname.rsplit_once('@') {
        host.hostname = rest.to_owned();
        host.username = Some(user.to_owned());
    } else if let Some(user) = text("UserName") {
        host.username = Some(user.to_owned());
    }
    host.port = match session.values.get("PortNumber") {
        Some(Value::Dword(port)) => match u16::try_from(*port) {
            Ok(port) if port != 0 => port,
            _ => {
                report.warn(ImportWarning::for_record(
                    &name,
                    format!("invalid PortNumber {port}; using 22"),
                ));
                22
            }
        },
        _ => 22,
    };
    if let Some(key) = text("PublicKeyFile") {
        host.identity_file = Some(PathBuf::from(key));
    }
    match session.values.get("ProxyMethod") {
        Some(Value::Dword(4 | 5)) => host.openssh_only.push("ProxyCommand".into()),
        Some(Value::Dword(1..=3)) => report.warn(ImportWarning::for_record(
            &name,
            "SOCKS/HTTP proxy settings are not imported",
        )),
        _ => {}
    }
    if text("ProxyPassword").is_some() {
        report.secret_skipped(&name, "proxy password");
    }
    report.push_host(host);
}
