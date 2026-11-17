use crate::cipher::CipherError;
use thiserror::Error;

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error(
        "the database uses schema version {found}, but this build only understands up to \
         {supported}; refusing to open it"
    )]
    DatabaseTooNew { found: i64, supported: i64 },
    #[error(transparent)]
    Cipher(#[from] CipherError),
    #[error("{entity} {id} was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("{entity} {id} is deleted; restore it before changing it")]
    Deleted { entity: &'static str, id: String },
    #[error("invalid value: {0}")]
    Invalid(String),
    #[error("v1 configuration could not be read: {0}")]
    V1Config(#[from] easyssh_core::config::ConfigError),
}
