//! Importers for other SSH clients' exports.
//!
//! Every format module exposes `detect(bytes) -> bool` and
//! `parse(bytes) -> ImportReport`; nothing here panics on malformed input and
//! nothing reads the user's own files (callers pass bytes). Passwords and
//! private key material are never imported; a warning is recorded instead.
//!
//! [`import_auto`] picks a format with the `detect` functions.

mod adapter;
pub mod ansible;
mod archive;
pub mod csv_format;
pub mod finalshell;
pub mod mobaxterm;
mod model;
pub mod openssh;
pub mod putty;
pub mod securecrt;
pub mod termius;
mod util;
pub mod xshell;

pub use adapter::{EntityBatch, OPENSSH_ONLY_TAG};
pub use model::{ImportReport, ImportWarning, ImportedHost};

use std::path::Path;

/// The supported source formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Putty,
    SecureCrt,
    MobaXterm,
    Xshell,
    FinalShell,
    Termius,
    Ansible,
    Csv,
    OpenSsh,
}

impl Format {
    /// Every format, in detection order.
    pub const ALL: [Format; 9] = [
        Format::Putty,
        Format::SecureCrt,
        Format::MobaXterm,
        Format::Xshell,
        Format::FinalShell,
        Format::Termius,
        Format::Ansible,
        Format::Csv,
        Format::OpenSsh,
    ];

    /// The stable identifier used in the daemon protocol and the UI.
    pub fn id(self) -> &'static str {
        match self {
            Format::Putty => "putty",
            Format::SecureCrt => "securecrt",
            Format::MobaXterm => "mobaxterm",
            Format::Xshell => "xshell",
            Format::FinalShell => "finalshell",
            Format::Termius => "termius",
            Format::Ansible => "ansible",
            Format::Csv => "csv",
            Format::OpenSsh => "openssh",
        }
    }

    /// The format with this [`Format::id`].
    pub fn from_id(id: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|format| format.id() == id)
    }
}

/// Formats in detection order: specific markers first, the permissive
/// OpenSSH syntax last.
type Detector = fn(&[u8]) -> bool;

const ORDER: &[(Format, Detector)] = &[
    (Format::Putty, putty::detect),
    (Format::SecureCrt, securecrt::detect),
    (Format::MobaXterm, mobaxterm::detect),
    (Format::Xshell, xshell::detect),
    (Format::FinalShell, finalshell::detect),
    (Format::Termius, termius::detect),
    (Format::Ansible, ansible::detect),
    (Format::Csv, csv_format::detect),
    (Format::OpenSsh, openssh::detect),
];

/// Guesses the format of `bytes`.
pub fn detect_format(bytes: &[u8]) -> Option<Format> {
    ORDER
        .iter()
        .find(|(_, detect)| detect(bytes))
        .map(|(format, _)| *format)
}

/// Parses with a known format. `base_dir` is used by OpenSSH `Include`.
pub fn import_as(format: Format, bytes: &[u8], base_dir: Option<&Path>) -> ImportReport {
    match format {
        Format::Putty => putty::parse(bytes),
        Format::SecureCrt => securecrt::parse(bytes),
        Format::MobaXterm => mobaxterm::parse(bytes),
        Format::Xshell => xshell::parse(bytes),
        Format::FinalShell => finalshell::parse(bytes),
        Format::Termius => termius::parse(bytes),
        Format::Ansible => ansible::parse(bytes),
        Format::Csv => csv_format::parse(bytes),
        Format::OpenSsh => match base_dir {
            Some(dir) => openssh::parse_with_base(bytes, dir),
            None => openssh::parse(bytes),
        },
    }
}

/// Detects the format and parses. An unrecognised input yields an empty
/// report with a warning.
pub fn import_auto(bytes: &[u8]) -> ImportReport {
    import_auto_in(bytes, None)
}

/// Like [`import_auto`], with a base directory for OpenSSH `Include`.
pub fn import_auto_in(bytes: &[u8], base_dir: Option<&Path>) -> ImportReport {
    match detect_format(bytes) {
        Some(format) => import_as(format, bytes, base_dir),
        None => ImportReport {
            warnings: vec![ImportWarning::new("unrecognised import format")],
            ..ImportReport::default()
        },
    }
}
