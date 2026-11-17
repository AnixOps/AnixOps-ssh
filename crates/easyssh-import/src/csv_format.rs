//! EasySSH's own CSV format.
//!
//! The first row must be a header. Columns (case-insensitive, any order,
//! extra columns ignored):
//!
//! | column          | meaning                                                  |
//! |-----------------|----------------------------------------------------------|
//! | `label`         | display name; defaults to `hostname`                     |
//! | `hostname`      | required                                                 |
//! | `port`          | default 22                                               |
//! | `username`      | optional                                                 |
//! | `group`         | folder path separated by `/`                             |
//! | `identity_file` | path of a private key file (the file is not read)        |
//! | `jump`          | jump host labels or aliases separated by `;`, in order   |
//!
//! Columns named `password`, `passphrase` or `private_key` are not imported, with a
//! warning.

use crate::model::{clean_path, ImportReport, ImportWarning, ImportedHost};
use crate::util::{decode_text, parse_port};
use std::path::PathBuf;

/// A header row plus data rows, with 1-based line numbers.
pub(crate) struct Table {
    pub headers: Vec<String>,
    pub rows: Vec<(usize, Vec<String>)>,
}

impl Table {
    /// Index of the first header equal to one of `names` (lowercase).
    pub fn col(&self, names: &[&str]) -> Option<usize> {
        self.headers
            .iter()
            .position(|h| names.iter().any(|n| h == n))
    }
}

/// Reads CSV into a table; malformed records become warnings.
pub(crate) fn read_table(text: &str, report: &mut ImportReport) -> Table {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(true)
        .trim(csv::Trim::All)
        .from_reader(text.as_bytes());
    let headers = match reader.headers() {
        Ok(h) => h
            .iter()
            .map(|c| c.trim_matches('\u{FEFF}').trim().to_ascii_lowercase())
            .collect(),
        Err(error) => {
            report.warn(ImportWarning::new(format!("unreadable header: {error}")));
            Vec::new()
        }
    };
    let mut rows = Vec::new();
    let mut errors = 0;
    for record in reader.records() {
        match record {
            Ok(record) => {
                let line = record.position().map_or(0, |p| p.line() as usize);
                rows.push((line, record.iter().map(str::to_owned).collect()));
            }
            Err(error) => {
                report.skipped += 1;
                report.warn(ImportWarning::new(format!("unreadable row: {error}")));
                errors += 1;
                if errors >= 100 {
                    report.warn(ImportWarning::new("too many unreadable rows; stopped"));
                    break;
                }
            }
        }
    }
    Table { headers, rows }
}

/// Cell text of a row, empty when the column or cell is missing.
pub(crate) fn cell(row: &[String], col: Option<usize>) -> &str {
    col.and_then(|i| row.get(i)).map_or("", |s| s.trim())
}

pub fn detect(bytes: &[u8]) -> bool {
    let text = decode_text(&bytes[..bytes.len().min(8 * 1024)]);
    let Some(first) = text.lines().next() else {
        return false;
    };
    let cols: Vec<String> = first
        .split(',')
        .map(|c| {
            c.trim()
                .trim_matches('"')
                .trim_matches('\u{FEFF}')
                .to_ascii_lowercase()
        })
        .collect();
    cols.iter().any(|c| c == "hostname") && cols.iter().any(|c| c == "label")
}

pub fn parse(bytes: &[u8]) -> ImportReport {
    let text = decode_text(bytes);
    let mut report = ImportReport::default();
    let table = read_table(&text, &mut report);
    let Some(hostname_col) = table.col(&["hostname"]) else {
        report.warn(ImportWarning::new(
            "header row with a `hostname` column is required",
        ));
        return report.finish();
    };
    let label = table.col(&["label"]);
    let port = table.col(&["port"]);
    let user = table.col(&["username", "user"]);
    let group = table.col(&["group"]);
    let identity = table.col(&["identity_file"]);
    let jump = table.col(&["jump"]);
    for secret in ["password", "passphrase", "private_key"] {
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
        let hostname = cell(row, Some(hostname_col));
        let name = match cell(row, label) {
            "" => hostname,
            name => name,
        };
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
        host.group_path = clean_path(cell(row, group).split('/'));
        host.identity_file = Some(cell(row, identity))
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        host.jump = clean_path(cell(row, jump).split([';', ',']));
        report.push_host(host);
    }
    report.finish()
}
