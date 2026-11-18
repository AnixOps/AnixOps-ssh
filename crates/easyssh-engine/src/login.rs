//! Saved logins: the private key, key passphrase and password a host has in
//! the vault, handed to the connection from memory.
//!
//! The same rules as the desktop daemon's russh engine: a saved key (with its
//! passphrase) becomes an in-memory identity that is never written to a file;
//! a saved password answers exactly that user's password prompt, once, and
//! never again after the server refused it. A locked vault is the error
//! `vault_locked`, so the UI can ask for the unlock and retry.

use crate::{OpenError, Target};
use easyssh_ssh::MemoryIdentity;
use easyssh_vault::{SecretSource, VaultError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// A saved password and the prompt it belongs to.
pub struct SavedPassword {
    /// `user@host`, which the engine's password prompt starts with.
    account: String,
    secrets: Arc<dyn SecretSource>,
    host_id: String,
    used: AtomicBool,
}

impl std::fmt::Debug for SavedPassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedPassword")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

impl SavedPassword {
    pub fn new(account: String, secrets: Arc<dyn SecretSource>, host_id: String) -> Self {
        Self {
            account,
            secrets,
            host_id,
            used: AtomicBool::new(false),
        }
    }

    /// The password for `prompt` when it asks for this account's password and
    /// the saved one was not tried yet. A locked vault or a missing entry
    /// means: ask the user.
    pub fn take(&self, prompt: &str) -> Option<String> {
        if !prompt.starts_with(&format!("{}'s password", self.account)) {
            return None;
        }
        if self.used.swap(true, Ordering::SeqCst) {
            return None;
        }
        let secrets = self.secrets.host_secrets(&self.host_id).ok()?;
        secrets
            .password
            .map(|password| password.expose().to_owned())
    }
}

/// What the vault holds for one connection attempt.
#[derive(Default, Debug)]
pub struct SavedLogin {
    pub identity: Option<MemoryIdentity>,
    pub password: Option<Arc<SavedPassword>>,
}

fn vault_error(error: VaultError) -> OpenError {
    match error {
        VaultError::Locked => OpenError {
            code: "vault_locked",
            message: "the vault is locked; unlock it to use the saved key".into(),
        },
        other => OpenError {
            code: "ssh_error",
            message: format!("cannot read the saved key: {other}"),
        },
    }
}

/// The login saved for `host_id` and how to use it for `target`. Nothing
/// saved is not an error. A saved key with a locked vault is.
pub fn saved_login(
    secrets: &Arc<dyn SecretSource>,
    host_id: &str,
    target: &Target,
) -> Result<SavedLogin, OpenError> {
    let flags = secrets.secret_flags(host_id).map_err(vault_error)?;
    let mut login = SavedLogin::default();
    if flags.private_key {
        let saved = secrets.host_secrets(host_id).map_err(vault_error)?;
        login.identity = saved.private_key.map(|key| {
            MemoryIdentity::new(
                format!("saved key of {}", target.label),
                key.expose(),
                saved.passphrase.map(|p| p.expose().to_owned()),
            )
        });
    }
    if flags.password {
        login.password = Some(Arc::new(SavedPassword::new(
            format!("{}@{}", target.user, target.host),
            secrets.clone(),
            host_id.to_owned(),
        )));
    }
    Ok(login)
}

/// `target` with the vault's saved login filled in: the key and password of the
/// host and of each jump hop that is a saved host. Every
/// way of opening a host (terminal, files, forward) goes through this, so they
/// all make the same connection and share it. Nothing saved leaves the target
/// as it is; a saved key with a locked vault is the error `vault_locked`.
pub fn with_saved_login(
    secrets: &Arc<dyn SecretSource>,
    mut target: Target,
) -> Result<Target, OpenError> {
    for hop in &mut target.jump {
        // A hop that is only an address (`jump:` ids) has nothing saved.
        if !hop.host_id.starts_with("jump:") {
            let login = saved_login(secrets, &hop.host_id, hop)?;
            hop.identity = login.identity;
            hop.saved_password = login.password;
        }
    }
    let login = saved_login(secrets, &target.host_id, &target)?;
    target.identity = login.identity;
    target.saved_password = login.password;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_crypto::SecretText;
    use easyssh_store::{SecretFlags, StoredSecrets};

    struct Saved {
        password: Option<&'static str>,
        key: Option<&'static str>,
        passphrase: Option<&'static str>,
        locked: bool,
    }

    impl SecretSource for Saved {
        fn host_secrets(&self, _: &str) -> Result<StoredSecrets, VaultError> {
            if self.locked {
                return Err(VaultError::Locked);
            }
            Ok(StoredSecrets {
                password: self.password.map(SecretText::new),
                private_key: self.key.map(SecretText::new),
                passphrase: self.passphrase.map(SecretText::new),
            })
        }

        fn secret_flags(&self, _: &str) -> Result<SecretFlags, VaultError> {
            Ok(SecretFlags {
                password: self.password.is_some(),
                private_key: self.key.is_some(),
                passphrase: self.passphrase.is_some(),
            })
        }
    }

    fn target() -> Target {
        Target {
            host_id: "h1".into(),
            label: "web".into(),
            host: "example.com".into(),
            port: 22,
            user: "me".into(),
            identity: None,
            jump: Vec::new(),
            saved_password: None,
        }
    }

    fn source(saved: Saved) -> Arc<dyn SecretSource> {
        Arc::new(saved)
    }

    #[test]
    fn a_saved_key_and_passphrase_become_an_in_memory_identity() {
        let login = saved_login(
            &source(Saved {
                password: None,
                key: Some("PEM"),
                passphrase: Some("hunter2"),
                locked: false,
            }),
            "h",
            &target(),
        )
        .unwrap();
        let identity = login.identity.unwrap();
        assert_eq!(identity.label, "saved key of web");
        assert!(login.password.is_none());
        // The key text is not printed by Debug.
        assert!(!format!("{identity:?}").contains("PEM"));
    }

    #[test]
    fn nothing_saved_is_an_empty_login() {
        let login = saved_login(
            &source(Saved {
                password: None,
                key: None,
                passphrase: None,
                locked: true,
            }),
            "h",
            &target(),
        )
        .unwrap();
        assert!(login.identity.is_none() && login.password.is_none());
    }

    #[test]
    fn a_saved_key_with_a_locked_vault_asks_for_the_unlock() {
        let error = saved_login(
            &source(Saved {
                password: None,
                key: Some("PEM"),
                passphrase: None,
                locked: true,
            }),
            "h",
            &target(),
        )
        .unwrap_err();
        assert_eq!(error.code, "vault_locked");
        assert!(error.message.contains("the vault is locked"));
    }

    #[test]
    fn a_saved_password_answers_only_its_own_prompt_and_only_once() {
        let login = saved_login(
            &source(Saved {
                password: Some("pw"),
                key: None,
                passphrase: None,
                locked: false,
            }),
            "h",
            &target(),
        )
        .unwrap();
        let saved = login.password.unwrap();
        // Another user (or a jump host) is never answered from the vault.
        assert_eq!(saved.take("you@example.com's password"), None);
        assert_eq!(saved.take("me@other.com's password"), None);
        assert_eq!(
            saved.take("me@example.com's password: ").as_deref(),
            Some("pw")
        );
        // The server refused it: the next prompt goes to the user.
        assert_eq!(saved.take("me@example.com's password: "), None);
        assert!(!format!("{saved:?}").contains("pw\""));
    }

    #[test]
    fn a_locked_vault_at_the_prompt_means_ask_the_user() {
        let saved = SavedPassword::new(
            "me@example.com".into(),
            source(Saved {
                password: Some("pw"),
                key: None,
                passphrase: None,
                locked: true,
            }),
            "h".into(),
        );
        assert_eq!(saved.take("me@example.com's password: "), None);
    }

    #[test]
    fn every_way_of_opening_a_host_gets_the_same_login() {
        let hop = |id: &str| Target {
            host_id: id.into(),
            ..target()
        };
        let opened = with_saved_login(
            &source(Saved {
                password: Some("pw"),
                key: Some("PEM"),
                passphrase: None,
                locked: false,
            }),
            Target {
                jump: vec![hop("bastion"), hop("jump:me@elsewhere:22")],
                ..target()
            },
        )
        .unwrap();
        assert!(opened.identity.is_some() && opened.saved_password.is_some());
        // A hop that is a saved host brings its key; a bare address has none.
        assert!(opened.jump[0].identity.is_some());
        assert!(opened.jump[0].saved_password.is_some());
        assert!(opened.jump[1].identity.is_none());
        assert!(opened.jump[1].saved_password.is_none());
        // A locked vault with a saved key stops the connection before it starts.
        let error = with_saved_login(
            &source(Saved {
                password: None,
                key: Some("PEM"),
                passphrase: None,
                locked: true,
            }),
            target(),
        )
        .unwrap_err();
        assert_eq!(error.code, "vault_locked");
    }
}
