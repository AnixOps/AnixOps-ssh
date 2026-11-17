//! The daemon's known hosts: an app-owned file first, the user's
//! `~/.ssh/known_hosts` as a read-only fallback.
//!
//! - New and replaced keys are only ever written to the app's file.
//! - A host the app file does not know but the OpenSSH file does is trusted
//!   on that record, and the record is imported into the app file, so the
//!   host stays trusted if the OpenSSH file changes later.
//! - A key that differs from what either file records is `Changed`; it is
//!   never accepted automatically (the engine only accepts an explicit
//!   replacement). The app file wins when both know the host.

use easyssh_ssh::known_hosts::{KnownHostStatus, KnownHosts, KnownHostsFile};
use easyssh_ssh::Result;
use russh::keys::ssh_key::{Algorithm, PublicKey};
use std::path::PathBuf;

/// See the module documentation.
pub struct LayeredKnownHosts {
    primary: KnownHostsFile,
    fallback: Option<KnownHostsFile>,
}

impl LayeredKnownHosts {
    pub fn new(primary: impl Into<PathBuf>, fallback: Option<PathBuf>) -> Self {
        LayeredKnownHosts {
            primary: KnownHostsFile::new(primary),
            fallback: fallback.map(KnownHostsFile::new),
        }
    }

    /// The app-owned file.
    pub fn primary_path(&self) -> &std::path::Path {
        self.primary.path()
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
}
