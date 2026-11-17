//! FinalShell connections (`conn/*.json`).
//!
//! Best effort: the format is undocumented. Accepted input is one JSON
//! object, an array of objects, or a zip of `.json` files (for example a
//! zipped `conn` directory).
//!
//! An object with a non-empty `host` is a connection: `name`, `host`, `port`
//! (number or string), `user_name`, `description`, `jump_server_id`.
//! An object without `host` is a folder (`id`, `name`, `parent_id`); folder
//! chains become group paths. `password` and key material are never read.

use crate::archive::{is_zip, read_entries};
use crate::model::{ImportReport, ImportWarning, ImportedHost};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

pub fn detect(bytes: &[u8]) -> bool {
    if is_zip(bytes) {
        let Ok(archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
            return false;
        };
        let json = archive
            .file_names()
            .take(10_000)
            .any(|n| n.to_ascii_lowercase().ends_with(".json"));
        return json;
    }
    let Ok(value) = serde_json::from_slice::<Value>(trim_bom(bytes)) else {
        return false;
    };
    let objects = objects_of(value);
    objects.iter().any(|o| {
        o.contains_key("host")
            && (o.contains_key("user_name")
                || o.contains_key("authentication_type")
                || o.contains_key("conection_type"))
    })
}

fn trim_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

fn objects_of(value: Value) -> Vec<Map<String, Value>> {
    match value {
        Value::Object(map) => vec![map],
        Value::Array(items) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub fn parse(bytes: &[u8]) -> ImportReport {
    let mut report = ImportReport::default();
    let mut objects: Vec<Map<String, Value>> = Vec::new();
    if is_zip(bytes) {
        let entries = read_entries(bytes, &[".json"], &mut report);
        if entries.is_empty() {
            report.warn(ImportWarning::new("no .json files found in the archive"));
        }
        for entry in entries {
            match serde_json::from_slice::<Value>(trim_bom(&entry.bytes)) {
                Ok(value) => objects.extend(objects_of(value)),
                Err(error) => {
                    report.skipped += 1;
                    report.warn(ImportWarning::for_record(
                        entry.display(),
                        format!("invalid JSON: {error}"),
                    ));
                }
            }
        }
    } else {
        match serde_json::from_slice::<Value>(trim_bom(bytes)) {
            Ok(value) => objects = objects_of(value),
            Err(error) => report.warn(ImportWarning::new(format!("invalid JSON: {error}"))),
        }
    }
    convert(&objects, &mut report);
    report.finish()
}

fn string(obj: &Map<String, Value>, key: &str) -> Option<String> {
    match obj.get(key)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_owned()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn convert(objects: &[Map<String, Value>], report: &mut ImportReport) {
    let mut folders: HashMap<String, (String, Option<String>)> = HashMap::new();
    let mut labels: HashMap<String, String> = HashMap::new();
    for obj in objects {
        let (Some(id), name) = (string(obj, "id"), string(obj, "name")) else {
            continue;
        };
        if string(obj, "host").is_some() {
            labels.insert(id, name.unwrap_or_default());
        } else if let Some(name) = name {
            folders.insert(id, (name, string(obj, "parent_id")));
        }
    }

    for obj in objects {
        let Some(hostname) = string(obj, "host") else {
            continue;
        };
        let label = string(obj, "name").unwrap_or_else(|| hostname.clone());
        let mut host = ImportedHost::new(&label, hostname);
        match string(obj, "port") {
            None => {}
            Some(raw) => match crate::util::parse_port(&raw) {
                Some(port) => host.port = port,
                None => report.warn(ImportWarning::for_record(
                    &label,
                    format!("invalid port {raw:?}; using 22"),
                )),
            },
        }
        host.username = string(obj, "user_name");
        host.group_path = folder_path(&folders, string(obj, "parent_id"));
        let mut notes = Vec::new();
        if let Some(text) = string(obj, "description") {
            notes.push(text);
        }
        if let Some(key) = string(obj, "private_key_id") {
            notes.push(format!(
                "FinalShell key id: {key} (export the key from FinalShell separately)"
            ));
        }
        host.notes = notes.join("\n");
        if let Some(jump) = string(obj, "jump_server_id") {
            match labels.get(&jump) {
                Some(label) if !label.is_empty() => host.jump.push(label.clone()),
                _ => report.warn(ImportWarning::for_record(
                    &label,
                    format!("jump server {jump:?} not found in the input"),
                )),
            }
        }
        for secret in ["password", "passphrase", "secret_key"] {
            if string(obj, secret).is_some() {
                report.secret_skipped(&label, secret);
            }
        }
        report.push_host(host);
    }
    if objects.is_empty() && report.warnings.is_empty() {
        report.warn(ImportWarning::new("no JSON objects found"));
    }
}

fn folder_path(
    folders: &HashMap<String, (String, Option<String>)>,
    mut parent: Option<String>,
) -> Vec<String> {
    let mut path = Vec::new();
    let mut seen = HashSet::new();
    while let Some(id) = parent {
        if !seen.insert(id.clone()) || path.len() >= 32 {
            break;
        }
        match folders.get(&id) {
            Some((name, up)) => {
                path.push(name.clone());
                parent = up.clone();
            }
            None => break,
        }
    }
    path.reverse();
    path
}
