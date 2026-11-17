//! The neutral output model shared by every importer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

/// One host found in another client's export.
///
/// Secrets (passwords, passphrases, private key material) are never part of
/// this type; importers drop them and add an [`ImportWarning`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImportedHost {
    /// Display name (session name, `Host` alias, CSV label).
    pub label: String,
    pub hostname: String,
    pub port: u16,
    pub username: Option<String>,
    /// Folder path from the root, outermost first. Empty for top level.
    pub group_path: Vec<String>,
    /// Path of a private key file the source referenced. The file is not read.
    pub identity_file: Option<PathBuf>,
    /// Jump hosts in connection order, as labels or ssh aliases (possibly
    /// `user@host:port` for an inline OpenSSH `ProxyJump`).
    pub jump: Vec<String>,
    /// Reasons the host needs the OpenSSH engine (`ProxyCommand`, `Match`,
    /// `LocalCommand`, algorithm overrides, ...). Empty when russh can run it.
    pub openssh_only: Vec<String>,
    pub notes: String,
}

impl ImportedHost {
    pub fn new(label: impl Into<String>, hostname: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            hostname: hostname.into(),
            port: 22,
            ..Self::default()
        }
    }

    /// Validates the fields that end up on an ssh command line or in the UI.
    /// Returns the reason when the host must be rejected.
    pub(crate) fn check(&self) -> Result<(), String> {
        let bad = |s: &str| s.chars().any(|c| c.is_control() || c.is_whitespace());
        if self.hostname.is_empty() {
            return Err("missing hostname".into());
        }
        if bad(&self.hostname) {
            return Err(format!(
                "hostname {:?} contains whitespace or control characters",
                self.hostname
            ));
        }
        if self.hostname.starts_with('-') {
            return Err(format!(
                "hostname {:?} starts with '-' and could be read as an option",
                self.hostname
            ));
        }
        if let Some(user) = &self.username {
            if user.starts_with('-') || user.chars().any(char::is_control) {
                return Err(format!("username {user:?} is not acceptable"));
            }
        }
        if self.port == 0 {
            return Err("port 0 is invalid".into());
        }
        Ok(())
    }
}

/// A problem that did not stop the import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportWarning {
    /// 1-based line in the input, where the format is line based.
    pub line: Option<usize>,
    /// Name of the record (session, file in an archive, CSV row) concerned.
    pub record: Option<String>,
    pub message: String,
}

impl ImportWarning {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            line: None,
            record: None,
            message: message.into(),
        }
    }

    pub fn at_line(line: usize, message: impl Into<String>) -> Self {
        Self {
            line: Some(line),
            record: None,
            message: message.into(),
        }
    }

    pub fn for_record(record: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            line: None,
            record: Some(record.into()),
            message: message.into(),
        }
    }
}

impl fmt::Display for ImportWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(line) = self.line {
            write!(f, "line {line}: ")?;
        }
        if let Some(record) = &self.record {
            write!(f, "{record}: ")?;
        }
        f.write_str(&self.message)
    }
}

/// Result of one import run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImportReport {
    pub hosts: Vec<ImportedHost>,
    /// Every group path used by `hosts`, including ancestors, parents before
    /// children, sorted.
    pub groups: Vec<Vec<String>>,
    pub warnings: Vec<ImportWarning>,
    /// Records that were recognised but not imported (non-SSH sessions,
    /// invalid hosts, wildcard-only patterns are not counted).
    pub skipped: usize,
}

impl ImportReport {
    pub fn warn(&mut self, warning: ImportWarning) {
        self.warnings.push(warning);
    }

    /// Records a secret that was deliberately not imported.
    pub fn secret_skipped(&mut self, record: &str, what: &str) {
        self.warnings.push(ImportWarning::for_record(
            record,
            format!("{what} was not imported (secrets are never imported)"),
        ));
    }

    /// Validates and adds a host; invalid hosts become a warning and a skip.
    pub fn push_host(&mut self, host: ImportedHost) {
        match host.check() {
            Ok(()) => self.hosts.push(host),
            Err(reason) => {
                self.skipped += 1;
                self.warnings
                    .push(ImportWarning::for_record(host.label, reason));
            }
        }
    }

    /// Computes `groups` from the hosts and returns the report.
    pub(crate) fn finish(mut self) -> Self {
        let mut set: BTreeSet<Vec<String>> = BTreeSet::new();
        for host in &self.hosts {
            for end in 1..=host.group_path.len() {
                set.insert(host.group_path[..end].to_vec());
            }
        }
        // Folders without hosts are registered through `extra_groups` by the
        // importers that know them; they are merged by `add_group`.
        set.extend(self.groups.drain(..));
        self.groups = set.into_iter().collect();
        self
    }

    /// Registers a folder that may hold no host (and its ancestors).
    pub(crate) fn add_group(&mut self, path: &[String]) {
        for end in 1..=path.len() {
            self.groups.push(path[..end].to_vec());
        }
    }
}

/// Trims a folder name and drops empty segments.
pub(crate) fn clean_path<I, S>(parts: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    parts
        .into_iter()
        .map(|part| part.as_ref().trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect()
}
