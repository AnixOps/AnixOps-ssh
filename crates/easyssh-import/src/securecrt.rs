//! SecureCRT XML export (`File > Export` or `SecureCRT.exe /Export`).
//!
//! ```xml
//! <VanDyke version="3.0">
//!   <key name="Sessions">
//!     <key name="Folder">
//!       <key name="web1">
//!         <string name="Hostname">example.com</string>
//!         <dword name="[SSH2] Port">00000016</dword>   <!-- hex -->
//! ```
//!
//! A `key` that has a `Hostname` string is a session; other keys under
//! `Sessions` are folders. Dword values are hexadecimal.

use crate::model::{clean_path, ImportReport, ImportWarning, ImportedHost};
use crate::util::decode_text;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::path::PathBuf;

const MAX_DEPTH: usize = 64;

pub fn detect(bytes: &[u8]) -> bool {
    let text = decode_text(&bytes[..bytes.len().min(64 * 1024)]);
    text.contains("<VanDyke") || (text.contains("<key name=\"Sessions\"") && text.contains("<?xml"))
}

#[derive(Default)]
struct Frame {
    name: String,
    strings: HashMap<String, String>,
    dwords: HashMap<String, u32>,
}

pub fn parse(bytes: &[u8]) -> ImportReport {
    let text = decode_text(bytes);
    let mut reader = Reader::from_reader(text.as_bytes());
    let mut report = ImportReport::default();
    let mut stack: Vec<Frame> = Vec::new();
    // Currently open `<string>` / `<dword>` leaf: (is_dword, name, text).
    let mut leaf: Option<(bool, String, String)> = None;
    let mut buf = Vec::new();
    let mut sessions_seen = false;

    loop {
        buf.clear();
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            Err(error) => {
                report.warn(ImportWarning::new(format!(
                    "XML error at byte {}: {error}; the rest of the file was not read",
                    reader.buffer_position()
                )));
                break;
            }
        };
        match event {
            Event::Start(e) => {
                let tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
                let name = e
                    .attributes()
                    .flatten()
                    .find(|a| a.key.as_ref() == b"name")
                    .map(|a| {
                        a.unescape_value()
                            .map(|v| v.into_owned())
                            .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned())
                    })
                    .unwrap_or_default();
                match tag.as_str() {
                    "key" => {
                        if stack.len() >= MAX_DEPTH {
                            report.warn(ImportWarning::new("XML nested too deeply; stopped"));
                            break;
                        }
                        stack.push(Frame {
                            name,
                            ..Frame::default()
                        });
                    }
                    "string" | "dword" => {
                        leaf = Some((tag == "dword", name, String::new()));
                    }
                    _ => {}
                }
            }
            Event::Text(t) => {
                if let Some((_, _, buffer)) = leaf.as_mut() {
                    match t.unescape() {
                        Ok(s) => buffer.push_str(&s),
                        Err(_) => buffer.push_str(&String::from_utf8_lossy(&t)),
                    }
                }
            }
            Event::CData(c) => {
                if let Some((_, _, buffer)) = leaf.as_mut() {
                    buffer.push_str(&String::from_utf8_lossy(&c));
                }
            }
            Event::End(e) => match e.name().as_ref() {
                b"string" | b"dword" => {
                    if let (Some((is_dword, name, value)), Some(frame)) =
                        (leaf.take(), stack.last_mut())
                    {
                        if is_dword {
                            if let Ok(n) = u32::from_str_radix(value.trim(), 16) {
                                frame.dwords.insert(name, n);
                            }
                        } else {
                            frame.strings.insert(name, value);
                        }
                    }
                }
                b"key" => {
                    if let Some(frame) = stack.pop() {
                        if frame.name == "Sessions" {
                            sessions_seen = true;
                        }
                        if frame.strings.contains_key("Hostname") {
                            convert(&frame, &stack, &mut report);
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    if report.hosts.is_empty() && report.skipped == 0 && !sessions_seen {
        report.warn(ImportWarning::new(
            "no <key name=\"Sessions\"> found; is this a SecureCRT XML export?",
        ));
    }
    report.finish()
}

fn convert(frame: &Frame, parents: &[Frame], report: &mut ImportReport) {
    let name = frame.name.clone();
    // Folder path: the keys below `Sessions` (or all, if `Sessions` is absent).
    let start = parents
        .iter()
        .position(|p| p.name == "Sessions")
        .map_or(0, |i| i + 1);
    let group_path = clean_path(parents[start..].iter().map(|p| p.name.as_str()));
    if name == "Default" && group_path.is_empty() {
        return;
    }
    let text = |key: &str| {
        frame
            .strings
            .get(key)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
    };
    let protocol = text("Protocol Name").unwrap_or("");
    if !protocol.to_ascii_uppercase().starts_with("SSH") {
        report.skipped += 1;
        report.warn(ImportWarning::for_record(
            &name,
            format!("protocol {protocol:?} is not SSH; skipped"),
        ));
        return;
    }
    let hostname = text("Hostname").unwrap_or_default();
    let mut host = ImportedHost::new(&name, hostname);
    host.group_path = group_path;
    host.username = text("Username").map(str::to_owned);
    let port = frame
        .dwords
        .get("[SSH2] Port")
        .or_else(|| frame.dwords.get("[SSH1] Port"))
        .or_else(|| frame.dwords.get("Port"));
    if let Some(port) = port {
        match u16::try_from(*port) {
            Ok(p) if p != 0 => host.port = p,
            _ => report.warn(ImportWarning::for_record(
                &name,
                format!("invalid port {port}; using 22"),
            )),
        }
    }
    if let Some(path) = text("Identity Filename V2").or_else(|| text("Identity Filename")) {
        if !path.eq_ignore_ascii_case("<default>") {
            host.identity_file = Some(PathBuf::from(path));
        }
    }
    if let Some(firewall) = text("Firewall Name") {
        if let Some(jump) = firewall.strip_prefix("Session:") {
            host.jump.push(jump.trim().to_owned());
        } else if !firewall.eq_ignore_ascii_case("none") {
            report.warn(ImportWarning::for_record(
                &name,
                format!("firewall {firewall:?} is not imported"),
            ));
        }
    }
    for key in ["Session_Password_V2", "Password V2", "Password"] {
        if text(key).is_some() {
            report.secret_skipped(&name, "saved password");
            break;
        }
    }
    report.push_host(host);
}
