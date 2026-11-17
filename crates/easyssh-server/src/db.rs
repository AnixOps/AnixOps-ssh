//! SQLite storage. Plaintext routing metadata and ciphertext only.

use crate::error::ApiError;
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value BLOB NOT NULL
);
CREATE TABLE IF NOT EXISTS accounts (
    id               TEXT PRIMARY KEY,
    username         TEXT NOT NULL UNIQUE,
    opaque_record    BLOB NOT NULL,
    kdf_header       BLOB NOT NULL,
    wrapped_keys     BLOB NOT NULL,
    seal_public      BLOB NOT NULL,
    signing_public   BLOB NOT NULL,
    created_at       INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
    id          TEXT PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    name        TEXT NOT NULL,
    public      BLOB NOT NULL,
    cert        BLOB NOT NULL,
    created_at  INTEGER NOT NULL,
    revoked_at  INTEGER
);
CREATE INDEX IF NOT EXISTS devices_by_account ON devices(account_id);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash  BLOB PRIMARY KEY,
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    device_id   TEXT,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_by_device ON sessions(device_id);
CREATE TABLE IF NOT EXISTS vaults (
    id              TEXT PRIMARY KEY,
    created_by      TEXT NOT NULL REFERENCES accounts(id),
    created_at      INTEGER NOT NULL,
    key_generation  INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS vault_members (
    vault_id    TEXT NOT NULL REFERENCES vaults(id),
    account_id  TEXT NOT NULL REFERENCES accounts(id),
    role        TEXT NOT NULL CHECK (role IN ('owner','editor','viewer')),
    sealed_key  BLOB NOT NULL,
    PRIMARY KEY (vault_id, account_id)
);
CREATE INDEX IF NOT EXISTS members_by_account ON vault_members(account_id);
CREATE TABLE IF NOT EXISTS records (
    vault_id        TEXT NOT NULL REFERENCES vaults(id),
    record_id       TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    tbl             TEXT NOT NULL,
    key_generation  INTEGER NOT NULL,
    version_vector  TEXT NOT NULL,
    deleted         INTEGER NOT NULL,
    ciphertext      BLOB NOT NULL,
    signer          BLOB NOT NULL,
    signature       BLOB NOT NULL,
    cert            BLOB NOT NULL,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (vault_id, record_id)
);
CREATE INDEX IF NOT EXISTS records_by_seq ON records(vault_id, seq);
CREATE TABLE IF NOT EXISTS record_history (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    vault_id        TEXT NOT NULL,
    record_id       TEXT NOT NULL,
    seq             INTEGER NOT NULL,
    tbl             TEXT NOT NULL,
    key_generation  INTEGER NOT NULL,
    version_vector  TEXT NOT NULL,
    deleted         INTEGER NOT NULL,
    ciphertext      BLOB NOT NULL,
    signer          BLOB NOT NULL,
    signature       BLOB NOT NULL,
    cert            BLOB NOT NULL,
    updated_at      INTEGER NOT NULL,
    replaced_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS history_by_record ON record_history(vault_id, record_id, id);
";

/// Shared connection; every call runs on the blocking pool.
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run `f` on the blocking pool with exclusive access to the connection.
    pub async fn call<R, F>(&self, f: F) -> Result<R, ApiError>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<R, ApiError> + Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut guard)
        })
        .await
        .map_err(|_| ApiError::internal("storage task failed"))?
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A stored secret blob in `meta`, created on first use.
pub fn meta_blob(
    conn: &Connection,
    key: &str,
    create: impl FnOnce() -> Vec<u8>,
) -> Result<Vec<u8>, rusqlite::Error> {
    use rusqlite::OptionalExtension;
    if let Some(v) = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?
    {
        return Ok(v);
    }
    let value = create();
    conn.execute(
        "INSERT INTO meta(key, value) VALUES (?1, ?2)",
        rusqlite::params![key, value],
    )?;
    Ok(value)
}
