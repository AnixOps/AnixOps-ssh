//! Bounded zip reading shared by the Xshell and FinalShell importers.

use crate::model::{ImportReport, ImportWarning};
use std::io::{Cursor, Read};

const MAX_ENTRIES: usize = 10_000;
const MAX_ENTRY_BYTES: u64 = 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) fn is_zip(bytes: &[u8]) -> bool {
    bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06")
}

/// One file of an archive: path segments (no empty or `..` parts) and bytes.
pub(crate) struct Entry {
    pub path: Vec<String>,
    pub bytes: Vec<u8>,
}

impl Entry {
    pub fn file_name(&self) -> &str {
        self.path.last().map_or("", String::as_str)
    }

    pub fn display(&self) -> String {
        self.path.join("/")
    }
}

/// Reads the entries whose file name ends with one of `extensions`
/// (lowercase, with dot). Entry and total sizes are capped, so a zip bomb
/// yields a warning instead of exhausting memory.
pub(crate) fn read_entries(
    bytes: &[u8],
    extensions: &[&str],
    report: &mut ImportReport,
) -> Vec<Entry> {
    let mut archive = match zip::ZipArchive::new(Cursor::new(bytes)) {
        Ok(archive) => archive,
        Err(error) => {
            report.warn(ImportWarning::new(format!("not a readable zip: {error}")));
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    let mut total = 0u64;
    let count = archive.len();
    if count > MAX_ENTRIES {
        report.warn(ImportWarning::new(format!(
            "archive has {count} entries; only the first {MAX_ENTRIES} are read"
        )));
    }
    for index in 0..count.min(MAX_ENTRIES) {
        let mut file = match archive.by_index(index) {
            Ok(file) => file,
            Err(error) => {
                report.warn(ImportWarning::new(format!(
                    "entry {index} unreadable ({error}); skipped"
                )));
                continue;
            }
        };
        if file.is_dir() {
            continue;
        }
        let name = file.name().replace('\\', "/");
        let lower = name.to_ascii_lowercase();
        if !extensions.iter().any(|ext| lower.ends_with(ext)) {
            continue;
        }
        let path: Vec<String> = name
            .split('/')
            .filter(|s| !s.is_empty() && *s != "." && *s != "..")
            .map(str::to_owned)
            .collect();
        if path.is_empty() {
            continue;
        }
        if file.size() > MAX_ENTRY_BYTES || total + file.size() > MAX_TOTAL_BYTES {
            report.skipped += 1;
            report.warn(ImportWarning::for_record(name, "entry too large; skipped"));
            continue;
        }
        let mut data = Vec::new();
        if let Err(error) = file.by_ref().take(MAX_ENTRY_BYTES).read_to_end(&mut data) {
            report.skipped += 1;
            report.warn(ImportWarning::for_record(
                name,
                format!("unreadable: {error}"),
            ));
            continue;
        }
        total += data.len() as u64;
        out.push(Entry { path, bytes: data });
    }
    out
}
