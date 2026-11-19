//! Known hosts: an OpenSSH-format `known_hosts` reader and writer.
//!
//! Supported lines: plain host lists (`host`, `[host]:port`, `*` and `?`
//! wildcards, `!negation`), hashed entries (`|1|salt|hash`), `@revoked`.
//! A host with recorded keys of the presented key's type that presents a
//! different key is [`KnownHostStatus::Changed`], which is never accepted
//! automatically. As in OpenSSH, keys of other types do not count: a key of
//! a type not recorded yet is [`KnownHostStatus::OtherTypes`], asked about
//! like a new host. A host with an `@cert-authority` line is left to OpenSSH
//! ([`Error::Unsupported`] from [`check_text`]): it verifies host
//! certificates, which the engine does not.

use crate::error::{Error, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use russh::keys::ssh_key::{Algorithm, HashAlg, PublicKey};
use sha1::Sha1;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// What the store knows about a host key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnownHostStatus {
    /// The host has this exact key on record.
    Trusted,
    /// Nothing is recorded for this host.
    Unknown,
    /// Keys are recorded for this host and none of them is the presented
    /// one. Possible man-in-the-middle, or the server was reinstalled.
    Changed {
        /// SHA256 fingerprints of the recorded keys.
        expected: Vec<String>,
        /// Line number (1-based) of the first recorded entry.
        line: usize,
    },
    /// The presented key is listed as `@revoked`.
    Revoked,
    /// Keys are recorded for this host, but none of the presented key's
    /// type (the server gained a key type, or the engine negotiated another
    /// one than OpenSSH did). Not a change: the user is asked as for a new
    /// host, and accepting adds the key beside the others.
    OtherTypes {
        /// `type SHA256:...` of each recorded key.
        known: Vec<String>,
    },
}

/// A store of trusted host keys.
pub trait KnownHosts: Send + Sync {
    fn check(&self, host: &str, port: u16, key: &PublicKey) -> Result<KnownHostStatus>;
    /// Records a key for a host that had none (trust on first use).
    fn add(&self, host: &str, port: u16, key: &PublicKey) -> Result<()>;
    /// Removes what is recorded for the host and records `key` instead.
    fn replace(&self, host: &str, port: u16, key: &PublicKey) -> Result<()>;
    /// Key algorithms recorded for the host, so the engine can offer them
    /// first (as OpenSSH does) and avoid a false "changed" when the server
    /// has several host keys.
    fn recorded_algorithms(&self, _host: &str, _port: u16) -> Result<Vec<Algorithm>> {
        Ok(Vec::new())
    }
}

/// SHA256 fingerprint, `SHA256:base64`.
pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    None,
    CertAuthority,
    Revoked,
}

#[derive(Debug)]
enum Hosts {
    Hashed { salt: Vec<u8>, hash: Vec<u8> },
    Patterns(Vec<String>),
}

#[derive(Debug)]
struct Entry {
    marker: Marker,
    hosts: Hosts,
    key: PublicKey,
    line: usize,
}

/// `host` for port 22, `[host]:port` otherwise: what OpenSSH matches and
/// hashes.
fn token(host: &str, port: u16) -> String {
    let host = host.to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

fn candidate_tokens(host: &str, port: u16) -> Vec<String> {
    let mut tokens = vec![token(host, port)];
    if port == 22 {
        tokens.push(format!("[{}]:22", host.to_ascii_lowercase()));
    }
    tokens
}

/// `*` and `?` wildcards, case-insensitive.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_ascii_lowercase().chars().collect();
    let t: Vec<char> = text.to_ascii_lowercase().chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

impl Hosts {
    fn matches(&self, host: &str, port: u16) -> bool {
        let tokens = candidate_tokens(host, port);
        match self {
            Hosts::Hashed { salt, hash } => tokens.iter().any(|token| {
                let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(salt) else {
                    return false;
                };
                mac.update(token.as_bytes());
                mac.verify_slice(hash).is_ok()
            }),
            Hosts::Patterns(patterns) => {
                let mut positive = false;
                for pattern in patterns {
                    if let Some(negated) = pattern.strip_prefix('!') {
                        if tokens.iter().any(|t| glob_match(negated, t)) {
                            return false;
                        }
                    } else if tokens.iter().any(|t| glob_match(pattern, t)) {
                        positive = true;
                    }
                }
                positive
            }
        }
    }
}

fn parse_hosts(field: &str) -> Option<Hosts> {
    if let Some(rest) = field.strip_prefix("|1|") {
        let (salt, hash) = rest.split_once('|')?;
        return Some(Hosts::Hashed {
            salt: B64.decode(salt).ok()?,
            hash: B64.decode(hash).ok()?,
        });
    }
    Some(Hosts::Patterns(
        field
            .split(',')
            .filter(|p| !p.is_empty())
            .map(str::to_owned)
            .collect(),
    ))
}

fn parse_line(raw: &str, line: usize) -> Option<Entry> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let mut parts = trimmed.split_whitespace();
    let first = parts.next()?;
    let (marker, host_field) = if first.starts_with('@') {
        let marker = match first {
            "@cert-authority" => Marker::CertAuthority,
            "@revoked" => Marker::Revoked,
            _ => return None,
        };
        (marker, parts.next()?)
    } else {
        (Marker::None, first)
    };
    let key_type = parts.next()?;
    let key_data = parts.next()?;
    let key = PublicKey::from_openssh(&format!("{key_type} {key_data}")).ok()?;
    Some(Entry {
        marker,
        hosts: parse_hosts(host_field)?,
        key,
        line,
    })
}

fn same_key(a: &PublicKey, b: &PublicKey) -> bool {
    a.key_data() == b.key_data()
}

/// Whether two keys are of one type, as OpenSSH compares host keys
/// (`ecdsa-sha2-nistp256` and `-nistp384` are two types; RSA is one).
fn same_type(a: &PublicKey, b: &PublicKey) -> bool {
    a.algorithm() == b.algorithm()
}

/// Looks `key` up in the text of a known_hosts file.
pub fn evaluate(text: &str, host: &str, port: u16, key: &PublicKey) -> KnownHostStatus {
    let matching: Vec<Entry> = text
        .lines()
        .enumerate()
        .filter_map(|(index, raw)| parse_line(raw, index + 1))
        .filter(|entry| entry.hosts.matches(host, port))
        .collect();
    if matching
        .iter()
        .any(|e| e.marker == Marker::Revoked && same_key(&e.key, key))
    {
        return KnownHostStatus::Revoked;
    }
    let all: Vec<&Entry> = matching
        .iter()
        .filter(|e| e.marker == Marker::None)
        .collect();
    if all.is_empty() {
        return KnownHostStatus::Unknown;
    }
    let recorded: Vec<&Entry> = all
        .iter()
        .copied()
        .filter(|e| same_type(&e.key, key))
        .collect();
    let Some(first) = recorded.first() else {
        let mut known: Vec<String> = Vec::new();
        for entry in all {
            let item = format!("{} {}", entry.key.algorithm(), fingerprint(&entry.key));
            if !known.contains(&item) {
                known.push(item);
            }
        }
        return KnownHostStatus::OtherTypes { known };
    };
    if recorded.iter().any(|e| same_key(&e.key, key)) {
        return KnownHostStatus::Trusted;
    }
    KnownHostStatus::Changed {
        expected: recorded.iter().map(|e| fingerprint(&e.key)).collect(),
        line: first.line,
    }
}

/// Whether an `@cert-authority` line covers the host.
pub fn has_cert_authority(text: &str, host: &str, port: u16) -> bool {
    text.lines()
        .enumerate()
        .filter_map(|(index, raw)| parse_line(raw, index + 1))
        .any(|e| e.marker == Marker::CertAuthority && e.hosts.matches(host, port))
}

/// [`evaluate`], and a key that is not trusted for a host that has a
/// certificate authority is [`Error::Unsupported`]: OpenSSH, which verifies
/// host certificates, should make that connection.
pub fn check_text(text: &str, host: &str, port: u16, key: &PublicKey) -> Result<KnownHostStatus> {
    let status = evaluate(text, host, port, key);
    if matches!(status, KnownHostStatus::Trusted | KnownHostStatus::Revoked) {
        return Ok(status);
    }
    if has_cert_authority(text, host, port) {
        return Err(Error::Unsupported(format!(
            "known_hosts names a certificate authority (@cert-authority) for {}",
            token(host, port)
        )));
    }
    Ok(status)
}

/// Algorithms of the keys recorded for the host.
pub fn recorded_algorithms_in(text: &str, host: &str, port: u16) -> Vec<Algorithm> {
    let mut algorithms: Vec<Algorithm> = Vec::new();
    for entry in text
        .lines()
        .enumerate()
        .filter_map(|(index, raw)| parse_line(raw, index + 1))
        .filter(|e| e.marker == Marker::None && e.hosts.matches(host, port))
    {
        let algorithm = entry.key.algorithm();
        if !algorithms.contains(&algorithm) {
            algorithms.push(algorithm);
        }
    }
    algorithms
}

/// The line to append for a new entry.
fn entry_line(host: &str, port: u16, key: &PublicKey, hashed: bool) -> Result<String> {
    let token = token(host, port);
    let host_field = if hashed {
        let salt: [u8; 20] = rand::random();
        let mut mac = Hmac::<Sha1>::new_from_slice(&salt)
            .map_err(|e| Error::KnownHosts(format!("hashing host: {e}")))?;
        mac.update(token.as_bytes());
        format!(
            "|1|{}|{}",
            B64.encode(salt),
            B64.encode(mac.finalize().into_bytes())
        )
    } else {
        token
    };
    let key_text = key
        .to_openssh()
        .map_err(|e| Error::KnownHosts(format!("encoding host key: {e}")))?;
    // Never carry a comment into the file.
    let mut fields = key_text.split_whitespace();
    let (Some(kind), Some(data)) = (fields.next(), fields.next()) else {
        return Err(Error::KnownHosts("host key has no encoding".into()));
    };
    Ok(format!("{host_field} {kind} {data}\n"))
}

/// Appends an entry to the text.
pub fn append_entry(
    text: &mut String,
    host: &str,
    port: u16,
    key: &PublicKey,
    hashed: bool,
) -> Result<()> {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&entry_line(host, port, key, hashed)?);
    Ok(())
}

/// Removes what is recorded for the host. A line that lists several hosts
/// loses only this host's pattern.
pub fn remove_host(text: &str, host: &str, port: u16) -> String {
    let exact = token(host, port);
    let mut out = String::new();
    for (index, raw) in text.lines().enumerate() {
        let keep = match parse_line(raw, index + 1) {
            Some(entry) if entry.marker == Marker::None && entry.hosts.matches(host, port) => {
                match &entry.hosts {
                    Hosts::Patterns(patterns) if patterns.len() > 1 => {
                        let remaining: Vec<&String> = patterns
                            .iter()
                            .filter(|p| !p.eq_ignore_ascii_case(&exact))
                            .collect();
                        if remaining.len() == patterns.len() {
                            // Matched by a wildcard: the line cannot be
                            // edited per host, drop it whole.
                            None
                        } else {
                            let trimmed = raw.trim_start();
                            let rest = trimmed
                                .split_once(char::is_whitespace)
                                .map(|(_, rest)| rest)
                                .unwrap_or("");
                            let joined = remaining
                                .iter()
                                .map(|p| p.as_str())
                                .collect::<Vec<_>>()
                                .join(",");
                            Some(format!("{joined} {rest}"))
                        }
                    }
                    _ => None,
                }
            }
            _ => Some(raw.to_owned()),
        };
        if let Some(line) = keep {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// A known_hosts file on disk. The file is read on every check, so edits
/// made by other programs are seen at once.
#[derive(Debug, Clone)]
pub struct KnownHostsFile {
    path: PathBuf,
    hash_new_entries: bool,
}

impl KnownHostsFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            hash_new_entries: false,
        }
    }

    /// Writes new entries in hashed form (`HashKnownHosts yes`).
    pub fn hash_new_entries(mut self, hashed: bool) -> Self {
        self.hash_new_entries = hashed;
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read(&self) -> Result<String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(Error::KnownHosts(format!("{}: {e}", self.path.display()))),
        }
    }

    fn write(&self, text: &str) -> Result<()> {
        let io = |e: std::io::Error| Error::KnownHosts(format!("{}: {e}", self.path.display()));
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(io)?;
            }
        }
        let mut temp = self.path.clone().into_os_string();
        temp.push(".easyssh-tmp");
        let temp = PathBuf::from(temp);
        std::fs::write(&temp, text).map_err(io)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600)).map_err(io)?;
        }
        std::fs::rename(&temp, &self.path).map_err(io)
    }
}

impl KnownHosts for KnownHostsFile {
    fn check(&self, host: &str, port: u16, key: &PublicKey) -> Result<KnownHostStatus> {
        check_text(&self.read()?, host, port, key)
    }

    fn add(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        let mut text = self.read()?;
        append_entry(&mut text, host, port, key, self.hash_new_entries)?;
        self.write(&text)
    }

    fn replace(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        let mut text = remove_host(&self.read()?, host, port);
        append_entry(&mut text, host, port, key, self.hash_new_entries)?;
        self.write(&text)
    }

    fn recorded_algorithms(&self, host: &str, port: u16) -> Result<Vec<Algorithm>> {
        Ok(recorded_algorithms_in(&self.read()?, host, port))
    }
}

/// A store that lives in memory only. The default when no file is given:
/// every host is unknown at the start of each process.
#[derive(Debug, Default)]
pub struct MemoryKnownHosts {
    text: Mutex<String>,
}

impl MemoryKnownHosts {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, String> {
        self.text.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl KnownHosts for MemoryKnownHosts {
    fn check(&self, host: &str, port: u16, key: &PublicKey) -> Result<KnownHostStatus> {
        check_text(&self.lock(), host, port, key)
    }

    fn add(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        append_entry(&mut self.lock(), host, port, key, false)
    }

    fn replace(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        let mut text = self.lock();
        *text = remove_host(&text, host, port);
        append_entry(&mut text, host, port, key, false)
    }

    fn recorded_algorithms(&self, host: &str, port: u16) -> Result<Vec<Algorithm>> {
        Ok(recorded_algorithms_in(&self.lock(), host, port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const K1: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIEyeW80014PoyoF1sMDe9uN+k8D2Pqj1b5BXR3dRAkRj";
    const K2: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKxXnqrgYJGHpjbb27Ddo024CtOCBG5K2xV1JzRg3eRu";
    // Made with `ssh-keygen -H` from `[host.example]:2222 K1` and `example.org K1`.
    const HASHED_PORT: &str = "|1|QY9i9kcV7WmCMoWKUyMYgTG6ouA=|nUx+No+yjBV4P0hqsxQhmX0fU88=";
    const HASHED_PLAIN: &str = "|1|KIaucvGwbhu7XaRfWUabUGWzSF0=|ZAF4AF9i3kp0NIjYbfYyjoaS25o=";

    fn key(text: &str) -> PublicKey {
        PublicKey::from_openssh(text).unwrap()
    }

    #[test]
    fn plain_entry_on_default_port() {
        let text = format!("# comment\n\nexample.org {K1}\n");
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&text, "EXAMPLE.org", 22, &key(K1)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&text, "other.org", 22, &key(K1)),
            KnownHostStatus::Unknown
        );
    }

    #[test]
    fn port_form_is_distinct_from_default_port() {
        let text = format!("[example.org]:2222 {K1}\n");
        assert_eq!(
            evaluate(&text, "example.org", 2222, &key(K1)),
            KnownHostStatus::Trusted
        );
        // The same host on another port is another entry.
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Unknown
        );
        assert_eq!(
            evaluate(&text, "example.org", 2223, &key(K1)),
            KnownHostStatus::Unknown
        );
    }

    #[test]
    fn bracketed_port_22_matches_default_port() {
        let text = format!("[example.org]:22 {K1}\n");
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn mismatch_is_a_distinct_status() {
        let text = format!("example.org {K1}\n");
        match evaluate(&text, "example.org", 22, &key(K2)) {
            KnownHostStatus::Changed { expected, line } => {
                assert_eq!(line, 1);
                assert_eq!(expected, vec![fingerprint(&key(K1))]);
            }
            other => panic!("expected Changed, got {other:?}"),
        }
    }

    #[test]
    fn any_recorded_key_may_match() {
        let text = format!("example.org {K2}\nexample.org {K1}\n");
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn hashed_entries_match_with_and_without_port() {
        let text = format!("{HASHED_PORT} {K1}\n{HASHED_PLAIN} {K1}\n");
        assert_eq!(
            evaluate(&text, "host.example", 2222, &key(K1)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&text, "host.example", 22, &key(K1)),
            KnownHostStatus::Unknown
        );
        assert!(matches!(
            evaluate(&text, "example.org", 22, &key(K2)),
            KnownHostStatus::Changed { line: 2, .. }
        ));
    }

    #[test]
    fn host_lists_wildcards_and_negation() {
        let text = format!("*.example.org,!bad.example.org,10.0.0.? {K1}\n");
        let trusted = |host: &str| evaluate(&text, host, 22, &key(K1));
        assert_eq!(trusted("a.example.org"), KnownHostStatus::Trusted);
        assert_eq!(trusted("10.0.0.7"), KnownHostStatus::Trusted);
        assert_eq!(trusted("bad.example.org"), KnownHostStatus::Unknown);
        assert_eq!(trusted("example.com"), KnownHostStatus::Unknown);
    }

    #[test]
    fn revoked_key_is_flagged_and_a_cert_authority_is_left_to_openssh() {
        let text = format!("@revoked example.org {K1}\n@cert-authority *.org {K2}\n");
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Revoked
        );
        assert_eq!(
            check_text(&text, "example.org", 22, &key(K1)).unwrap(),
            KnownHostStatus::Revoked
        );
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K2)),
            KnownHostStatus::Unknown
        );
        let error = check_text(&text, "example.org", 22, &key(K2)).unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
        // Another host is not covered by the authority.
        assert_eq!(
            check_text(&text, "example.com", 22, &key(K2)).unwrap(),
            KnownHostStatus::Unknown
        );
        // A plain key on record still wins over the authority.
        let trusted = format!("{text}example.org {K2}\n");
        assert_eq!(
            check_text(&trusted, "example.org", 22, &key(K2)).unwrap(),
            KnownHostStatus::Trusted
        );
        let file = MemoryKnownHosts::new();
        *file.lock() = text;
        assert!(matches!(
            file.check("example.org", 22, &key(K2)),
            Err(Error::Unsupported(_))
        ));
    }

    const ECDSA: &str = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBGraOqigrA2J6S48cVOGbRXlRQ3KRsSMnWw3Td9jEP9QjIXLCOqVgMjQEVU7poDOolrgePDITb5Y4c5yppo7Z0Q=";

    #[test]
    fn only_keys_of_the_presented_type_can_change() {
        let text = format!("example.org {ECDSA}\n");
        // An ed25519 key where only an ECDSA key is recorded: a new type,
        // not a change.
        match evaluate(&text, "example.org", 22, &key(K1)) {
            KnownHostStatus::OtherTypes { known } => {
                assert_eq!(
                    known,
                    vec![format!("ecdsa-sha2-nistp256 {}", fingerprint(&key(ECDSA)))]
                );
            }
            other => panic!("expected OtherTypes, got {other:?}"),
        }
        // With an ed25519 key on record, another ed25519 key is a change.
        let text = format!("{text}example.org {K2}\n");
        assert!(matches!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Changed { line: 2, .. }
        ));
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(ECDSA)),
            KnownHostStatus::Trusted
        );
        // Accepting the new type adds it beside the old one.
        let store = MemoryKnownHosts::new();
        store.add("example.org", 22, &key(ECDSA)).unwrap();
        store.add("example.org", 22, &key(K1)).unwrap();
        assert_eq!(
            store.check("example.org", 22, &key(ECDSA)).unwrap(),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            store.check("example.org", 22, &key(K1)).unwrap(),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn garbage_lines_are_skipped() {
        let text =
            format!("not a known hosts line\nexample.org ssh-ed25519 !!!\nexample.org {K1}\n");
        assert_eq!(
            evaluate(&text, "example.org", 22, &key(K1)),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn append_writes_plain_and_hashed_entries() {
        let mut text = String::from("existing.org ssh-ed25519 AAAA-not-checked");
        append_entry(&mut text, "example.org", 2222, &key(K1), false).unwrap();
        assert!(text.contains("\n[example.org]:2222 ssh-ed25519 "), "{text}");
        append_entry(&mut text, "hashed.org", 22, &key(K2), true).unwrap();
        assert!(text.contains("\n|1|"), "{text}");
        assert!(!text.contains("hashed.org"));
        assert_eq!(
            evaluate(&text, "example.org", 2222, &key(K1)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&text, "hashed.org", 22, &key(K2)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&text, "hashed.org", 2222, &key(K2)),
            KnownHostStatus::Unknown
        );
    }

    #[test]
    fn remove_host_keeps_other_hosts() {
        let text = format!("a.org,b.org {K1}\nc.org {K2}\n[a.org]:2222 {K2}\n");
        let removed = remove_host(&text, "a.org", 22);
        assert_eq!(
            evaluate(&removed, "a.org", 22, &key(K1)),
            KnownHostStatus::Unknown
        );
        assert_eq!(
            evaluate(&removed, "b.org", 22, &key(K1)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&removed, "c.org", 22, &key(K2)),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            evaluate(&removed, "a.org", 2222, &key(K2)),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn file_store_round_trip_and_replace() {
        let dir = tempfile::tempdir().unwrap();
        let file = KnownHostsFile::new(dir.path().join("sub").join("known_hosts"));
        let k1 = key(K1);
        let k2 = key(K2);
        assert_eq!(
            file.check("h.example", 2222, &k1).unwrap(),
            KnownHostStatus::Unknown
        );
        file.add("h.example", 2222, &k1).unwrap();
        assert_eq!(
            file.check("h.example", 2222, &k1).unwrap(),
            KnownHostStatus::Trusted
        );
        assert!(matches!(
            file.check("h.example", 2222, &k2).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
        assert_eq!(
            file.recorded_algorithms("h.example", 2222).unwrap(),
            vec![Algorithm::Ed25519]
        );
        file.replace("h.example", 2222, &k2).unwrap();
        assert_eq!(
            file.check("h.example", 2222, &k2).unwrap(),
            KnownHostStatus::Trusted
        );
        assert!(matches!(
            file.check("h.example", 2222, &k1).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
        let written = std::fs::read_to_string(file.path()).unwrap();
        assert_eq!(written.lines().count(), 1, "{written}");
    }

    #[test]
    fn hashing_file_store_does_not_leak_the_host() {
        let dir = tempfile::tempdir().unwrap();
        let file = KnownHostsFile::new(dir.path().join("kh")).hash_new_entries(true);
        let k1 = key(K1);
        file.add("secret.example", 22, &k1).unwrap();
        let written = std::fs::read_to_string(file.path()).unwrap();
        assert!(!written.contains("secret.example"));
        assert_eq!(
            file.check("secret.example", 22, &k1).unwrap(),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn memory_store_behaves_like_the_file_store() {
        let store = MemoryKnownHosts::new();
        let k1 = key(K1);
        assert_eq!(store.check("h", 22, &k1).unwrap(), KnownHostStatus::Unknown);
        store.add("h", 22, &k1).unwrap();
        assert_eq!(store.check("h", 22, &k1).unwrap(), KnownHostStatus::Trusted);
    }
}
