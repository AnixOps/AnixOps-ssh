//! What the dialog remembers about a service: the plain form values in the
//! app database, and the API credential sealed by the vault in a settings slot
//! of its own. The credential is never part of [`ServiceForm`] and the UI
//! cannot read it back.

use crate::error::{Result, ServiceError};
use base64::{engine::general_purpose::STANDARD, Engine};
use easyssh_store::Store;
use easyssh_vault::Vault;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// The services hosts can be imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    /// AnixOps Control: the admin API's node list.
    Control,
    /// JumpServer: the assets API, optionally through its KoKo SSH gateway.
    JumpServer,
}

impl Service {
    pub fn id(self) -> &'static str {
        match self {
            Service::Control => "control",
            Service::JumpServer => "jumpserver",
        }
    }

    pub fn from_id(id: &str) -> Option<Service> {
        [Service::Control, Service::JumpServer]
            .into_iter()
            .find(|service| service.id() == id)
    }

    fn form_key(self) -> String {
        format!("service.{}", self.id())
    }

    fn secret_slot(self) -> String {
        format!("service.{}.secret", self.id())
    }

    fn aad(self) -> Vec<u8> {
        format!("easyssh/v1/service-secret/{}", self.id()).into_bytes()
    }
}

const MAX_SECRET_LEN: usize = 512;

/// The plain settings of a service import. Fields a service does not use stay
/// empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceForm {
    /// Address of the service, `https://panel.example.com`.
    pub url: String,
    /// SSH user for the imported hosts. Empty: none (ssh's own default).
    pub default_user: String,
    /// Key file the hosts use; noted on the host, never read.
    pub identity_file: Option<String>,
    /// JumpServer: the access key id (not secret; the secret is sealed).
    pub access_key_id: String,
    /// JumpServer: organization id; empty means the default organization.
    pub org_id: String,
    /// JumpServer: connect through the KoKo SSH gateway.
    pub gateway: bool,
    /// JumpServer: gateway host; empty means the service's own host.
    pub gateway_host: String,
    /// JumpServer: gateway SSH port.
    pub gateway_port: u16,
    /// JumpServer: the JumpServer user name that logs in to the gateway.
    pub gateway_user: String,
}

impl Default for ServiceForm {
    fn default() -> Self {
        Self {
            url: String::new(),
            default_user: "root".to_owned(),
            identity_file: None,
            access_key_id: String::new(),
            org_id: String::new(),
            gateway: true,
            gateway_host: String::new(),
            gateway_port: 2222,
            gateway_user: String::new(),
        }
    }
}

/// What the dialog opens with: the form and whether a credential is saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Remembered {
    pub form: ServiceForm,
    pub has_secret: bool,
}

pub fn remembered(store: &Store, service: Service) -> Remembered {
    Remembered {
        form: store
            .get_setting_as::<ServiceForm>(&service.form_key())
            .ok()
            .flatten()
            .unwrap_or_default(),
        has_secret: has_secret(store, service),
    }
}

pub fn save_form(store: &Store, service: Service, form: &ServiceForm) -> Result<()> {
    store.set_setting_as(&service.form_key(), form)?;
    Ok(())
}

/// Whether a credential is saved. Needs no unlock.
pub fn has_secret(store: &Store, service: Service) -> bool {
    store
        .get_setting_as::<String>(&service.secret_slot())
        .ok()
        .flatten()
        .is_some()
}

/// Saves `secret` sealed by the vault; `None` removes it.
pub fn set_secret(
    store: &Store,
    vault: &Vault,
    service: Service,
    secret: Option<&str>,
) -> Result<()> {
    let Some(secret) = secret.map(str::trim).filter(|s| !s.is_empty()) else {
        store.delete_setting(&service.secret_slot())?;
        return Ok(());
    };
    check_secret(secret)?;
    let blob = vault
        .seal_value(&service.aad(), secret.as_bytes())
        .map_err(|error| ServiceError::Secret(error.to_string()))?;
    store.set_setting_as(&service.secret_slot(), &STANDARD.encode(blob))?;
    Ok(())
}

/// The saved credential, opened by the vault. `Ok(None)` when none is saved.
pub fn read_secret(
    store: &Store,
    vault: &Vault,
    service: Service,
) -> Result<Option<Zeroizing<String>>> {
    let Some(encoded) = store.get_setting_as::<String>(&service.secret_slot())? else {
        return Ok(None);
    };
    let damaged = || ServiceError::Secret("The saved credential is damaged.".to_owned());
    let blob = STANDARD.decode(encoded).map_err(|_| damaged())?;
    let plain = vault
        .open_value(&service.aad(), &blob)
        .map_err(|error| ServiceError::Secret(error.to_string()))?;
    let text = String::from_utf8(plain.to_vec()).map_err(|_| damaged())?;
    Ok(Some(Zeroizing::new(text)))
}

/// One line of printable ASCII: it goes into an HTTP header, so nothing else
/// is accepted.
pub fn check_secret(secret: &str) -> Result<()> {
    if secret.is_empty()
        || secret.len() > MAX_SECRET_LEN
        || !secret.bytes().all(|b| (0x21..=0x7e).contains(&b))
    {
        return Err(ServiceError::Invalid(
            "The credential must be one line of plain characters, without spaces.".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_crypto::{KdfParams, MemoryKeyStore};
    use std::sync::Arc;

    pub(crate) fn fixture() -> (Vault, Store) {
        let vault = Vault::with_kdf(
            Arc::new(MemoryKeyStore::new()),
            KdfParams::insecure_for_tests(),
        );
        let store = Store::open_in_memory("dev", vault.cipher()).unwrap();
        (vault, store)
    }

    #[test]
    fn the_secret_is_sealed_and_never_in_the_form() {
        let (vault, store) = fixture();
        assert!(!has_secret(&store, Service::Control));
        set_secret(&store, &vault, Service::Control, Some("anixadm_topsecret")).unwrap();
        assert!(has_secret(&store, Service::Control));
        assert!(
            !has_secret(&store, Service::JumpServer),
            "one slot per service"
        );
        let raw: String = store
            .get_setting_as(&Service::Control.secret_slot())
            .unwrap()
            .unwrap();
        assert!(!raw.contains("topsecret"));
        assert!(!STANDARD
            .decode(&raw)
            .unwrap()
            .windows(9)
            .any(|w| w == b"topsecret"));
        // The plain form never holds it.
        save_form(&store, Service::Control, &ServiceForm::default()).unwrap();
        let plain = store
            .get_setting(&Service::Control.form_key())
            .unwrap()
            .unwrap()
            .to_string();
        assert!(!plain.contains("topsecret"));
        assert_eq!(
            &*read_secret(&store, &vault, Service::Control)
                .unwrap()
                .unwrap(),
            "anixadm_topsecret"
        );
        // A blob moved to another slot does not open (the slot is in its AAD).
        let other = vault.seal_value(b"another slot", b"anixadm_x").unwrap();
        store
            .set_setting_as(&Service::Control.secret_slot(), &STANDARD.encode(other))
            .unwrap();
        assert!(read_secret(&store, &vault, Service::Control).is_err());
        store
            .set_setting_as(&Service::Control.secret_slot(), &raw)
            .unwrap();
        set_secret(&store, &vault, Service::Control, None).unwrap();
        assert!(read_secret(&store, &vault, Service::Control)
            .unwrap()
            .is_none());
    }

    #[test]
    fn odd_secrets_are_refused_and_the_form_round_trips() {
        let (vault, store) = fixture();
        for bad in [
            "a b",
            "k\r\nX-Evil: 1",
            "naïve",
            &"k".repeat(MAX_SECRET_LEN + 1),
        ] {
            assert!(set_secret(&store, &vault, Service::Control, Some(bad)).is_err());
        }
        assert!(!has_secret(&store, Service::Control));
        assert_eq!(
            remembered(&store, Service::Control).form.default_user,
            "root"
        );
        let form = ServiceForm {
            url: "https://panel.example.com".into(),
            default_user: "ops".into(),
            ..ServiceForm::default()
        };
        save_form(&store, Service::Control, &form).unwrap();
        assert_eq!(remembered(&store, Service::Control).form, form);
    }
}
