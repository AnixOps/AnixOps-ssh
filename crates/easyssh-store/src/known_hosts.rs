//! Host keys: the `known_hosts` table and import of OpenSSH `known_hosts` text.

use crate::error::{Result, StoreError};
use crate::meta::{fmt_ts, parse_ts, Meta};
use crate::store::{b64, int, opt_text, text, Entity, Field, Fields, Record, Store};
use base64::Engine;
use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Row};
use sha2::{Digest, Sha256};
use std::path::Path;

/// One trusted host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHost {
    pub id: String,
    /// The first field of the known_hosts line, verbatim: `host`,
    /// `[host]:port`, a comma list, or a hashed `|1|salt|hash` entry.
    pub host: String,
    /// Derived from `host` (22 when it names none, and for hashed entries,
    /// whose port is inside the hash).
    pub port: u16,
    /// Derived: the entry is hashed (`|1|...`) and cannot be read back.
    pub hashed: bool,
    /// `@cert-authority` or `@revoked` when the line had a marker.
    pub marker: Option<String>,
    pub key_type: String,
    pub key_blob: Vec<u8>,
    /// Derived: `SHA256:` plus unpadded base64 of the SHA-256 of the blob.
    pub fingerprint: String,
    pub comment: Option<String>,
    pub added_at: DateTime<Utc>,
}

/// OpenSSH-style fingerprint of a public key blob.
pub fn fingerprint_of(key_blob: &[u8]) -> String {
    let digest = Sha256::digest(key_blob);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

fn first_pattern(host: &str) -> &str {
    host.split(',').next().unwrap_or(host)
}

fn bracketed(pattern: &str) -> Option<(&str, u16)> {
    let rest = pattern.strip_prefix('[')?;
    let (name, port) = rest.split_once("]:")?;
    Some((name, port.parse().ok()?))
}

fn derive_port(host: &str) -> u16 {
    bracketed(first_pattern(host)).map_or(22, |(_, port)| port)
}

impl KnownHost {
    /// The entry as `known_hosts.list` reports it (the protocol's
    /// `KnownHostEntry`): everything but the key itself.
    pub fn summary_json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "host": self.host,
            "port": self.port,
            "hashed": self.hashed,
            "marker": self.marker,
            "key_type": self.key_type,
            "fingerprint": self.fingerprint,
            "comment": self.comment,
            "added_at": self.added_at.to_rfc3339(),
        })
    }

    /// A key for `host` on `port`, written the way OpenSSH writes it.
    pub fn new(host: &str, port: u16, key_type: &str, key_blob: Vec<u8>) -> Self {
        let host = if port == 22 {
            host.to_owned()
        } else {
            format!("[{host}]:{port}")
        };
        let mut entry = Self {
            id: uuid::Uuid::new_v4().to_string(),
            host,
            port,
            hashed: false,
            marker: None,
            key_type: key_type.to_owned(),
            key_blob,
            fingerprint: String::new(),
            comment: None,
            added_at: Utc::now(),
        };
        entry.derive();
        entry
    }

    fn derive(&mut self) {
        self.port = derive_port(&self.host);
        self.hashed = self.host.starts_with("|1|");
        self.fingerprint = fingerprint_of(&self.key_blob);
    }

    /// Whether this (plain) entry names `host` on `port`. Hashed entries and
    /// wildcard patterns never match here.
    pub fn matches(&self, host: &str, port: u16) -> bool {
        if self.hashed {
            return false;
        }
        self.host
            .split(',')
            .any(|pattern| match bracketed(pattern) {
                Some((name, entry_port)) => name.eq_ignore_ascii_case(host) && entry_port == port,
                None => pattern.eq_ignore_ascii_case(host) && port == 22,
            })
    }
}

impl Entity for KnownHost {
    const TABLE: &'static str = "known_hosts";
    const NAME: &'static str = "known host";
    const ORDER: &'static str = "host, key_type, id";
    fn id(&self) -> &str {
        &self.id
    }
    fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.id)
            .map_err(|_| StoreError::Invalid("known host id must be a UUID".into()))?;
        if self.host.trim().is_empty() || self.key_type.is_empty() || self.key_blob.is_empty() {
            return Err(StoreError::Invalid(
                "a known host needs a host, key type and key".into(),
            ));
        }
        Ok(())
    }
    fn from_row(_: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self> {
        Ok(Self {
            id: meta.id.clone(),
            host: row.get("host")?,
            port: row.get("port")?,
            hashed: row.get::<_, i64>("hashed")? != 0,
            marker: row.get("marker")?,
            key_type: row.get("key_type")?,
            key_blob: row.get("key_blob")?,
            fingerprint: row.get("fingerprint")?,
            comment: row.get("comment")?,
            added_at: parse_ts(&row.get::<_, String>("added_at")?)?,
        })
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.insert("host", Field::plain(&self.host));
        fields.insert("marker", Field::plain(&self.marker));
        fields.insert("key_type", Field::plain(&self.key_type));
        fields.insert("key_blob", Field::plain(b64(&self.key_blob)));
        fields.insert("comment", Field::plain(&self.comment));
        fields
    }
    fn columns(&self, _: &Store, _: &str) -> Result<Vec<(&'static str, Value)>> {
        let mut derived = self.clone();
        derived.derive();
        Ok(vec![
            ("host", text(&derived.host)),
            ("port", int(derived.port)),
            ("hashed", int(derived.hashed)),
            ("marker", opt_text(derived.marker.as_deref())),
            ("key_type", text(&derived.key_type)),
            ("key_blob", Value::Blob(derived.key_blob.clone())),
            ("fingerprint", text(&derived.fingerprint)),
            ("comment", opt_text(derived.comment.as_deref())),
            ("added_at", text(fmt_ts(derived.added_at))),
        ])
    }
    fn keep_from(&mut self, old: &Self) {
        self.added_at = old.added_at;
    }
    fn sync_meta(&mut self, _: &Meta) {
        self.derive();
    }
}

crud!(
    KnownHost,
    list_known_hosts,
    list_deleted_known_hosts,
    get_known_host,
    upsert_known_host,
    soft_delete_known_host,
    restore_known_host
);

/// A `known_hosts` line that was not imported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedLine {
    /// 1-based line number.
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KnownHostsImport {
    pub imported: usize,
    /// Lines whose host, key type and key were already stored.
    pub duplicates: usize,
    pub skipped: Vec<SkippedLine>,
}

fn parse_line(line: &str) -> std::result::Result<Option<KnownHost>, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let mut parts = line.split_whitespace();
    let mut first = parts.next().ok_or("empty line")?;
    let marker = if first.starts_with('@') {
        let marker = first.to_owned();
        if marker != "@cert-authority" && marker != "@revoked" {
            return Err(format!("unknown marker {marker}"));
        }
        first = parts.next().ok_or("missing host after marker")?;
        Some(marker)
    } else {
        None
    };
    let key_type = parts.next().ok_or("missing key type")?;
    let blob = parts.next().ok_or("missing key")?;
    let key_blob = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .map_err(|_| "key is not valid base64".to_owned())?;
    if key_blob.is_empty() {
        return Err("key is empty".into());
    }
    let comment: Vec<&str> = parts.collect();
    let mut entry = KnownHost::new("placeholder", 22, key_type, key_blob);
    entry.host = first.to_owned();
    entry.marker = marker;
    entry.comment = (!comment.is_empty()).then(|| comment.join(" "));
    entry.derive();
    Ok(Some(entry))
}

impl Store {
    /// Live entries that name `host` on `port` (plain entries only).
    pub fn find_known_hosts(&self, host: &str, port: u16) -> Result<Vec<Record<KnownHost>>> {
        Ok(self
            .list_known_hosts()?
            .into_iter()
            .filter(|record| record.item.matches(host, port))
            .collect())
    }

    /// Imports OpenSSH `known_hosts` text. Entries are kept as written: plain
    /// and hashed hosts alike, with markers and comments. Lines already stored
    /// (same host, key type and key) are counted as duplicates, except removed
    /// ones, which are restored; lines that cannot be read are reported in `skipped`.
    pub fn import_known_hosts(&self, text: &str) -> Result<KnownHostsImport> {
        self.in_tx(|| {
            let mut report = KnownHostsImport::default();
            let now = Utc::now();
            for (index, line) in text.lines().enumerate() {
                let entry = match parse_line(line) {
                    Ok(Some(entry)) => entry,
                    Ok(None) => continue,
                    Err(reason) => {
                        report.skipped.push(SkippedLine {
                            line: index + 1,
                            reason,
                        });
                        continue;
                    }
                };
                let existing: Option<(String, i64)> = self
                    .conn
                    .query_row(
                        "SELECT id, deleted FROM known_hosts
                         WHERE vault_id = ?1 AND host = ?2 AND key_type = ?3 AND key_blob = ?4",
                        rusqlite::params![
                            self.vault_id,
                            entry.host,
                            entry.key_type,
                            entry.key_blob
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                match existing {
                    // Removed earlier: importing the line again brings it back.
                    Some((id, 1)) => {
                        self.restore_known_host(&id)?;
                        report.imported += 1;
                        continue;
                    }
                    Some(_) => {
                        report.duplicates += 1;
                        continue;
                    }
                    None => {}
                }
                let mut entry = entry;
                entry.added_at = now;
                self.upsert_at(&entry, now)?;
                report.imported += 1;
            }
            Ok(report)
        })
    }

    /// Reads a `known_hosts` file at an explicit path and imports it.
    pub fn import_known_hosts_file(&self, path: &Path) -> Result<KnownHostsImport> {
        let bytes = std::fs::read(path)?;
        self.import_known_hosts(&String::from_utf8_lossy(&bytes))
    }
}
