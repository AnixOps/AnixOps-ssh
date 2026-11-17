//! How the rest of the app reaches saved secrets: [`SecretSource`] for the
//! SSH engines, [`PromptAnswers`] for the askpass bridge.

use crate::prompt::{Binding, SecretKind};
use crate::vault::{Vault, VaultError};
use easyssh_crypto::SecretText;
use easyssh_store::{SecretChange, SecretsUpdate, Store, StoredSecrets};
use std::sync::{Arc, Mutex};

/// Saved secrets by host id.
///
/// The OpenSSH engine does not call this directly (the daemon answers
/// askpass prompts and writes key files itself). It is the seam for the
/// in-process `russh` engine: it asks for a host's password, key and
/// passphrase and keeps them in memory only, as [`SecretText`], which
/// zeroizes on drop.
pub trait SecretSource: Send + Sync {
    /// The host's saved secrets. `Err(VaultError::Locked)` when the user
    /// has to unlock the vault first; an empty value when nothing is saved.
    fn host_secrets(&self, host_id: &str) -> Result<StoredSecrets, VaultError>;
}

/// What the askpass bridge may do with the vault.
pub trait PromptAnswers: Send + Sync {
    /// The saved answer for a matched prompt of `binding`.
    fn answer(&self, binding: &Binding, kind: SecretKind) -> Answer;
    /// Saves `value` as the host's password or key passphrase.
    fn remember(
        &self,
        binding: &Binding,
        kind: SecretKind,
        value: SecretText,
    ) -> Result<(), VaultError>;
}

/// The result of looking for a saved answer.
#[derive(Debug)]
pub enum Answer {
    /// Nothing saved for this prompt.
    None,
    /// Something is saved but the vault is locked.
    Locked,
    Secret(SecretText),
}

/// Runs code against the app database; implemented by whoever owns a store.
pub trait StoreAccess: Send + Sync {
    /// Calls `f` with a store and returns true, or returns false if no store
    /// could be opened.
    fn with_store(&self, f: &mut dyn FnMut(&Store)) -> bool;
}

impl StoreAccess for Mutex<Store> {
    fn with_store(&self, f: &mut dyn FnMut(&Store)) -> bool {
        let store = self.lock().unwrap_or_else(|e| e.into_inner());
        f(&store);
        true
    }
}

fn with<T>(access: &dyn StoreAccess, f: impl FnOnce(&Store) -> T) -> Option<T> {
    let mut f = Some(f);
    let mut out = None;
    access.with_store(&mut |store| {
        if let Some(f) = f.take() {
            out = Some(f(store));
        }
    });
    out
}

/// The vault over an app database: the one object that is both a
/// [`SecretSource`] and [`PromptAnswers`].
pub struct VaultSecrets {
    vault: Arc<Vault>,
    store: Arc<dyn StoreAccess>,
}

impl VaultSecrets {
    pub fn new(vault: Arc<Vault>, store: Arc<dyn StoreAccess>) -> Self {
        Self { vault, store }
    }

    pub fn vault(&self) -> &Arc<Vault> {
        &self.vault
    }

    /// Runs `f` with the database; an error when none could be opened.
    pub fn with_store<T>(&self, f: impl FnOnce(&Store) -> T) -> Result<T, VaultError> {
        with(&*self.store, f).ok_or_else(|| VaultError::Store("the database is unavailable".into()))
    }
}

impl SecretSource for VaultSecrets {
    fn host_secrets(&self, host_id: &str) -> Result<StoredSecrets, VaultError> {
        self.with_store(|store| self.vault.host_secrets(store, host_id))?
    }
}

impl PromptAnswers for VaultSecrets {
    fn answer(&self, binding: &Binding, kind: SecretKind) -> Answer {
        let found = with(&*self.store, |store| {
            let flags = store.host_secret_flags(&binding.host_id).ok()?;
            let saved = match kind {
                SecretKind::Password => flags.password,
                SecretKind::KeyPassphrase => flags.passphrase,
            };
            if !saved {
                return Some(Answer::None);
            }
            Some(match self.vault.host_secrets(store, &binding.host_id) {
                Ok(secrets) => match match kind {
                    SecretKind::Password => secrets.password,
                    SecretKind::KeyPassphrase => secrets.passphrase,
                } {
                    Some(secret) => Answer::Secret(secret),
                    None => Answer::None,
                },
                Err(VaultError::Locked) => Answer::Locked,
                Err(_) => Answer::None,
            })
        });
        found.flatten().unwrap_or(Answer::None)
    }

    fn remember(
        &self,
        binding: &Binding,
        kind: SecretKind,
        value: SecretText,
    ) -> Result<(), VaultError> {
        let change = SecretChange::Set(value);
        let update = match kind {
            SecretKind::Password => SecretsUpdate {
                password: change,
                ..SecretsUpdate::default()
            },
            SecretKind::KeyPassphrase => SecretsUpdate {
                passphrase: change,
                ..SecretsUpdate::default()
            },
        };
        self.with_store(|store| {
            self.vault
                .set_host_secrets(store, &binding.host_id, &update)
        })?
    }
}
