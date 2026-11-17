//! Xshell sessions: a single `.xsh` file (INI, usually UTF-16LE) or a zip of
//! `.xsh` files (what Xshell's "Export" writes as `.xts`). Folders inside the
//! zip become groups.
//!
//! Fields used: `[CONNECTION]` `Host`, `Port`, `Protocol`;
//! `[CONNECTION:AUTHENTICATION]` `UserName`, `UserKey`. `Password` and
//! `Passphrase` are never read. `UserKey` names a key in Xshell's own key
//! store, not a file, so it goes into the notes.

use crate::archive::{is_zip, read_entries};
use crate::model::{ImportReport, ImportWarning, ImportedHost};
use crate::util::{decode_text, parse_ini, parse_port};

pub fn detect(bytes: &[u8]) -> bool {
    if is_zip(bytes) {
        return zip_has_extension(bytes, ".xsh");
    }
    let text = decode_text(&bytes[..bytes.len().min(64 * 1024)]);
    text.lines()
        .any(|l| l.trim().eq_ignore_ascii_case("[CONNECTION]"))
}

fn zip_has_extension(bytes: &[u8], ext: &str) -> bool {
    let Ok(archive) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else {
        return false;
    };
    let found = archive
        .file_names()
        .take(10_000)
        .any(|name| name.to_ascii_lowercase().ends_with(ext));
    found
}

/// Parses a `.xsh` file or a zip of them. A lone `.xsh` has no session
/// name; use [`parse_named`] when the file name is known.
pub fn parse(bytes: &[u8]) -> ImportReport {
    if is_zip(bytes) {
        let mut report = ImportReport::default();
        let entries = read_entries(bytes, &[".xsh"], &mut report);
        if entries.is_empty() {
            report.warn(ImportWarning::new("no .xsh files found in the archive"));
        }
        for entry in entries {
            let stem = file_stem(entry.file_name());
            let group = entry.path[..entry.path.len() - 1].to_vec();
            report.add_group(&group);
            session(&stem, &group, &entry.bytes, &mut report);
        }
        report.finish()
    } else {
        parse_named("", bytes)
    }
}

/// Parses one `.xsh` file; `name` is the file name, used as the label.
pub fn parse_named(name: &str, bytes: &[u8]) -> ImportReport {
    let mut report = ImportReport::default();
    session(&file_stem(name), &[], bytes, &mut report);
    report.finish()
}

fn file_stem(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((stem, ext)) if ext.eq_ignore_ascii_case("xsh") && !stem.is_empty() => stem.to_owned(),
        _ => base.to_owned(),
    }
}

fn session(name: &str, group: &[String], bytes: &[u8], report: &mut ImportReport) {
    let text = decode_text(bytes);
    let sections = parse_ini(&text);
    let find = |wanted: &str| {
        sections
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(wanted))
    };
    let Some(conn) = find("CONNECTION") else {
        report.skipped += 1;
        report.warn(ImportWarning::for_record(
            name,
            "no [CONNECTION] section; skipped",
        ));
        return;
    };
    if let Some(protocol) = conn.get("Protocol") {
        if !protocol.eq_ignore_ascii_case("ssh") && !protocol.is_empty() {
            report.skipped += 1;
            report.warn(ImportWarning::for_record(
                name,
                format!("protocol {protocol:?} is not SSH; skipped"),
            ));
            return;
        }
    }
    let hostname = conn.get("Host").unwrap_or("");
    let label = if name.is_empty() { hostname } else { name };
    let mut host = ImportedHost::new(label, hostname);
    host.group_path = group.to_vec();
    if let Some(raw) = conn.get("Port") {
        match parse_port(raw) {
            Some(port) => host.port = port,
            None => report.warn(ImportWarning::for_record(
                label,
                format!("invalid port {raw:?}; using 22"),
            )),
        }
    }
    if let Some(auth) = find("CONNECTION:AUTHENTICATION") {
        host.username = auth
            .get("UserName")
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        if let Some(key) = auth.get("UserKey").filter(|s| !s.is_empty()) {
            host.notes = format!("Xshell user key: {key} (export the key from Xshell separately)");
        }
        for secret in ["Password", "Passphrase"] {
            if auth.get(secret).is_some_and(|s| !s.is_empty()) {
                report.secret_skipped(label, secret);
            }
        }
    }
    report.push_host(host);
}
