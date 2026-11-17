//! Termius exports. **Best effort**: Termius publishes no format
//! specification, so this follows what its CSV import template and its
//! account API objects look like and may need adjusting against real exports.
//!
//! CSV, header required, case-insensitive: `Groups`, `Label`, `Tags`,
//! `Hostname/IP`, `Protocol`, `Port`, `Username`, `Password`, `SSH Key`
//! (columns may be missing). `Groups` is a `/` separated path. Rows whose
//! `Protocol` is present and not `ssh` are skipped. `Tags` go to the notes.
//! `Password` and `SSH Key` are never imported.
//!
//! JSON: an object with `hosts` and optionally `groups` arrays (or a bare
//! array of hosts). Host: `label`, `address`/`hostname`, `port`, `username`,
//! `group`/`group_id` (an id), optionally nested under `ssh_config` (and
//! `ssh_config.identity` for the username), `ssh_config.host_chain.host_ids`
//! for jump hosts. Group: `id`, `label`/`name`, `parent_group`/`parent`.

use crate::csv_format::{cell, read_table};
use crate::model::{clean_path, ImportReport, ImportWarning, ImportedHost};
use crate::util::{decode_text, parse_port};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

pub fn detect(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(64 * 1024)];
    let text = decode_text(head);
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        let Ok(value) = serde_json::from_slice::<Value>(
            bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes),
        ) else {
            return false;
        };
        return match &value {
            Value::Object(map) => map.get("hosts").is_some_and(Value::is_array),
            Value::Array(items) => items.iter().any(|v| {
                v.as_object()
                    .is_some_and(|o| o.contains_key("address") && o.contains_key("label"))
            }),
            _ => false,
        };
    }
    let first = text.lines().next().unwrap_or("").to_ascii_lowercase();
    first.contains("hostname/ip") || first.contains("hostname / ip")
}

pub fn parse(bytes: &[u8]) -> ImportReport {
    let text = decode_text(bytes);
    if text.trim_start().starts_with(['{', '[']) {
        parse_json(&text)
    } else {
        parse_csv(&text)
    }
}

fn parse_csv(text: &str) -> ImportReport {
    let mut report = ImportReport::default();
    let table = read_table(text, &mut report);
    let Some(host_col) = table.col(&["hostname/ip", "hostname / ip", "hostname", "address"]) else {
        report.warn(ImportWarning::new("no `Hostname/IP` column found"));
        return report.finish();
    };
    let label = table.col(&["label", "name"]);
    let groups = table.col(&["groups", "group"]);
    let tags = table.col(&["tags"]);
    let protocol = table.col(&["protocol"]);
    let port = table.col(&["port"]);
    let user = table.col(&["username", "user"]);
    for secret in ["password", "ssh key", "key", "passphrase"] {
        if table.col(&[secret]).is_some() {
            report.warn(ImportWarning::new(format!(
                "column `{secret}` is not imported (secrets are never imported)"
            )));
        }
    }
    for (line, row) in &table.rows {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let protocol = cell(row, protocol);
        let hostname = cell(row, Some(host_col));
        let name = match cell(row, label) {
            "" => hostname,
            name => name,
        };
        if !protocol.is_empty() && !protocol.eq_ignore_ascii_case("ssh") {
            report.skipped += 1;
            report.warn(ImportWarning {
                line: Some(*line),
                record: Some(name.to_owned()),
                message: format!("protocol {protocol:?} is not SSH; skipped"),
            });
            continue;
        }
        let mut host = ImportedHost::new(name, hostname);
        let raw_port = cell(row, port);
        if !raw_port.is_empty() {
            match parse_port(raw_port) {
                Some(p) => host.port = p,
                None => report.warn(ImportWarning {
                    line: Some(*line),
                    record: Some(name.to_owned()),
                    message: format!("invalid port {raw_port:?}; using 22"),
                }),
            }
        }
        host.username = Some(cell(row, user).to_owned()).filter(|s| !s.is_empty());
        host.group_path = clean_path(cell(row, groups).split('/'));
        let tag_text = cell(row, tags);
        if !tag_text.is_empty() {
            host.notes = format!("Tags: {tag_text}");
        }
        report.push_host(host);
    }
    report.finish()
}

fn id_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(o) => id_of(o.get("id")),
        _ => None,
    }
}

fn str_of(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn parse_json(text: &str) -> ImportReport {
    let mut report = ImportReport::default();
    let value: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(error) => {
            report.warn(ImportWarning::new(format!("invalid JSON: {error}")));
            return report.finish();
        }
    };
    let empty = Vec::new();
    let (hosts, groups) = match &value {
        Value::Object(map) => (
            map.get("hosts").and_then(Value::as_array).unwrap_or(&empty),
            map.get("groups")
                .and_then(Value::as_array)
                .unwrap_or(&empty),
        ),
        Value::Array(items) => (items, &empty),
        _ => {
            report.warn(ImportWarning::new("expected an object or an array"));
            return report.finish();
        }
    };
    let mut group_map: HashMap<String, (String, Option<String>)> = HashMap::new();
    for group in groups.iter().filter_map(Value::as_object) {
        if let Some(id) = id_of(group.get("id")) {
            let name = str_of(group.get("label").or_else(|| group.get("name")));
            let parent = id_of(group.get("parent_group").or_else(|| group.get("parent")));
            group_map.insert(id, (name.unwrap_or_default(), parent));
        }
    }
    let mut labels: HashMap<String, String> = HashMap::new();
    for host in hosts.iter().filter_map(Value::as_object) {
        if let Some(id) = id_of(host.get("id")) {
            labels.insert(id, label_of(host));
        }
    }
    for host in hosts.iter().filter_map(Value::as_object) {
        convert_host(host, &group_map, &labels, &mut report);
    }
    if hosts.is_empty() {
        report.warn(ImportWarning::new("no hosts found"));
    }
    report.finish()
}

fn label_of(host: &Map<String, Value>) -> String {
    str_of(host.get("label"))
        .or_else(|| str_of(host.get("name")))
        .or_else(|| str_of(host.get("address")))
        .or_else(|| str_of(host.get("hostname")))
        .unwrap_or_default()
}

fn convert_host(
    host: &Map<String, Value>,
    groups: &HashMap<String, (String, Option<String>)>,
    labels: &HashMap<String, String>,
    report: &mut ImportReport,
) {
    let label = label_of(host);
    let hostname = str_of(host.get("address"))
        .or_else(|| str_of(host.get("hostname")))
        .or_else(|| str_of(host.get("host")))
        .unwrap_or_default();
    let ssh = host.get("ssh_config").and_then(Value::as_object);
    let pick = |key: &str| str_of(host.get(key)).or_else(|| ssh.and_then(|s| str_of(s.get(key))));
    let identity = ssh
        .and_then(|s| s.get("identity"))
        .and_then(Value::as_object);

    let mut out = ImportedHost::new(&label, hostname);
    if let Some(raw) = pick("port") {
        match parse_port(&raw) {
            Some(p) => out.port = p,
            None => report.warn(ImportWarning::for_record(
                &label,
                format!("invalid port {raw:?}; using 22"),
            )),
        }
    }
    out.username = pick("username").or_else(|| identity.and_then(|i| str_of(i.get("username"))));

    let mut path = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor = id_of(host.get("group").or_else(|| host.get("group_id")));
    while let Some(id) = cursor {
        if !seen.insert(id.clone()) || path.len() >= 32 {
            break;
        }
        let Some((name, parent)) = groups.get(&id) else {
            break;
        };
        path.push(name.clone());
        cursor = parent.clone();
    }
    path.reverse();
    out.group_path = clean_path(path);

    let chain = ssh
        .and_then(|s| s.get("host_chain"))
        .and_then(|c| c.get("host_ids"))
        .and_then(Value::as_array);
    for hop in chain.into_iter().flatten() {
        match id_of(Some(hop)).and_then(|id| labels.get(&id)) {
            Some(name) if !name.is_empty() => out.jump.push(name.clone()),
            _ => report.warn(ImportWarning::for_record(
                &label,
                "jump host not found in the export",
            )),
        }
    }
    let has =
        |key: &str| pick(key).is_some() || identity.is_some_and(|i| str_of(i.get(key)).is_some());
    for secret in ["password", "passphrase", "private_key", "key"] {
        if has(secret) {
            report.secret_skipped(&label, secret);
        }
    }
    report.push_host(out);
}
