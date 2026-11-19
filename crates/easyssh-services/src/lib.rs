//! Import hosts from services the user already runs.
//!
//! AnixOps Control (`control`) lists its nodes through the admin API;
//! JumpServer (`jumpserver`) lists its assets. Each importer fetches over
//! HTTPS with the user's API credential, maps what it finds to
//! [`easyssh_store::import::ImportHost`] and stops there: the preview with
//! "already stored" marks and the merge are the store's
//! (`import_preview_hosts`, `import_apply`), the same ones a file import
//! uses. Every host carries a stable source key, so importing again updates
//! the hosts it created instead of adding more.
//!
//! The credential is sealed by the vault ([`settings`]), sent only in a
//! request header, and never part of a URL, a log line or an error message.

pub mod control;
mod error;
pub mod http;
pub mod settings;

pub use error::{Result, ServiceError};
pub use settings::{Remembered, Service, ServiceForm};

use easyssh_store::import::{import_preview_hosts, ImportHost, ImportPreview};
use easyssh_store::Store;

/// What a service import found, before the store has marked duplicates.
#[derive(Debug, Clone, Default)]
pub struct Fetched {
    pub hosts: Vec<ImportHost>,
    pub warnings: Vec<String>,
    /// Records the service listed that were not imported.
    pub skipped: usize,
    /// Prefix of the source keys of this service instance.
    pub scope: String,
}

impl Fetched {
    /// Marks the hosts that are stored already and the ones the service no
    /// longer lists. Writes nothing.
    pub fn preview(self, store: &Store) -> easyssh_store::Result<ImportPreview> {
        import_preview_hosts(store, self.hosts, &self.scope, self.warnings, self.skipped)
    }
}

/// The credential to use: the typed one, else the saved one.
pub fn credential(
    store: &Store,
    vault: &easyssh_vault::Vault,
    service: Service,
    typed: Option<&str>,
) -> Result<zeroize::Zeroizing<String>> {
    if let Some(typed) = typed.map(str::trim).filter(|typed| !typed.is_empty()) {
        return Ok(zeroize::Zeroizing::new(typed.to_owned()));
    }
    settings::read_secret(store, vault, service)?.ok_or_else(|| {
        ServiceError::Invalid(match service {
            Service::Control => "Enter an admin API token.".to_owned(),
        })
    })
}

/// Checks the address and the credential; returns how many hosts the service
/// lists.
pub fn test(service: Service, form: &ServiceForm, credential: &str) -> Result<usize> {
    match service {
        Service::Control => control::test(form, credential),
    }
}

/// Reads the hosts of the service.
pub fn fetch(service: Service, form: &ServiceForm, credential: &str) -> Result<Fetched> {
    match service {
        Service::Control => control::fetch(form, credential),
    }
}
