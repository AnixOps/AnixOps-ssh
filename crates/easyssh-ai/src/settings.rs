//! Provider settings (plain, in the app database) and the API key (sealed by
//! the vault, in a separate settings slot per provider). The key is never
//! part of [`AiSettings`] and cannot be read back by the UI.

use crate::provider::{AiConfig, Provider};
use base64::{engine::general_purpose::STANDARD, Engine};
use easyssh_store::Store;
use easyssh_vault::{Vault, VaultError};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const MAX_KEY_LEN: usize = 512;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Vault(#[from] VaultError),
    #[error("database error: {0}")]
    Store(String),
}

impl SettingsError {
    /// A short stable code, the vault's where there is one.
    pub fn code(&self) -> &'static str {
        match self {
            SettingsError::Invalid(_) => "invalid_command",
            SettingsError::Vault(error) => error.code(),
            SettingsError::Store(_) => "store_error",
        }
    }
}

impl From<easyssh_store::StoreError> for SettingsError {
    fn from(error: easyssh_store::StoreError) -> Self {
        SettingsError::Store(error.to_string())
    }
}

/// What the settings form edits. Off until the user turns it on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSettings {
    pub enabled: bool,
    pub provider: Provider,
    /// Empty: the provider's default model.
    pub model: String,
    pub base_url: Option<String>,
}

impl Default for AiSettings {
    fn default() -> Self {
        AiSettings {
            enabled: false,
            provider: Provider::Anthropic,
            model: Provider::Anthropic.default_model().to_owned(),
            base_url: None,
        }
    }
}

impl AiSettings {
    /// The key in the app database's settings table.
    pub const KEY: &'static str = "ai";

    pub fn config(&self) -> AiConfig {
        let model = self.model.trim();
        AiConfig {
            provider: self.provider,
            model: if model.is_empty() {
                self.provider.default_model().to_owned()
            } else {
                model.to_owned()
            },
            base_url: self.base_url.clone().filter(|u| !u.trim().is_empty()),
        }
    }

    pub fn load(store: &Store) -> AiSettings {
        store
            .get_setting_as::<AiSettings>(Self::KEY)
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn save(&self, store: &Store) -> Result<(), SettingsError> {
        self.config().validate().map_err(SettingsError::Invalid)?;
        store.set_setting_as(Self::KEY, self)?;
        Ok(())
    }
}

fn slot(provider: Provider) -> String {
    format!("ai_key.{}", provider.id())
}

fn aad(provider: Provider) -> Vec<u8> {
    format!("easyssh/v1/ai-key/{}", provider.id()).into_bytes()
}

/// Whether a key is saved for `provider`. Needs no unlock.
pub fn has_key(store: &Store, provider: Provider) -> bool {
    store
        .get_setting_as::<String>(&slot(provider))
        .ok()
        .flatten()
        .is_some()
}

/// Saves `key` for `provider`, sealed by the vault; `None` removes it.
pub fn set_key(
    store: &Store,
    vault: &Vault,
    provider: Provider,
    key: Option<&str>,
) -> Result<(), SettingsError> {
    let Some(key) = key.map(str::trim).filter(|k| !k.is_empty()) else {
        store.delete_setting(&slot(provider))?;
        return Ok(());
    };
    if key.len() > MAX_KEY_LEN || !key.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(SettingsError::Invalid(
            "the API key must be one line of plain characters, without spaces".to_owned(),
        ));
    }
    let blob = vault.seal_value(&aad(provider), key.as_bytes())?;
    store.set_setting_as(&slot(provider), &STANDARD.encode(blob))?;
    Ok(())
}

/// The saved key, opened by the vault. `Ok(None)` when none is saved.
pub fn read_key(
    store: &Store,
    vault: &Vault,
    provider: Provider,
) -> Result<Option<Zeroizing<String>>, SettingsError> {
    let Some(encoded) = store.get_setting_as::<String>(&slot(provider))? else {
        return Ok(None);
    };
    let blob = STANDARD
        .decode(encoded)
        .map_err(|_| SettingsError::Invalid("the saved API key is damaged".to_owned()))?;
    let plain = vault.open_value(&aad(provider), &blob)?;
    let text = String::from_utf8(plain.to_vec())
        .map_err(|_| SettingsError::Invalid("the saved API key is damaged".to_owned()))?;
    Ok(Some(Zeroizing::new(text)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_crypto::{KdfParams, MemoryKeyStore};
    use std::sync::Arc;

    fn fixture() -> (Vault, Store) {
        let vault = Vault::with_kdf(
            Arc::new(MemoryKeyStore::new()),
            KdfParams::insecure_for_tests(),
        );
        let store = Store::open_in_memory("dev", vault.cipher()).unwrap();
        (vault, store)
    }

    #[test]
    fn off_by_default_with_the_anthropic_default_model() {
        let (_, store) = fixture();
        let s = AiSettings::load(&store);
        assert!(!s.enabled);
        assert_eq!(s.config().model, "claude-sonnet-5-5");
        assert_eq!(s.config().provider, Provider::Anthropic);
    }

    #[test]
    fn settings_round_trip_and_validate() {
        let (_, store) = fixture();
        let s = AiSettings {
            enabled: true,
            provider: Provider::Openai,
            model: "qwen".into(),
            base_url: Some("http://127.0.0.1:8000/v1".into()),
        };
        s.save(&store).unwrap();
        assert_eq!(AiSettings::load(&store), s);
        let bad = AiSettings {
            base_url: Some("http://example.com".into()),
            ..s.clone()
        };
        assert!(matches!(bad.save(&store), Err(SettingsError::Invalid(_))));
        assert_eq!(
            AiSettings::load(&store),
            s,
            "a rejected save changes nothing"
        );
        // An empty model means the provider's default.
        let blank = AiSettings {
            model: " ".into(),
            provider: Provider::Ollama,
            ..AiSettings::default()
        };
        assert_eq!(blank.config().model, "llama3.1");
    }

    #[test]
    fn the_key_is_sealed_never_plain_and_per_provider() {
        let (vault, store) = fixture();
        assert!(!has_key(&store, Provider::Anthropic));
        set_key(
            &store,
            &vault,
            Provider::Anthropic,
            Some("sk-ant-secret-123"),
        )
        .unwrap();
        assert!(has_key(&store, Provider::Anthropic));
        assert!(!has_key(&store, Provider::Openai));
        // What is in the settings table is a blob, not the key.
        let raw: String = store
            .get_setting_as(&slot(Provider::Anthropic))
            .unwrap()
            .unwrap();
        assert!(!raw.contains("secret"));
        let blob = STANDARD.decode(&raw).unwrap();
        assert!(!blob.windows(6).any(|w| w == b"secret"));
        // The settings blob has no key in it either.
        AiSettings::default().save(&store).unwrap();
        let plain = store
            .get_setting(AiSettings::KEY)
            .unwrap()
            .unwrap()
            .to_string();
        assert!(
            !plain.contains("secret") && !plain.contains("key"),
            "{plain}"
        );
        let key = read_key(&store, &vault, Provider::Anthropic)
            .unwrap()
            .unwrap();
        assert_eq!(&*key, "sk-ant-secret-123");
        // A blob moved to another provider's slot does not open.
        store.set_setting_as(&slot(Provider::Openai), &raw).unwrap();
        assert!(read_key(&store, &vault, Provider::Openai).is_err());
        set_key(&store, &vault, Provider::Anthropic, None).unwrap();
        assert!(read_key(&store, &vault, Provider::Anthropic)
            .unwrap()
            .is_none());
    }

    #[test]
    fn odd_keys_are_refused() {
        let (vault, store) = fixture();
        for bad in [
            "a b",
            "k\r\nX-Evil: 1",
            "naïve",
            &"k".repeat(MAX_KEY_LEN + 1),
        ] {
            assert!(
                matches!(
                    set_key(&store, &vault, Provider::Anthropic, Some(bad)),
                    Err(SettingsError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        assert!(!has_key(&store, Provider::Anthropic));
    }

    #[test]
    fn a_locked_vault_says_so() {
        let (vault, store) = fixture();
        set_key(&store, &vault, Provider::Anthropic, Some("k-12345")).unwrap();
        vault.set_passphrase("passphrase").unwrap();
        vault.lock();
        let error = read_key(&store, &vault, Provider::Anthropic).unwrap_err();
        assert_eq!(error.code(), "vault_locked");
        vault.unlock("passphrase").unwrap();
        assert!(read_key(&store, &vault, Provider::Anthropic)
            .unwrap()
            .is_some());
    }
}
