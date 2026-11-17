//! The `Store` handle and the machinery every entity shares: transactions,
//! secret sealing, the `bump` helper with history, and generic CRUD over the
//! [`Entity`] trait.

use crate::cipher::FieldCipher;
use crate::error::{Result, StoreError};
use crate::meta::{fmt_ts, parse_ts, FieldChange, HistoryEntry, Meta};
use crate::migrations;
use base64::Engine;
use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{Connection, Row, ToSql};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// The personal vault created by migration 1. Records live here until team
/// vaults exist.
pub const PERSONAL_VAULT_ID: &str = "00000000-0000-0000-0000-000000000001";

/// A stored item together with its sync metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct Record<T> {
    pub item: T,
    pub meta: Meta,
}

/// One field of an entity as the diff and history see it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Field {
    pub value: serde_json::Value,
    pub secret: bool,
}

impl Field {
    pub fn plain(value: impl serde::Serialize) -> Self {
        Self {
            value: serde_json::to_value(value).expect("field serializes"),
            secret: false,
        }
    }
    pub fn secret(value: impl serde::Serialize) -> Self {
        Self {
            secret: true,
            ..Self::plain(value)
        }
    }
}

pub(crate) type Fields = BTreeMap<&'static str, Field>;

/// A table-backed syncable entity.
pub(crate) trait Entity: Clone {
    const TABLE: &'static str;
    const NAME: &'static str;
    const ORDER: &'static str;
    fn id(&self) -> &str;
    fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(self.id())
            .map(|_| ())
            .map_err(|_| StoreError::Invalid(format!("{} id must be a UUID", Self::NAME)))
    }
    fn from_row(store: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self>;
    /// Every field that takes part in diffing, history and stamps.
    fn fields(&self) -> Fields;
    /// The entity's own columns (the shared sync columns are added for it).
    fn columns(&self, store: &Store, vault_id: &str) -> Result<Vec<(&'static str, Value)>>;
    /// Carries over what the caller may leave out (for example unchanged
    /// secrets) from the stored version.
    fn keep_from(&mut self, _old: &Self) {}
    /// Copies store-owned values (such as `updated_at`) into the item.
    fn sync_meta(&mut self, _meta: &Meta) {}
    /// Writes dependent rows (tags, hops) after the main row.
    fn after_write(&self, _store: &Store, _meta: &Meta) -> Result<()> {
        Ok(())
    }
}

pub struct Store {
    pub(crate) conn: Connection,
    pub(crate) cipher: Arc<dyn FieldCipher>,
    pub(crate) device_id: String,
    pub(crate) vault_id: String,
    depth: Cell<u32>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("device_id", &self.device_id)
            .field("vault_id", &self.vault_id)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Opens (creating if needed) the database at `path`, in WAL mode with
    /// foreign keys on and `synchronous=FULL`, and applies pending migrations.
    /// A database written by a newer build is refused untouched.
    pub fn open(
        path: impl AsRef<Path>,
        device_id: impl Into<String>,
        cipher: Arc<dyn FieldCipher>,
    ) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        Self::init(Connection::open(path)?, device_id.into(), cipher)
    }

    /// A throwaway in-memory database (tests, previews).
    pub fn open_in_memory(
        device_id: impl Into<String>,
        cipher: Arc<dyn FieldCipher>,
    ) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, device_id.into(), cipher)
    }

    fn init(conn: Connection, device_id: String, cipher: Arc<dyn FieldCipher>) -> Result<Self> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Check before anything below writes to the file.
        retry_busy(|| migrations::refuse_newer(&conn))?;
        // Switching to WAL needs the file to itself for a moment, and SQLite
        // does not always call the busy handler for it (several processes
        // opening a new database together, notably on Windows), so retry.
        retry_busy(|| {
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))?;
            }
            Ok(())
        })?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        retry_busy(|| migrations::apply(&conn))?;
        Ok(Self {
            conn,
            cipher,
            device_id,
            vault_id: PERSONAL_VAULT_ID.to_owned(),
            depth: Cell::new(0),
        })
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    pub fn schema_version(&self) -> Result<i64> {
        migrations::current_version(&self.conn)
    }

    /// Runs `f` in one `BEGIN IMMEDIATE` transaction. Nested calls join the
    /// outer transaction; any error rolls everything back.
    pub(crate) fn in_tx<T>(&self, f: impl FnOnce() -> Result<T>) -> Result<T> {
        if self.depth.get() > 0 {
            return f();
        }
        self.conn.execute_batch("BEGIN IMMEDIATE")?;
        self.depth.set(1);
        let result = f();
        self.depth.set(0);
        match result {
            Ok(value) => match self.conn.execute_batch("COMMIT") {
                Ok(()) => Ok(value),
                Err(error) => {
                    let _ = self.conn.execute_batch("ROLLBACK");
                    Err(error.into())
                }
            },
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    // ---- secrets -------------------------------------------------------

    fn aad(table: &str, vault_id: &str, id: &str, field: &str) -> Vec<u8> {
        format!("easyssh/v1/{table}/{vault_id}/{id}/{field}").into_bytes()
    }

    pub(crate) fn seal(
        &self,
        table: &str,
        vault_id: &str,
        id: &str,
        field: &str,
        plaintext: &str,
    ) -> Result<Vec<u8>> {
        let aad = Self::aad(table, vault_id, id, field);
        Ok(self.cipher.encrypt(plaintext.as_bytes(), &aad)?)
    }

    pub(crate) fn open_sealed(
        &self,
        table: &str,
        vault_id: &str,
        id: &str,
        field: &str,
        ciphertext: &[u8],
    ) -> Result<String> {
        let aad = Self::aad(table, vault_id, id, field);
        String::from_utf8(self.cipher.decrypt(ciphertext, &aad)?)
            .map_err(|_| StoreError::Invalid(format!("secret field {field} is not UTF-8")))
    }

    // ---- bump and history ----------------------------------------------

    /// Stamps a write on `meta`: updates the version vector and the stamps of
    /// `changes`, and writes the overwritten values to `record_history`
    /// (secret values encrypted). The caller persists `meta`.
    pub fn bump(
        &self,
        table: &str,
        meta: &mut Meta,
        changes: &[FieldChange<'_>],
        now: DateTime<Utc>,
    ) -> Result<()> {
        let names: Vec<&str> = changes.iter().map(|change| change.field).collect();
        meta.bump(&names, &self.device_id, now);
        for change in changes {
            let Some(old) = &change.old else { continue };
            let text = serde_json::to_string(old)?;
            let (blob, encrypted) = if change.secret && !old.is_null() {
                let aad = Self::aad("record_history", table, &meta.id, change.field);
                (self.cipher.encrypt(text.as_bytes(), &aad)?, 1)
            } else {
                (text.into_bytes(), 0)
            };
            self.conn.execute(
                "INSERT INTO record_history (record_table, record_id, field, old_value, encrypted, changed_at, device)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    table,
                    meta.id,
                    change.field,
                    blob,
                    encrypted,
                    fmt_ts(now),
                    self.device_id
                ],
            )?;
        }
        Ok(())
    }

    /// Overwritten values of a record, newest first.
    pub fn history(&self, record_id: &str) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT seq, record_table, field, old_value, encrypted, changed_at, device
             FROM record_history WHERE record_id = ?1 ORDER BY seq DESC",
        )?;
        let mut rows = stmt.query([record_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let table: String = row.get(1)?;
            let field: String = row.get(2)?;
            let blob: Option<Vec<u8>> = row.get(3)?;
            let encrypted = row.get::<_, i64>(4)? != 0;
            let old_value = match blob {
                None => None,
                Some(bytes) => {
                    let bytes = if encrypted {
                        let aad = Self::aad("record_history", &table, record_id, &field);
                        self.cipher.decrypt(&bytes, &aad)?
                    } else {
                        bytes
                    };
                    Some(serde_json::from_slice(&bytes)?)
                }
            };
            out.push(HistoryEntry {
                seq: row.get(0)?,
                record_table: table,
                record_id: record_id.to_owned(),
                field,
                old_value,
                changed_at: parse_ts(&row.get::<_, String>(5)?)?,
                device: row.get(6)?,
            });
        }
        Ok(out)
    }

    /// Diffs `new` against the stored fields, bumps the record and writes
    /// history. Returns the new meta and whether anything changed.
    fn stamp(
        &self,
        table: &str,
        id: &str,
        vault_id: &str,
        old: Option<(Meta, Fields)>,
        new: &Fields,
        now: DateTime<Utc>,
    ) -> Result<(Meta, bool)> {
        let Some((mut meta, old_fields)) = old else {
            let mut meta = Meta::new(id, vault_id, now);
            let names: Vec<&str> = new.keys().copied().collect();
            meta.bump(&names, &self.device_id, now);
            return Ok((meta, true));
        };
        let changes: Vec<FieldChange<'_>> = new
            .iter()
            .filter_map(|(name, field)| {
                let old = old_fields.get(name);
                (old.map(|old| &old.value) != Some(&field.value)).then(|| FieldChange {
                    field: name,
                    old: old.map(|old| old.value.clone()),
                    secret: field.secret || old.is_some_and(|old| old.secret),
                })
            })
            .collect();
        if changes.is_empty() {
            return Ok((meta, false));
        }
        self.bump(table, &mut meta, &changes, now)?;
        Ok((meta, true))
    }

    // ---- generic CRUD --------------------------------------------------

    pub(crate) fn select<T: Entity>(
        &self,
        filter: &str,
        params: &[&dyn ToSql],
    ) -> Result<Vec<Record<T>>> {
        let sql = format!(
            "SELECT * FROM {} WHERE {filter} ORDER BY {}",
            T::TABLE,
            T::ORDER
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params)?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let meta = Meta::from_row(row)?;
            let mut item = T::from_row(self, row, &meta)?;
            item.sync_meta(&meta);
            out.push(Record { item, meta });
        }
        Ok(out)
    }

    pub(crate) fn list<T: Entity>(&self, deleted: bool) -> Result<Vec<Record<T>>> {
        self.select("deleted = ?1", &[&i64::from(deleted)])
    }

    /// A record whether or not it is deleted.
    pub(crate) fn get_any<T: Entity>(&self, id: &str) -> Result<Option<Record<T>>> {
        Ok(self.select::<T>("id = ?1", &[&id])?.into_iter().next())
    }

    pub(crate) fn get<T: Entity>(&self, id: &str) -> Result<Option<Record<T>>> {
        Ok(self.get_any::<T>(id)?.filter(|record| !record.meta.deleted))
    }

    pub(crate) fn upsert<T: Entity>(&self, item: &T) -> Result<Record<T>> {
        self.upsert_at(item, Utc::now())
    }

    pub(crate) fn upsert_at<T: Entity>(&self, item: &T, now: DateTime<Utc>) -> Result<Record<T>> {
        item.validate()?;
        self.in_tx(|| {
            let old = self.get_any::<T>(item.id())?;
            if old.as_ref().is_some_and(|old| old.meta.deleted) {
                return Err(StoreError::Deleted {
                    entity: T::NAME,
                    id: item.id().to_owned(),
                });
            }
            let mut item = item.clone();
            if let Some(old) = &old {
                item.keep_from(&old.item);
            }
            let vault_id = old
                .as_ref()
                .map_or_else(|| self.vault_id.clone(), |old| old.meta.vault_id.clone());
            let new_fields = item.fields();
            let (meta, changed) = self.stamp(
                T::TABLE,
                item.id(),
                &vault_id,
                old.as_ref()
                    .map(|old| (old.meta.clone(), old.item.fields())),
                &new_fields,
                now,
            )?;
            if !changed {
                return Ok(old.expect("unchanged implies existing"));
            }
            let mut columns = item.columns(self, &vault_id)?;
            columns.push(("id", Value::Text(meta.id.clone())));
            columns.push(("vault_id", Value::Text(meta.vault_id.clone())));
            columns.push(("updated_at", Value::Text(fmt_ts(meta.updated_at))));
            columns.push((
                "version_vector",
                Value::Text(serde_json::to_string(&meta.version_vector)?),
            ));
            columns.push((
                "field_stamps",
                Value::Text(serde_json::to_string(&meta.field_stamps)?),
            ));
            columns.push(("deleted", Value::Integer(i64::from(meta.deleted))));
            self.write_row(T::TABLE, columns)?;
            item.after_write(self, &meta)?;
            item.sync_meta(&meta);
            Ok(Record { item, meta })
        })
    }

    /// `INSERT ... ON CONFLICT(id) DO UPDATE` over a dynamic column list.
    pub(crate) fn write_row(&self, table: &str, columns: Vec<(&'static str, Value)>) -> Result<()> {
        let names: Vec<&str> = columns.iter().map(|(name, _)| *name).collect();
        let placeholders: Vec<String> = (1..=names.len()).map(|n| format!("?{n}")).collect();
        let updates: Vec<String> = names
            .iter()
            .filter(|name| **name != "id")
            .map(|name| format!("{name} = excluded.{name}"))
            .collect();
        let sql = format!(
            "INSERT INTO {table} ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
            names.join(", "),
            placeholders.join(", "),
            updates.join(", ")
        );
        self.conn.execute(
            &sql,
            rusqlite::params_from_iter(columns.into_iter().map(|c| c.1)),
        )?;
        Ok(())
    }

    /// Soft-deletes or restores. Setting the state a record already has is a
    /// no-op. Returns whether the record exists.
    pub(crate) fn set_deleted<T: Entity>(&self, id: &str, deleted: bool) -> Result<bool> {
        self.in_tx(|| {
            let Some(record) = self.get_any::<T>(id)? else {
                return Ok(false);
            };
            if record.meta.deleted == deleted {
                return Ok(true);
            }
            let mut meta = record.meta;
            let change = FieldChange {
                field: "deleted",
                old: Some(serde_json::Value::Bool(meta.deleted)),
                secret: false,
            };
            self.bump(T::TABLE, &mut meta, &[change], Utc::now())?;
            self.conn.execute(
                &format!(
                    "UPDATE {} SET deleted = ?1, updated_at = ?2, version_vector = ?3, field_stamps = ?4 WHERE id = ?5",
                    T::TABLE
                ),
                rusqlite::params![
                    i64::from(deleted),
                    fmt_ts(meta.updated_at),
                    serde_json::to_string(&meta.version_vector)?,
                    serde_json::to_string(&meta.field_stamps)?,
                    id
                ],
            )?;
            Ok(true)
        })
    }

    pub(crate) fn exists_any<T: Entity>(&self, id: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            &format!("SELECT EXISTS (SELECT 1 FROM {} WHERE id = ?1)", T::TABLE),
            [id],
            |row| row.get::<_, i64>(0),
        )? != 0)
    }
}

// ---- value helpers for `columns` ----------------------------------------

pub(crate) fn text(value: impl Into<String>) -> Value {
    Value::Text(value.into())
}

pub(crate) fn opt_text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |value| Value::Text(value.to_owned()))
}

pub(crate) fn int(value: impl Into<i64>) -> Value {
    Value::Integer(value.into())
}

pub(crate) fn opt_int(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

pub(crate) fn json<T: serde::Serialize>(value: &T) -> Result<Value> {
    Ok(Value::Text(serde_json::to_string(value)?))
}

pub(crate) fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---- settings -----------------------------------------------------------

impl Store {
    /// A setting's JSON value, or `None` when unset or deleted.
    pub fn get_setting(&self, key: &str) -> Result<Option<serde_json::Value>> {
        Ok(self
            .setting_row(key)?
            .filter(|(_, meta)| !meta.deleted)
            .map(|(value, _)| value))
    }

    pub fn get_setting_as<T: serde::de::DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        self.get_setting(key)?
            .map(serde_json::from_value)
            .transpose()
            .map_err(Into::into)
    }

    pub(crate) fn setting_exists_any(&self, key: &str) -> Result<bool> {
        Ok(self.setting_row(key)?.is_some())
    }

    fn setting_row(&self, key: &str) -> Result<Option<(serde_json::Value, Meta)>> {
        let mut stmt = self.conn.prepare(
            "SELECT key AS id, '' AS vault_id, value, updated_at, version_vector, field_stamps, deleted
             FROM settings WHERE key = ?1",
        )?;
        let mut rows = stmt.query([key])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        let meta = Meta::from_row(row)?;
        let value = serde_json::from_str(&row.get::<_, String>("value")?)?;
        Ok(Some((value, meta)))
    }

    /// Sets a setting, stamping the write and keeping the old value in history.
    pub fn set_setting(&self, key: &str, value: &serde_json::Value) -> Result<()> {
        if key.is_empty() {
            return Err(StoreError::Invalid("setting key is empty".into()));
        }
        self.in_tx(|| {
            let now = Utc::now();
            let (mut meta, old) = match self.setting_row(key)? {
                Some((old_value, meta)) => {
                    if !meta.deleted && &old_value == value {
                        return Ok(());
                    }
                    (meta, Some(old_value))
                }
                None => (Meta::new(key, "", now), None),
            };
            let change = FieldChange {
                field: "value",
                old,
                secret: false,
            };
            meta.deleted = false;
            self.bump("settings", &mut meta, &[change], now)?;
            self.conn.execute(
                "INSERT INTO settings (key, value, updated_at, version_vector, field_stamps, deleted)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at,
                   version_vector = excluded.version_vector, field_stamps = excluded.field_stamps, deleted = 0",
                rusqlite::params![
                    key,
                    serde_json::to_string(value)?,
                    fmt_ts(meta.updated_at),
                    serde_json::to_string(&meta.version_vector)?,
                    serde_json::to_string(&meta.field_stamps)?
                ],
            )?;
            Ok(())
        })
    }

    pub fn set_setting_as<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.set_setting(key, &serde_json::to_value(value)?)
    }

    /// Tombstones a setting. Returns whether it existed.
    pub fn delete_setting(&self, key: &str) -> Result<bool> {
        self.in_tx(|| {
            let Some((old, mut meta)) = self.setting_row(key)? else {
                return Ok(false);
            };
            if meta.deleted {
                return Ok(true);
            }
            let change = FieldChange {
                field: "deleted",
                old: Some(serde_json::Value::Bool(false)),
                secret: false,
            };
            let _ = old;
            self.bump("settings", &mut meta, &[change], Utc::now())?;
            self.conn.execute(
                "UPDATE settings SET deleted = 1, updated_at = ?1, version_vector = ?2, field_stamps = ?3 WHERE key = ?4",
                rusqlite::params![
                    fmt_ts(meta.updated_at),
                    serde_json::to_string(&meta.version_vector)?,
                    serde_json::to_string(&meta.field_stamps)?,
                    key
                ],
            )?;
            Ok(true)
        })
    }

    /// All live settings, ordered by key.
    pub fn list_settings(&self) -> Result<BTreeMap<String, serde_json::Value>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM settings WHERE deleted = 0 ORDER BY key")?;
        let mut rows = stmt.query([])?;
        let mut out = BTreeMap::new();
        while let Some(row) = rows.next()? {
            out.insert(
                row.get::<_, String>(0)?,
                serde_json::from_str(&row.get::<_, String>(1)?)?,
            );
        }
        Ok(out)
    }
}

// ---- vaults (placeholder) -----------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultKind {
    Personal,
    Team,
}

impl VaultKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Team => "team",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vault {
    pub id: String,
    pub name: String,
    pub kind: VaultKind,
    pub created_at: DateTime<Utc>,
}

impl Store {
    pub fn list_vaults(&self) -> Result<Vec<Vault>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, kind, created_at FROM vaults ORDER BY created_at, id")?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Vault {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: match row.get::<_, String>(2)?.as_str() {
                    "team" => VaultKind::Team,
                    _ => VaultKind::Personal,
                },
                created_at: parse_ts(&row.get::<_, String>(3)?)?,
            });
        }
        Ok(out)
    }

    /// Placeholder until team vaults exist: records a vault row only.
    pub fn create_vault(&self, name: &str, kind: VaultKind) -> Result<Vault> {
        let vault = Vault {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
            kind,
            created_at: Utc::now(),
        };
        self.conn.execute(
            "INSERT INTO vaults (id, name, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                vault.id,
                vault.name,
                kind.as_str(),
                fmt_ts(vault.created_at)
            ],
        )?;
        Ok(vault)
    }

    /// Records created from now on go into this vault.
    pub fn set_active_vault(&mut self, vault_id: &str) -> Result<()> {
        if !self.list_vaults()?.iter().any(|vault| vault.id == vault_id) {
            return Err(StoreError::NotFound {
                entity: "vault",
                id: vault_id.to_owned(),
            });
        }
        self.vault_id = vault_id.to_owned();
        Ok(())
    }
}

/// Runs `step` again for up to 15 seconds while another process holds the
/// database ("database is locked" that the busy timeout did not wait out).
fn retry_busy<T>(mut step: impl FnMut() -> Result<T>) -> Result<T> {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        match step() {
            Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(error, _)))
                if matches!(
                    error.code,
                    rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
                ) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            other => return other,
        }
    }
}
