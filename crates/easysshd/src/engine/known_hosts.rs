//! The daemon's known hosts: the store's `known_hosts` table first (it syncs
//! with the vault), the user's `~/.ssh/known_hosts` as a read-only fallback.
//!
//! - New and replaced keys are only ever written to the store.
//! - A host the store does not know but the OpenSSH file does is trusted
//!   on that record, and the record is imported into the store, so the
//!   host stays trusted if the OpenSSH file changes later.
//! - A key that differs from what either source records is `Changed`; it is
//!   never accepted automatically (the engine only accepts an explicit
//!   replacement). The store wins when both know the host.
//! - The file the engine used before the store existed is imported once
//!   ([`migrate_app_file`]) and then renamed.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use easyssh_ssh::known_hosts::{
    evaluate, recorded_algorithms_in, remove_host, KnownHostStatus, KnownHosts, KnownHostsFile,
};
use easyssh_ssh::{Error, Result};
use easyssh_store::{KnownHost, Store};
use russh::keys::ssh_key::{Algorithm, PublicKey};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Host keys in the app database.
pub struct StoreKnownHosts {
    store: Arc<Mutex<Store>>,
}

fn failed(error: impl std::fmt::Display) -> Error {
    Error::KnownHosts(format!("the known hosts store: {error}"))
}

/// The `known_hosts` line of a stored entry.
fn line_of(entry: &KnownHost) -> String {
    let marker = entry
        .marker
        .as_deref()
        .map(|m| format!("{m} "))
        .unwrap_or_default();
    format!(
        "{marker}{} {} {}",
        entry.host,
        entry.key_type,
        B64.encode(&entry.key_blob)
    )
}

/// The key type and raw blob of `key`, as stored.
fn parts_of(key: &PublicKey) -> Result<(String, Vec<u8>)> {
    let text = key
        .to_openssh()
        .map_err(|e| Error::KnownHosts(format!("encoding host key: {e}")))?;
    let mut fields = text.split_whitespace();
    let (Some(kind), Some(data)) = (fields.next(), fields.next()) else {
        return Err(Error::KnownHosts("host key has no encoding".into()));
    };
    let blob = B64
        .decode(data)
        .map_err(|e| Error::KnownHosts(format!("encoding host key: {e}")))?;
    Ok((kind.to_owned(), blob))
}

impl StoreKnownHosts {
    pub fn new(store: Arc<Mutex<Store>>) -> Self {
        StoreKnownHosts { store }
    }

    fn text(&self) -> Result<String> {
        let entries = lock(&self.store).list_known_hosts().map_err(failed)?;
        let mut text = String::new();
        for record in entries {
            text.push_str(&line_of(&record.item));
            text.push('\n');
        }
        Ok(text)
    }
}

impl KnownHosts for StoreKnownHosts {
    fn check(&self, host: &str, port: u16, key: &PublicKey) -> Result<KnownHostStatus> {
        Ok(evaluate(&self.text()?, host, port, key))
    }

    fn add(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        let (kind, blob) = parts_of(key)?;
        let entry = KnownHost::new(&host.to_ascii_lowercase(), port, &kind, blob);
        lock(&self.store)
            .upsert_known_host(&entry)
            .map_err(failed)?;
        Ok(())
    }

    fn replace(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        {
            let store = lock(&self.store);
            for record in store.list_known_hosts().map_err(failed)? {
                let line = line_of(&record.item);
                let kept = remove_host(&line, host, port);
                let kept = kept.trim_end();
                if kept == line {
                    continue;
                }
                if kept.is_empty() {
                    store
                        .soft_delete_known_host(&record.item.id)
                        .map_err(failed)?;
                } else if let Some(rest) = kept.split_whitespace().next() {
                    // One line that listed several hosts loses only this one.
                    let mut entry = record.item.clone();
                    entry.host = rest.to_owned();
                    store.upsert_known_host(&entry).map_err(failed)?;
                }
            }
        }
        self.add(host, port, key)
    }

    fn recorded_algorithms(&self, host: &str, port: u16) -> Result<Vec<Algorithm>> {
        Ok(recorded_algorithms_in(&self.text()?, host, port))
    }
}

/// Imports the file the built-in engine kept before host keys moved into the
/// store, then renames it to `<name>.migrated` so it happens once. Returns
/// how many entries were new. A missing file is not an error; a file that
/// cannot be imported stays where it is.
pub fn migrate_app_file(store: &Store, file: &Path) -> std::io::Result<usize> {
    if !file.is_file() {
        return Ok(0);
    }
    let report = store
        .import_known_hosts_file(file)
        .map_err(std::io::Error::other)?;
    let mut moved = file.as_os_str().to_owned();
    moved.push(".migrated");
    // If this fails the next start imports again; duplicates are skipped.
    let _ = std::fs::rename(file, PathBuf::from(moved));
    Ok(report.imported)
}

/// See the module documentation.
pub struct LayeredKnownHosts {
    primary: Arc<dyn KnownHosts>,
    fallback: Option<KnownHostsFile>,
}

impl LayeredKnownHosts {
    /// A file as the primary store (what the engine used before the
    /// database; kept for tests and for a daemon without one).
    pub fn new(primary: impl Into<PathBuf>, fallback: Option<PathBuf>) -> Self {
        Self::with_primary(Arc::new(KnownHostsFile::new(primary)), fallback)
    }

    pub fn with_primary(primary: Arc<dyn KnownHosts>, fallback: Option<PathBuf>) -> Self {
        LayeredKnownHosts {
            primary,
            fallback: fallback.map(KnownHostsFile::new),
        }
    }
}

impl KnownHosts for LayeredKnownHosts {
    fn check(&self, host: &str, port: u16, key: &PublicKey) -> Result<KnownHostStatus> {
        let own = self.primary.check(host, port, key)?;
        if own != KnownHostStatus::Unknown {
            return Ok(own);
        }
        let Some(fallback) = &self.fallback else {
            return Ok(own);
        };
        let status = fallback.check(host, port, key)?;
        if status == KnownHostStatus::Trusted {
            // Import it. A failure to write only means the next check asks
            // the fallback again.
            let _ = self.primary.add(host, port, key);
        }
        Ok(status)
    }

    fn add(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        self.primary.add(host, port, key)
    }

    fn replace(&self, host: &str, port: u16, key: &PublicKey) -> Result<()> {
        self.primary.replace(host, port, key)
    }

    fn recorded_algorithms(&self, host: &str, port: u16) -> Result<Vec<Algorithm>> {
        let mut algorithms = self.primary.recorded_algorithms(host, port)?;
        if algorithms.is_empty() {
            if let Some(fallback) = &self.fallback {
                algorithms = fallback.recorded_algorithms(host, port)?;
            }
        }
        Ok(algorithms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::private::{Ed25519Keypair, KeypairData};
    use russh::keys::ssh_key::PrivateKey;

    fn key(seed: u8) -> PublicKey {
        let pair = Ed25519Keypair::from_seed(&[seed; 32]);
        PrivateKey::new(KeypairData::Ed25519(pair), "")
            .unwrap()
            .public_key()
            .clone()
    }

    fn setup() -> (tempfile::TempDir, LayeredKnownHosts, KnownHostsFile) {
        let dir = tempfile::tempdir().unwrap();
        let layered = LayeredKnownHosts::new(
            dir.path().join("app_known_hosts"),
            Some(dir.path().join("user_known_hosts")),
        );
        let user = KnownHostsFile::new(dir.path().join("user_known_hosts"));
        (dir, layered, user)
    }

    #[test]
    fn unknown_everywhere_is_unknown_and_new_keys_go_to_the_app_file() {
        let (dir, layered, _) = setup();
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Unknown
        );
        layered.add("h", 22, &key(1)).unwrap();
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        assert!(dir.path().join("app_known_hosts").is_file());
        assert!(!dir.path().join("user_known_hosts").exists());
    }

    #[test]
    fn the_openssh_file_is_trusted_and_imported() {
        let (dir, layered, user) = setup();
        user.add("h", 22, &key(1)).unwrap();
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        // Imported: still trusted when the user's file loses the entry.
        std::fs::remove_file(dir.path().join("user_known_hosts")).unwrap();
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn a_key_that_differs_from_the_openssh_file_is_changed() {
        let (dir, layered, user) = setup();
        user.add("h", 22, &key(1)).unwrap();
        assert!(matches!(
            layered.check("h", 22, &key(2)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
        // The fallback is never written to, by checks or by replacing.
        let before = std::fs::read_to_string(dir.path().join("user_known_hosts")).unwrap();
        layered.replace("h", 22, &key(2)).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("user_known_hosts")).unwrap(),
            before
        );
        // After the explicit replacement the app file answers.
        assert_eq!(
            layered.check("h", 22, &key(2)).unwrap(),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn the_app_file_wins_and_a_changed_key_is_never_added() {
        let (_dir, layered, user) = setup();
        layered.add("h", 22, &key(1)).unwrap();
        user.add("h", 22, &key(2)).unwrap();
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        // A third key matches neither: changed, and checking records nothing.
        assert!(matches!(
            layered.check("h", 22, &key(3)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
        assert!(matches!(
            layered.check("h", 22, &key(3)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
    }

    #[test]
    fn without_a_fallback_only_the_app_file_counts() {
        let dir = tempfile::tempdir().unwrap();
        let layered = LayeredKnownHosts::new(dir.path().join("a"), None);
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Unknown
        );
    }

    // ---- the store as the primary ----------------------------------------

    /// A database in its own directory (kept alive with the handle).
    fn store() -> (tempfile::TempDir, Arc<Mutex<Store>>) {
        let dir = tempfile::tempdir().unwrap();
        let opened = easyssh_store::open_app_store(&dir.path().join("connections.json")).unwrap();
        (dir, Arc::new(Mutex::new(opened.store)))
    }

    fn in_store(store: &Arc<Mutex<Store>>) -> Vec<String> {
        let mut lines: Vec<String> = lock(store)
            .list_known_hosts()
            .unwrap()
            .iter()
            .map(|r| line_of(&r.item))
            .collect();
        lines.sort();
        lines
    }

    #[test]
    fn the_store_remembers_new_keys_and_never_accepts_a_changed_one() {
        let (_db, store) = store();
        let known = StoreKnownHosts::new(store.clone());
        assert_eq!(
            known.check("Host.Example", 2222, &key(1)).unwrap(),
            KnownHostStatus::Unknown
        );
        known.add("Host.Example", 2222, &key(1)).unwrap();
        assert_eq!(
            known.check("host.example", 2222, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        // Recorded as OpenSSH would write it, so it also syncs and exports.
        let lines = in_store(&store);
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].starts_with("[host.example]:2222 ssh-ed25519 "),
            "{lines:?}"
        );
        // Another port and another key are not the same thing.
        assert_eq!(
            known.check("host.example", 22, &key(1)).unwrap(),
            KnownHostStatus::Unknown
        );
        assert!(matches!(
            known.check("host.example", 2222, &key(2)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
        // Checking records nothing.
        assert_eq!(in_store(&store).len(), 1);
        assert_eq!(
            known.recorded_algorithms("host.example", 2222).unwrap(),
            vec![Algorithm::Ed25519]
        );
    }

    #[test]
    fn replacing_in_the_store_drops_the_old_key_and_only_for_that_host() {
        let (_db, store) = store();
        let known = StoreKnownHosts::new(store.clone());
        known.add("a.example", 22, &key(1)).unwrap();
        known.add("b.example", 22, &key(1)).unwrap();
        known.replace("a.example", 22, &key(2)).unwrap();
        assert_eq!(
            known.check("a.example", 22, &key(2)).unwrap(),
            KnownHostStatus::Trusted
        );
        assert!(matches!(
            known.check("a.example", 22, &key(1)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
        assert_eq!(
            known.check("b.example", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        // The old entry is a tombstone (so sync carries the removal).
        assert_eq!(lock(&store).list_deleted_known_hosts().unwrap().len(), 1);
    }

    #[test]
    fn replacing_one_host_of_a_list_line_keeps_the_others() {
        let (_db, store) = store();
        let blob = {
            let (kind, blob) = parts_of(&key(1)).unwrap();
            assert_eq!(kind, "ssh-ed25519");
            blob
        };
        let mut entry = KnownHost::new("placeholder", 22, "ssh-ed25519", blob);
        entry.host = "a.example,b.example".into();
        lock(&store).upsert_known_host(&entry).unwrap();
        let known = StoreKnownHosts::new(store);
        known.replace("a.example", 22, &key(2)).unwrap();
        assert_eq!(
            known.check("b.example", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            known.check("a.example", 22, &key(2)).unwrap(),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn hashed_entries_imported_into_the_store_still_match() {
        let (_db, store) = store();
        let known = StoreKnownHosts::new(store.clone());
        known.add("h.example", 22, &key(1)).unwrap();
        // The same host as OpenSSH writes it with HashKnownHosts.
        let mut text = String::new();
        easyssh_ssh::known_hosts::append_entry(&mut text, "hashed.example", 22, &key(3), true)
            .unwrap();
        assert!(text.starts_with("|1|"));
        let report = lock(&store).import_known_hosts(&text).unwrap();
        assert_eq!(report.imported, 1);
        assert_eq!(
            known.check("hashed.example", 22, &key(3)).unwrap(),
            KnownHostStatus::Trusted
        );
        assert!(matches!(
            known.check("hashed.example", 22, &key(1)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
    }

    #[test]
    fn the_openssh_file_is_a_read_only_fallback_behind_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let (_db, store) = store();
        let user_path = dir.path().join("user_known_hosts");
        let user = KnownHostsFile::new(&user_path);
        user.add("h", 22, &key(1)).unwrap();
        let layered = LayeredKnownHosts::with_primary(
            Arc::new(StoreKnownHosts::new(store.clone())),
            Some(user_path.clone()),
        );
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        // Imported into the store, so it survives the file going away.
        assert_eq!(in_store(&store).len(), 1);
        std::fs::remove_file(&user_path).unwrap();
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        // A different key is changed and is not recorded by the check.
        user.add("other", 22, &key(1)).unwrap();
        assert!(matches!(
            layered.check("other", 22, &key(2)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
        assert_eq!(in_store(&store).len(), 1);
        // Only an explicit replacement writes, and only to the store.
        let before = std::fs::read_to_string(&user_path).unwrap();
        layered.replace("other", 22, &key(2)).unwrap();
        assert_eq!(std::fs::read_to_string(&user_path).unwrap(), before);
        assert_eq!(
            layered.check("other", 22, &key(2)).unwrap(),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn the_store_wins_over_the_openssh_file() {
        let dir = tempfile::tempdir().unwrap();
        let (_db, store) = store();
        let user_path = dir.path().join("user_known_hosts");
        KnownHostsFile::new(&user_path)
            .add("h", 22, &key(2))
            .unwrap();
        let layered =
            LayeredKnownHosts::with_primary(Arc::new(StoreKnownHosts::new(store)), Some(user_path));
        layered.add("h", 22, &key(1)).unwrap();
        assert_eq!(
            layered.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        assert!(matches!(
            layered.check("h", 22, &key(3)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
    }

    #[test]
    fn the_app_file_moves_into_the_store_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_hosts");
        let old = KnownHostsFile::new(&file);
        old.add("h", 22, &key(1)).unwrap();
        old.add("i.example", 2222, &key(2)).unwrap();
        let (_db, store) = store();
        assert_eq!(migrate_app_file(&lock(&store), &file).unwrap(), 2);
        assert!(!file.exists());
        assert!(dir.path().join("known_hosts.migrated").is_file());
        let known = StoreKnownHosts::new(store.clone());
        assert_eq!(
            known.check("h", 22, &key(1)).unwrap(),
            KnownHostStatus::Trusted
        );
        assert_eq!(
            known.check("i.example", 2222, &key(2)).unwrap(),
            KnownHostStatus::Trusted
        );
        // Nothing left to do the second time, and nothing is duplicated.
        assert_eq!(migrate_app_file(&lock(&store), &file).unwrap(), 0);
        assert_eq!(in_store(&store).len(), 2);
    }

    #[test]
    fn a_changed_key_in_the_old_file_stays_changed_after_the_move() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("known_hosts");
        KnownHostsFile::new(&file).add("h", 22, &key(1)).unwrap();
        let (_db, store) = store();
        migrate_app_file(&lock(&store), &file).unwrap();
        let known = StoreKnownHosts::new(store);
        assert!(matches!(
            known.check("h", 22, &key(2)).unwrap(),
            KnownHostStatus::Changed { .. }
        ));
    }
}
