//! MobaXterm bookmarks (`.mxtsessions`, an INI file).
//!
//! ```ini
//! [Bookmarks_1]
//! SubRep=Servers\Prod
//! ImgNum=42
//! web1=#109#0%example.com%22%alice%%-1%-1%...#MobaFont%10%...
//! ```
//!
//! * `SubRep` is the folder path, separated by `\`.
//! * Every other key is a session name; its value is `#icon#fields#...`.
//! * `fields` is `%`-separated. Positions relied on:
//!   0 = session type (`0` is SSH; everything else is skipped),
//!   1 = host, 2 = port, 3 = username,
//!   14 = private key path (best effort; empty when unused).
//!
//! The format is undocumented, so unexpected shapes yield warnings.

use crate::model::{ImportReport, ImportWarning, ImportedHost};
use crate::util::{decode_text, parse_ini, parse_port, split_path};
use std::path::PathBuf;

pub fn detect(bytes: &[u8]) -> bool {
    let text = decode_text(&bytes[..bytes.len().min(64 * 1024)]);
    text.lines()
        .any(|l| l.trim().to_ascii_lowercase().starts_with("[bookmarks"))
}

pub fn parse(bytes: &[u8]) -> ImportReport {
    let text = decode_text(bytes);
    let mut report = ImportReport::default();
    let mut found = false;
    for section in parse_ini(&text) {
        let lower = section.name.to_ascii_lowercase();
        if lower.starts_with("password") || lower.starts_with("credential") {
            if !section.entries.is_empty() {
                report.secret_skipped(&section.name, "stored credentials");
            }
            continue;
        }
        if !lower.starts_with("bookmarks") {
            continue;
        }
        found = true;
        let group_path = section.get("SubRep").map(split_path).unwrap_or_default();
        report.add_group(&group_path);
        for (name, value, line) in &section.entries {
            if name.eq_ignore_ascii_case("SubRep") || name.eq_ignore_ascii_case("ImgNum") {
                continue;
            }
            if !value.starts_with('#') {
                report.warn(ImportWarning {
                    line: Some(*line),
                    record: Some(name.clone()),
                    message: "not a session entry; skipped".into(),
                });
                report.skipped += 1;
                continue;
            }
            session(name, value, *line, &group_path, &mut report);
        }
    }
    if !found {
        report.warn(ImportWarning::new("no [Bookmarks] sections found"));
    }
    report.finish()
}

fn session(name: &str, value: &str, line: usize, group: &[String], report: &mut ImportReport) {
    // "#icon#fields#rest": split on '#', skipping the empty first piece.
    let mut parts = value.split('#').skip(1);
    let _icon = parts.next();
    let Some(fields) = parts.next() else {
        report.skipped += 1;
        report.warn(ImportWarning {
            line: Some(line),
            record: Some(name.to_owned()),
            message: "malformed session value; skipped".into(),
        });
        return;
    };
    let fields: Vec<&str> = fields.split('%').collect();
    if fields[0] != "0" {
        report.skipped += 1;
        report.warn(ImportWarning {
            line: Some(line),
            record: Some(name.to_owned()),
            message: format!("session type {} is not SSH; skipped", fields[0]),
        });
        return;
    }
    let get = |i: usize| fields.get(i).copied().map(str::trim).unwrap_or("");
    let mut host = ImportedHost::new(name, get(1));
    host.group_path = group.to_vec();
    match parse_port(get(2)) {
        Some(port) => host.port = port,
        None => {
            if !get(2).is_empty() && get(2) != "0" {
                report.warn(ImportWarning {
                    line: Some(line),
                    record: Some(name.to_owned()),
                    message: format!("invalid port {:?}; using 22", get(2)),
                });
            }
        }
    }
    if !get(3).is_empty() {
        host.username = Some(get(3).to_owned());
    }
    if !get(14).is_empty() {
        host.identity_file = Some(PathBuf::from(get(14)));
    }
    report.push_host(host);
}
