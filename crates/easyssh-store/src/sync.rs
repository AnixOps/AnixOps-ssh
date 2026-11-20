//! Sync over the local store: [`StoreSync`] implements `easyssh-sync`'s
//! [`SyncStore`] on top of the tables, so the sync engine merges server
//! changes into the same rows the app edits.
//!
//! * Synced tables: `groups`, `hosts`, `snippets`, `tunnels`, `known_hosts`,
//!   plus the name of a team vault (record `vault-meta`, table `vault_meta`).
//! * Never synced: `host_secrets` (passwords, keys, passphrases), a host's
//!   secret notes, settings, jump chains. Secret fields are those an entity
//!   marks `secret`; they are left out of every record.
//! * A record is the entity's non-secret fields, each with the stamp the
//!   store already keeps (`field_stamps`), the row's version vector and a
//!   tombstone (the stamp of the newest delete). Merging happens in
//!   `easyssh-sync`; this module only maps rows to records and back.
//! * Values a merge overwrites go to `record_history` next to the values that
//!   local edits overwrite, so one panel restores both.
//! * The pull cursor, the compare-and-set token of every record and the
//!   rollback high-water mark are stored here, so they survive restarts.
//!
//! The personal vault has the same id on every install (`PERSONAL_VAULT_ID`),
//! but on a server it is a per-account vault; [`StoreSync`] translates.

use crate::entities::Host;
use crate::error::{Result, StoreError};
use crate::meta::{fmt_ts, FieldStamp, Meta};
use crate::store::{Entity, Store, PERSONAL_VAULT_ID};
use base64::Engine;
use chrono::Utc;
use easyssh_core::{CommandSnippet, Connection, Group, Tunnel, TunnelKind};
use easyssh_crypto::VersionVector;
use easyssh_sync::verify::{HighWater, HighWaterSnapshot};
use easyssh_sync::{LocalRecord, Overwritten, RecordVersion, Stamp, SyncStore};
use rusqlite::{params, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

/// Table name of the record that carries a team vault's name.
pub const VAULT_META_TABLE: &str = "vault_meta";
/// Record id of that record (ids are scoped to a vault).
pub const VAULT_META_RECORD: &str = "vault-meta";
/// The pseudo field a delete or restore is stamped with in `field_stamps`.
const DELETED: &str = "deleted";
/// `FieldCipher` associated data of the sealed account secrets.
const ACCOUNT_AAD: &[u8] = b"easyssh/v1/sync_account";
/// Order matters: referenced rows (groups) come before the rows that point at them.
const ENTITY_TABLES: [&str; 5] = ["groups", "hosts", "snippets", "tunnels", "known_hosts"];
const SYNC_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x65, 0x61, 0x73, 0x79, 0x73, 0x73, 0x68, 0x2d, 0x73, 0x79, 0x6e, 0x63, 0x2d, 0x76, 0x31, 0x00,
]);

/// The server-side id of an account's personal vault. Deterministic, so every
/// device of the account agrees on it without asking the server.
pub fn personal_vault_server_id(account_id: &str) -> String {
    uuid::Uuid::new_v5(&SYNC_NAMESPACE, format!("personal:{account_id}").as_bytes()).to_string()
}

// ---- turning fields back into entities -----------------------------------

/// An entity the sync can rebuild from a record's fields.
trait Syncable: Entity {
    /// `base` is the stored item (its secrets and local-only values carry
    /// over); a field missing from `fields` keeps the base value.
    fn from_fields(base: Option<&Self>, id: &str, fields: &BTreeMap<String, Value>)
        -> Result<Self>;

    /// Extra checks on what arrived from another device; `Err(Invalid)`
    /// makes the sync skip the record.
    fn check_remote(&self) -> Result<()> {
        self.validate()
    }

    /// True when the row cannot be written because another row already
    /// holds the same natural key.
    fn duplicate_of_other(&self, _store: &Store, _vault_id: &str) -> Result<bool> {
        Ok(false)
    }
}

/// Reads `name` into `slot`. A `null` for a field that cannot be null is
/// "no value": the slot keeps what it had.
fn take<T: DeserializeOwned>(
    fields: &BTreeMap<String, Value>,
    name: &str,
    slot: &mut T,
) -> Result<()> {
    let Some(value) = fields.get(name) else {
        return Ok(());
    };
    match serde_json::from_value(value.clone()) {
        Ok(parsed) => *slot = parsed,
        Err(_) if value.is_null() => {}
        Err(error) => {
            return Err(StoreError::Invalid(format!(
                "synced field {name} is not usable: {error}"
            )))
        }
    }
    Ok(())
}

impl Syncable for Host {
    fn from_fields(
        base: Option<&Self>,
        id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        let mut host = base
            .cloned()
            .unwrap_or_else(|| Host::new(Connection::alias("", "")));
        host.connection.id = id.to_owned();
        host.connection.updated_at = Utc::now();
        let c = &mut host.connection;
        take(fields, "name", &mut c.name)?;
        take(fields, "target", &mut c.target)?;
        take(fields, "group_id", &mut c.group_id)?;
        take(fields, "favorite", &mut c.favorite)?;
        take(fields, "color", &mut c.color)?;
        take(fields, "notes", &mut c.notes)?;
        take(fields, "proxy_jump", &mut c.proxy_jump)?;
        take(fields, "local_forwards", &mut c.local_forwards)?;
        take(fields, "remote_forwards", &mut c.remote_forwards)?;
        take(fields, "dynamic_forwards", &mut c.dynamic_forwards)?;
        take(fields, "remote_command", &mut c.remote_command)?;
        take(fields, "local_directories", &mut c.local_directories)?;
        take(fields, "remote_directories", &mut c.remote_directories)?;
        take(fields, "terminal", &mut c.terminal)?;
        take(fields, "shell_integration", &mut c.shell_integration)?;
        take(fields, "engine", &mut c.engine)?;
        take(fields, "forward_x11", &mut c.forward_x11)?;
        take(fields, "proxy", &mut c.proxy)?;
        take(fields, "tags", &mut c.tags)?;
        take(fields, "notes_secret", &mut host.notes_secret)?;
        Ok(host)
    }

    fn check_remote(&self) -> Result<()> {
        self.validate()?;
        easyssh_core::security::validate_connection(&self.connection)
            .map_err(|error| StoreError::Invalid(error.to_string()))
    }
}

impl Syncable for Group {
    fn from_fields(
        base: Option<&Self>,
        id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        let mut group = base.cloned().unwrap_or(Group {
            id: id.to_owned(),
            name: String::new(),
            color: None,
            parent_id: None,
        });
        group.id = id.to_owned();
        take(fields, "name", &mut group.name)?;
        take(fields, "color", &mut group.color)?;
        take(fields, "parent_id", &mut group.parent_id)?;
        Ok(group)
    }
}

impl Syncable for CommandSnippet {
    fn from_fields(
        base: Option<&Self>,
        id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        let mut snippet = base.cloned().unwrap_or(CommandSnippet {
            id: id.to_owned(),
            name: String::new(),
            content: String::new(),
        });
        snippet.id = id.to_owned();
        take(fields, "name", &mut snippet.name)?;
        take(fields, "content", &mut snippet.content)?;
        Ok(snippet)
    }
}

impl Syncable for Tunnel {
    fn from_fields(
        base: Option<&Self>,
        id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        let mut tunnel = base.cloned().unwrap_or(Tunnel {
            id: id.to_owned(),
            name: String::new(),
            host_id: String::new(),
            kind: TunnelKind::Local,
            bind_address: None,
            bind_port: 0,
            target_host: None,
            target_port: None,
        });
        tunnel.id = id.to_owned();
        take(fields, "name", &mut tunnel.name)?;
        take(fields, "host_id", &mut tunnel.host_id)?;
        take(fields, "kind", &mut tunnel.kind)?;
        take(fields, "bind_address", &mut tunnel.bind_address)?;
        take(fields, "bind_port", &mut tunnel.bind_port)?;
        take(fields, "target_host", &mut tunnel.target_host)?;
        take(fields, "target_port", &mut tunnel.target_port)?;
        Ok(tunnel)
    }
}

impl Syncable for crate::known_hosts::KnownHost {
    fn from_fields(
        base: Option<&Self>,
        id: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<Self> {
        let mut entry = base
            .cloned()
            .unwrap_or_else(|| crate::known_hosts::KnownHost::new("", 22, "", Vec::new()));
        entry.id = id.to_owned();
        take(fields, "host", &mut entry.host)?;
        take(fields, "marker", &mut entry.marker)?;
        take(fields, "key_type", &mut entry.key_type)?;
        take(fields, "comment", &mut entry.comment)?;
        if let Some(Value::String(blob)) = fields.get("key_blob") {
            entry.key_blob = base64::engine::general_purpose::STANDARD
                .decode(blob)
                .map_err(|_| StoreError::Invalid("synced key is not base64".into()))?;
        }
        Ok(entry)
    }

    fn duplicate_of_other(&self, store: &Store, vault_id: &str) -> Result<bool> {
        Ok(store
            .conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM known_hosts
                 WHERE vault_id = ?1 AND host = ?2 AND key_type = ?3 AND key_blob = ?4 AND id != ?5)",
                params![vault_id, self.host, self.key_type, self.key_blob, self.id],
                |row| row.get::<_, i64>(0),
            )?
            != 0)
    }
}

// ---- rows to records and back ---------------------------------------------

fn vector_of(map: &BTreeMap<String, u64>) -> VersionVector {
    VersionVector::from_entries(map.iter().map(|(device, count)| (device.as_str(), *count)))
}

fn map_of(vector: &VersionVector) -> BTreeMap<String, u64> {
    vector
        .iter()
        .map(|(device, count)| (device.to_owned(), count))
        .collect()
}

fn stamp_of(stamp: &FieldStamp) -> Stamp {
    Stamp {
        lamport: stamp.lamport,
        device: stamp.device.clone(),
    }
}

fn field_stamp_of(stamp: &Stamp) -> FieldStamp {
    FieldStamp {
        lamport: stamp.lamport,
        device: stamp.device.clone(),
    }
}

/// The record of a row: non-secret fields with their stamps, the version
/// vector and the tombstone.
fn record_of(table: &str, fields: BTreeMap<String, Value>, meta: &Meta) -> RecordVersion {
    let mut version = RecordVersion::new(table);
    version.version_vector = vector_of(&meta.version_vector);
    for (name, value) in fields {
        let stamp = meta.field_stamps.get(&name).map(stamp_of).unwrap_or(Stamp {
            lamport: 0,
            device: String::new(),
        });
        version.stamps.insert(name.clone(), stamp);
        version.fields.insert(name, value);
    }
    if let Some(stamp) = meta.field_stamps.get(DELETED) {
        let stamp = stamp_of(stamp);
        if meta.deleted {
            version.tombstone = Some(stamp);
        } else {
            // A restored record: the restore outranks the delete it undid.
            version
                .fields
                .insert(DELETED.to_owned(), Value::Bool(false));
            version.stamps.insert(DELETED.to_owned(), stamp);
        }
    } else if meta.deleted {
        // A delete that was never stamped (should not happen): newer than
        // everything the row has.
        let lamport = version
            .stamps
            .values()
            .map(|stamp| stamp.lamport)
            .max()
            .unwrap_or(0)
            + 1;
        version.tombstone = Some(Stamp {
            lamport,
            device: String::new(),
        });
    }
    version
}

/// What the store keeps of a merged version: `field_stamps` (including the
/// stamp of the newest delete or restore) and the deleted flag.
fn stamps_of(version: &RecordVersion) -> (BTreeMap<String, FieldStamp>, bool) {
    let mut stamps: BTreeMap<String, FieldStamp> = version
        .stamps
        .iter()
        .filter(|(name, _)| name.as_str() != DELETED)
        .map(|(name, stamp)| (name.clone(), field_stamp_of(stamp)))
        .collect();
    let newest = [version.stamps.get(DELETED), version.tombstone.as_ref()]
        .into_iter()
        .flatten()
        .max();
    if let Some(stamp) = newest {
        stamps.insert(DELETED.to_owned(), field_stamp_of(stamp));
    }
    (stamps, version.is_deleted())
}

impl Store {
    fn export_entity<T: Syncable>(&self, id: &str) -> Result<Option<(RecordVersion, String)>> {
        let Some(record) = self.get_any::<T>(id)? else {
            return Ok(None);
        };
        let fields: BTreeMap<String, Value> = record
            .item
            .fields()
            .into_iter()
            .filter(|(_, field)| !field.secret)
            .map(|(name, field)| (name.to_owned(), field.value))
            .collect();
        Ok(Some((
            record_of(T::TABLE, fields, &record.meta),
            record.meta.vault_id,
        )))
    }

    fn export_any(&self, id: &str) -> Result<Option<(RecordVersion, String)>> {
        for table in ENTITY_TABLES {
            let found = match table {
                "groups" => self.export_entity::<Group>(id)?,
                "hosts" => self.export_entity::<Host>(id)?,
                "snippets" => self.export_entity::<CommandSnippet>(id)?,
                "tunnels" => self.export_entity::<Tunnel>(id)?,
                _ => self.export_entity::<crate::known_hosts::KnownHost>(id)?,
            };
            if found.is_some() {
                return Ok(found);
            }
        }
        Ok(None)
    }

    fn export_table(&self, table: &str, id: &str) -> Result<Option<(RecordVersion, String)>> {
        match table {
            "groups" => self.export_entity::<Group>(id),
            "hosts" => self.export_entity::<Host>(id),
            "snippets" => self.export_entity::<CommandSnippet>(id),
            "tunnels" => self.export_entity::<Tunnel>(id),
            _ => self.export_entity::<crate::known_hosts::KnownHost>(id),
        }
    }

    fn server_vector(&self, server_vault: &str, record_id: &str) -> Result<Option<VersionVector>> {
        let text: Option<String> = self
            .conn
            .query_row(
                "SELECT server_vector FROM sync_records WHERE vault_id = ?1 AND record_id = ?2",
                [server_vault, record_id],
                |row| row.get(0),
            )
            .optional()?;
        text.map(|text| {
            serde_json::from_str::<BTreeMap<String, u64>>(&text)
                .map(|map| vector_of(&map))
                .map_err(StoreError::from)
        })
        .transpose()
    }

    fn vault_meta_row(&self, local_vault: &str) -> Result<Option<(String, Meta)>> {
        let row = self
            .conn
            .query_row(
                "SELECT name, version_vector, field_stamps FROM vaults WHERE id = ?1 AND kind = 'team'",
                [local_vault],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((name, vector, stamps)) = row else {
            return Ok(None);
        };
        let mut meta = Meta::new(VAULT_META_RECORD, local_vault, Utc::now());
        meta.version_vector = serde_json::from_str(&vector)?;
        meta.field_stamps = serde_json::from_str(&stamps)?;
        Ok(Some((name, meta)))
    }

    fn sync_get(
        &self,
        personal: &str,
        server_vault: &str,
        record_id: &str,
    ) -> Result<Option<LocalRecord>> {
        let local_vault = local_vault_id(personal, server_vault);
        let found = if record_id == VAULT_META_RECORD {
            self.vault_meta_row(&local_vault)?.map(|(name, meta)| {
                let fields = BTreeMap::from([("name".to_owned(), Value::String(name))]);
                (
                    record_of(VAULT_META_TABLE, fields, &meta),
                    local_vault.clone(),
                )
            })
        } else {
            self.export_any(record_id)?
        };
        let Some((version, row_vault)) = found else {
            return Ok(None);
        };
        if row_vault != local_vault {
            return Ok(None);
        }
        Ok(Some(LocalRecord {
            vault_id: server_vault.to_owned(),
            record_id: record_id.to_owned(),
            version,
            server_vector: self.server_vector(server_vault, record_id)?,
        }))
    }

    fn sync_dirty(&self, personal: &str, server_vault: &str) -> Result<Vec<LocalRecord>> {
        self.sync_listing(personal, server_vault, true)
    }

    /// Every record of the vault with a local version, for re-encrypting it
    /// under a new vault key.
    fn sync_all(&self, personal: &str, server_vault: &str) -> Result<Vec<LocalRecord>> {
        self.sync_listing(personal, server_vault, false)
    }

    fn sync_listing(
        &self,
        personal: &str,
        server_vault: &str,
        only_dirty: bool,
    ) -> Result<Vec<LocalRecord>> {
        let local_vault = local_vault_id(personal, server_vault);
        let mut out = Vec::new();
        if local_vault != PERSONAL_VAULT_ID {
            if let Some((_, meta)) = self.vault_meta_row(&local_vault)? {
                if !meta.version_vector.is_empty() {
                    if let Some(record) =
                        self.sync_get(personal, server_vault, VAULT_META_RECORD)?
                    {
                        if !only_dirty || record.is_dirty() {
                            out.push(record);
                        }
                    }
                }
            }
        }
        for table in ENTITY_TABLES {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT t.id, t.version_vector, s.server_vector FROM {table} t
                 LEFT JOIN sync_records s ON s.vault_id = ?1 AND s.record_id = t.id
                 WHERE t.vault_id = ?2 ORDER BY t.id"
            ))?;
            let candidates = stmt
                .query_map([server_vault, local_vault.as_str()], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, vector, server) in candidates {
                let local: BTreeMap<String, u64> = serde_json::from_str(&vector)?;
                if local.is_empty() {
                    continue;
                }
                let server: Option<BTreeMap<String, u64>> =
                    server.map(|text| serde_json::from_str(&text)).transpose()?;
                if only_dirty && server.as_ref() == Some(&local) {
                    continue;
                }
                if let Some((version, _)) = self.export_table(table, &id)? {
                    out.push(LocalRecord {
                        vault_id: server_vault.to_owned(),
                        record_id: id,
                        version,
                        server_vector: server.as_ref().map(vector_of),
                    });
                }
            }
        }
        Ok(out)
    }

    fn apply_synced<T: Syncable>(
        &self,
        local_vault: &str,
        record: &LocalRecord,
        history: &[Overwritten],
    ) -> Result<()> {
        let id = &record.record_id;
        let existing = self.get_any::<T>(id)?;
        if existing
            .as_ref()
            .is_some_and(|row| row.meta.vault_id != local_vault)
        {
            // The same id is a record of another vault here; a record never
            // changes vault, so this is not ours to overwrite.
            return Ok(());
        }
        let version = &record.version;
        let mut item =
            match T::from_fields(existing.as_ref().map(|row| &row.item), id, &version.fields) {
                Ok(item) => item,
                Err(StoreError::Invalid(_)) => return Ok(()),
                Err(other) => return Err(other),
            };
        let deleted = version.is_deleted();
        if !deleted {
            match item.check_remote() {
                Ok(()) => {}
                Err(StoreError::Invalid(_)) => return Ok(()),
                Err(other) => return Err(other),
            }
        } else if item.validate().is_err() {
            return Ok(());
        }
        if item.duplicate_of_other(self, local_vault)? {
            return Ok(());
        }
        let (field_stamps, deleted) = stamps_of(version);
        let mut meta = Meta::new(id.as_str(), local_vault, Utc::now());
        meta.version_vector = map_of(&version.version_vector);
        meta.field_stamps = field_stamps;
        meta.deleted = deleted;
        item.sync_meta(&meta);
        let mut columns = item.columns(self, local_vault)?;
        columns.push(("id", rusqlite::types::Value::Text(meta.id.clone())));
        columns.push((
            "vault_id",
            rusqlite::types::Value::Text(local_vault.to_owned()),
        ));
        columns.push((
            "updated_at",
            rusqlite::types::Value::Text(fmt_ts(meta.updated_at)),
        ));
        columns.push((
            "version_vector",
            rusqlite::types::Value::Text(serde_json::to_string(&meta.version_vector)?),
        ));
        columns.push((
            "field_stamps",
            rusqlite::types::Value::Text(serde_json::to_string(&meta.field_stamps)?),
        ));
        columns.push((
            "deleted",
            rusqlite::types::Value::Integer(i64::from(meta.deleted)),
        ));
        self.write_row(T::TABLE, columns)?;
        item.after_write(self, &meta)?;
        self.write_overwritten(T::TABLE, id, history)
    }

    fn write_overwritten(&self, table: &str, id: &str, history: &[Overwritten]) -> Result<()> {
        let now = fmt_ts(Utc::now());
        for entry in history {
            if entry.field == DELETED {
                continue;
            }
            self.conn.execute(
                "INSERT INTO record_history (record_table, record_id, field, old_value, encrypted, changed_at, device)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
                params![
                    table,
                    id,
                    entry.field,
                    serde_json::to_string(&entry.value)?.into_bytes(),
                    now,
                    entry
                        .stamp
                        .as_ref()
                        .map_or_else(|| "sync".to_owned(), |stamp| stamp.device.clone()),
                ],
            )?;
        }
        Ok(())
    }

    fn apply_vault_meta(&self, local_vault: &str, record: &LocalRecord) -> Result<()> {
        let version = &record.version;
        let (stamps, _) = stamps_of(version);
        let name = version.fields.get("name").and_then(Value::as_str);
        if let Some(name) = name.filter(|name| !name.trim().is_empty()) {
            self.conn.execute(
                "UPDATE vaults SET name = ?1 WHERE id = ?2 AND kind = 'team'",
                params![name, local_vault],
            )?;
        }
        self.conn.execute(
            "UPDATE vaults SET version_vector = ?1, field_stamps = ?2 WHERE id = ?3 AND kind = 'team'",
            params![
                serde_json::to_string(&map_of(&version.version_vector))?,
                serde_json::to_string(&stamps)?,
                local_vault
            ],
        )?;
        Ok(())
    }

    fn sync_put(
        &self,
        personal: &str,
        record: LocalRecord,
        history: Vec<Overwritten>,
    ) -> Result<()> {
        let local_vault = local_vault_id(personal, &record.vault_id);
        // Referenced rows may arrive after the rows that name them (a host
        // before its group), so references are not enforced while a synced
        // row is written; `sync_repair_references` runs after a sync.
        self.conn.execute_batch("PRAGMA foreign_keys = OFF")?;
        let result = self.in_tx(|| {
            let table = record.version.table.as_str();
            if record.record_id == VAULT_META_RECORD && table == VAULT_META_TABLE {
                if local_vault != PERSONAL_VAULT_ID {
                    self.apply_vault_meta(&local_vault, &record)?;
                }
            } else {
                match table {
                    "groups" => self.apply_synced::<Group>(&local_vault, &record, &history)?,
                    "hosts" => self.apply_synced::<Host>(&local_vault, &record, &history)?,
                    "snippets" => {
                        self.apply_synced::<CommandSnippet>(&local_vault, &record, &history)?
                    }
                    "tunnels" => self.apply_synced::<Tunnel>(&local_vault, &record, &history)?,
                    "known_hosts" => self.apply_synced::<crate::known_hosts::KnownHost>(
                        &local_vault,
                        &record,
                        &history,
                    )?,
                    // A table a newer version syncs: remember we saw it.
                    _ => {}
                }
            }
            match &record.server_vector {
                Some(vector) => {
                    self.conn.execute(
                        "INSERT INTO sync_records (vault_id, record_id, server_vector) VALUES (?1, ?2, ?3)
                         ON CONFLICT(vault_id, record_id) DO UPDATE SET server_vector = excluded.server_vector",
                        params![
                            record.vault_id,
                            record.record_id,
                            serde_json::to_string(&map_of(vector))?
                        ],
                    )?;
                }
                None => {
                    self.conn.execute(
                        "DELETE FROM sync_records WHERE vault_id = ?1 AND record_id = ?2",
                        [&record.vault_id, &record.record_id],
                    )?;
                }
            }
            Ok(())
        });
        let restore = self.conn.execute_batch("PRAGMA foreign_keys = ON");
        result?;
        restore?;
        Ok(())
    }

    // ---- public bookkeeping used by the daemon ---------------------------

    /// Clears references that name rows which do not exist (a host whose
    /// group never arrived, for example). Does not stamp the rows.
    pub fn sync_repair_references(&self) -> Result<()> {
        self.conn.execute_batch(
            "UPDATE hosts SET group_id = NULL
               WHERE group_id IS NOT NULL AND group_id NOT IN (SELECT id FROM groups);
             UPDATE groups SET parent_id = NULL
               WHERE parent_id IS NOT NULL AND parent_id NOT IN (SELECT id FROM groups);",
        )?;
        Ok(())
    }

    /// Number of records waiting to be pushed to the server.
    pub fn sync_pending(&self, personal_server_vault: &str) -> Result<usize> {
        let mut total = 0;
        let mut vaults = vec![personal_server_vault.to_owned()];
        for vault in self.list_vaults()? {
            if vault.id != PERSONAL_VAULT_ID {
                vaults.push(vault.id);
            }
        }
        for vault in vaults {
            total += self.sync_dirty(personal_server_vault, &vault)?.len();
        }
        Ok(total)
    }

    /// SQLite's data version of this connection: it changes when another
    /// connection (another process) commits, not for this connection's own
    /// writes.
    pub fn data_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))?)
    }

    /// The signed-in account: server URL, username and the sealed secrets.
    pub fn sync_account(&self) -> Result<Option<SyncAccountRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT server_url, username, secrets_enc FROM sync_account WHERE id = 1",
                [],
                |row| {
                    Ok(SyncAccountRow {
                        server_url: row.get(0)?,
                        username: row.get(1)?,
                        secrets_enc: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Stores the account (sealing `secrets` with the field cipher).
    pub fn set_sync_account(&self, server_url: &str, username: &str, secrets: &[u8]) -> Result<()> {
        let sealed = self.cipher.encrypt(secrets, ACCOUNT_AAD)?;
        self.conn.execute(
            "INSERT INTO sync_account (id, server_url, username, secrets_enc, created_at)
             VALUES (1, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET server_url = excluded.server_url,
               username = excluded.username, secrets_enc = excluded.secrets_enc",
            params![server_url, username, sealed, fmt_ts(Utc::now())],
        )?;
        Ok(())
    }

    /// Opens the sealed secrets of `row`. Fails with the cipher's error
    /// (`Locked`) while the vault is locked.
    pub fn open_sync_secrets(&self, row: &SyncAccountRow) -> Result<Vec<u8>> {
        Ok(self.cipher.decrypt(&row.secrets_enc, ACCOUNT_AAD)?)
    }

    /// Forgets the account and every piece of sync bookkeeping. Hosts,
    /// groups and the other records stay on this device.
    pub fn clear_sync(&self) -> Result<()> {
        self.in_tx(|| {
            for table in [
                "sync_account",
                "sync_records",
                "sync_cursors",
                "sync_high_water",
                "sync_pins",
            ] {
                self.conn.execute(&format!("DELETE FROM {table}"), [])?;
            }
            self.conn
                .execute("UPDATE vaults SET role = NULL WHERE kind = 'team'", [])?;
            Ok(())
        })
    }

    pub fn load_high_water(&self) -> Result<HighWater> {
        let mut stmt = self
            .conn
            .prepare("SELECT vault_id, record_id, vector FROM sync_high_water")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut snapshot: HighWaterSnapshot = Vec::new();
        for (vault, record, vector) in rows {
            snapshot.push((vault, record, serde_json::from_str(&vector)?));
        }
        Ok(HighWater::from_snapshot(snapshot))
    }

    pub fn save_high_water(&self, high_water: &HighWater) -> Result<()> {
        self.in_tx(|| {
            for (vault, record, vector) in high_water.snapshot() {
                self.conn.execute(
                    "INSERT INTO sync_high_water (vault_id, record_id, vector) VALUES (?1, ?2, ?3)
                     ON CONFLICT(vault_id, record_id) DO UPDATE SET vector = excluded.vector",
                    params![vault, record, serde_json::to_string(&vector)?],
                )?;
            }
            Ok(())
        })
    }

    pub fn sync_cursor(&self, server_vault: &str) -> Result<u64> {
        Ok(self
            .conn
            .query_row(
                "SELECT cursor FROM sync_cursors WHERE vault_id = ?1",
                [server_vault],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .map_or(0, |cursor| cursor.max(0) as u64))
    }

    pub fn set_sync_cursor(&self, server_vault: &str, cursor: u64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_cursors (vault_id, cursor) VALUES (?1, ?2)
             ON CONFLICT(vault_id) DO UPDATE SET cursor = excluded.cursor",
            params![server_vault, i64::try_from(cursor).unwrap_or(i64::MAX)],
        )?;
        Ok(())
    }

    /// The pinned fingerprint of an account, if any.
    pub fn sync_pin(&self, account_id: &str) -> Result<Option<SyncPin>> {
        Ok(self
            .conn
            .query_row(
                "SELECT username, fingerprint, confirmed FROM sync_pins WHERE account_id = ?1",
                [account_id],
                |row| {
                    Ok(SyncPin {
                        username: row.get(0)?,
                        fingerprint: row.get(1)?,
                        confirmed: row.get::<_, i64>(2)? != 0,
                    })
                },
            )
            .optional()?)
    }

    pub fn set_sync_pin(
        &self,
        account_id: &str,
        username: &str,
        fingerprint: &str,
        confirmed: bool,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_pins (account_id, username, fingerprint, confirmed, pinned_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(account_id) DO UPDATE SET username = excluded.username,
               fingerprint = excluded.fingerprint, confirmed = excluded.confirmed",
            params![
                account_id,
                username,
                fingerprint,
                i64::from(confirmed),
                fmt_ts(Utc::now())
            ],
        )?;
        Ok(())
    }

    /// Makes sure a row exists for a team vault the server lists and records
    /// the signed-in account's role. A new row is named `name` until the
    /// vault's own name record arrives.
    pub fn ensure_team_vault(&self, id: &str, name: &str, role: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO vaults (id, name, kind, created_at, role) VALUES (?1, ?2, 'team', ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET role = excluded.role",
            params![id, name, fmt_ts(Utc::now()), role],
        )?;
        Ok(())
    }

    /// Creates a team vault locally with its name stamped by this device, so
    /// the name record is pushed with the next sync.
    pub fn create_team_vault(&self, id: &str, name: &str) -> Result<()> {
        self.in_tx(|| {
            self.ensure_team_vault(id, name, "owner")?;
            let now = Utc::now();
            let mut meta = Meta::new(VAULT_META_RECORD, id, now);
            meta.bump(&["name"], &self.device_id, now);
            self.conn.execute(
                "UPDATE vaults SET name = ?1, version_vector = ?2, field_stamps = ?3 WHERE id = ?4",
                params![
                    name,
                    serde_json::to_string(&meta.version_vector)?,
                    serde_json::to_string(&meta.field_stamps)?,
                    id
                ],
            )?;
            Ok(())
        })
    }

    /// Moves a host into another vault. The vault of a record never changes,
    /// so this creates a copy under a new id in `vault_id`, repoints the
    /// host's tunnels, and removes the original. Saved secrets are copied
    /// (the vault must be unlockable). Returns the new id.
    pub fn move_host_to_vault(&self, host_id: &str, vault_id: &str) -> Result<String> {
        if !self.list_vaults()?.iter().any(|vault| vault.id == vault_id) {
            return Err(StoreError::NotFound {
                entity: "vault",
                id: vault_id.to_owned(),
            });
        }
        self.in_tx(|| {
            let Some(record) = self.get::<Host>(host_id)? else {
                return Err(StoreError::NotFound {
                    entity: "host",
                    id: host_id.to_owned(),
                });
            };
            if record.meta.vault_id == vault_id {
                return Ok(host_id.to_owned());
            }
            let secrets = if self.host_secret_flags(host_id)?.any() {
                Some(self.host_secrets(host_id)?)
            } else {
                None
            };
            let new_id = uuid::Uuid::new_v4().to_string();
            let mut copy = record.item.clone();
            copy.connection.id = new_id.clone();
            // Groups belong to a vault; the copy starts ungrouped.
            copy.connection.group_id = None;
            copy.connection.created_at = Utc::now();
            copy.secrets = None;
            self.upsert_in(&copy, Utc::now(), Some(vault_id))?;
            if let Some(secrets) = secrets {
                use crate::secrets::{SecretChange, SecretsUpdate};
                let set = |value: Option<easyssh_crypto::SecretText>| {
                    value.map_or(SecretChange::Keep, SecretChange::Set)
                };
                self.set_host_secrets(
                    &new_id,
                    &SecretsUpdate {
                        password: set(secrets.password),
                        private_key: set(secrets.private_key),
                        passphrase: set(secrets.passphrase),
                        proxy_password: set(secrets.proxy_password),
                    },
                )?;
            }
            for tunnel in self.tunnels_for_host(host_id)? {
                let mut tunnel = tunnel.item;
                tunnel.host_id = new_id.clone();
                self.upsert_tunnel(&tunnel)?;
            }
            self.soft_delete_host(host_id)?;
            self.clear_host_secrets(host_id)?;
            Ok(new_id)
        })
    }

    /// Puts an old value of one field of a host back, as an ordinary edit:
    /// it is stamped, synced, and the value it replaces goes to history.
    pub fn restore_host_field(&self, host_id: &str, history_seq: i64) -> Result<()> {
        self.in_tx(|| {
            let Some(record) = self.get::<Host>(host_id)? else {
                return Err(StoreError::NotFound {
                    entity: "host",
                    id: host_id.to_owned(),
                });
            };
            let entry = self
                .history(host_id)?
                .into_iter()
                .find(|entry| entry.seq == history_seq)
                .ok_or_else(|| StoreError::NotFound {
                    entity: "history entry",
                    id: history_seq.to_string(),
                })?;
            if matches!(
                entry.field.as_str(),
                "password" | "private_key" | "passphrase"
            ) {
                return Err(StoreError::Invalid("secrets are not restored".into()));
            }
            let value = entry.old_value.unwrap_or(Value::Null);
            let fields = BTreeMap::from([(entry.field.clone(), value)]);
            let mut restored = Host::from_fields(Some(&record.item), host_id, &fields)?;
            // A restore never carries the saved secrets along.
            restored.secrets = None;
            restored.check_remote()?;
            self.upsert(&restored)?;
            Ok(())
        })
    }
}

/// The signed-in account as stored.
#[derive(Debug, Clone)]
pub struct SyncAccountRow {
    pub server_url: String,
    pub username: String,
    pub secrets_enc: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncPin {
    pub username: String,
    pub fingerprint: String,
    /// A person confirmed the fingerprint (as opposed to first sight).
    pub confirmed: bool,
}

fn local_vault_id(personal: &str, server_vault: &str) -> String {
    if server_vault == personal {
        PERSONAL_VAULT_ID.to_owned()
    } else {
        server_vault.to_owned()
    }
}

/// [`SyncStore`] over a shared [`Store`]. Each call takes the store's lock
/// for the duration of one query or transaction, never across an await.
pub struct StoreSync {
    store: Arc<Mutex<Store>>,
    personal_server_vault: String,
}

impl StoreSync {
    /// `personal_server_vault` is the server-side id of the account's
    /// personal vault (see [`personal_vault_server_id`]).
    pub fn new(store: Arc<Mutex<Store>>, personal_server_vault: impl Into<String>) -> Self {
        Self {
            store,
            personal_server_vault: personal_server_vault.into(),
        }
    }

    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl SyncStore for StoreSync {
    type Error = StoreError;

    fn get(&self, vault_id: &str, record_id: &str) -> Result<Option<LocalRecord>> {
        self.store()
            .sync_get(&self.personal_server_vault, vault_id, record_id)
    }

    fn dirty(&self, vault_id: &str) -> Result<Vec<LocalRecord>> {
        self.store()
            .sync_dirty(&self.personal_server_vault, vault_id)
    }

    fn all(&self, vault_id: &str) -> Result<Vec<LocalRecord>> {
        self.store().sync_all(&self.personal_server_vault, vault_id)
    }

    fn put(&mut self, record: LocalRecord, history: Vec<Overwritten>) -> Result<()> {
        self.store()
            .sync_put(&self.personal_server_vault, record, history)
    }

    fn cursor(&self, vault_id: &str) -> Result<u64> {
        self.store().sync_cursor(vault_id)
    }

    fn set_cursor(&mut self, vault_id: &str, cursor: u64) -> Result<()> {
        self.store().set_sync_cursor(vault_id, cursor)
    }
}

#[cfg(test)]
mod engine_field_tests {
    use super::Syncable;
    use crate::entities::Host;
    use crate::store::Entity;
    use easyssh_core::{Connection, EnginePreference};

    #[test]
    fn a_hosts_engine_choice_travels_with_it() {
        let mut connection = Connection::alias("engine", "engine");
        connection.id = uuid::Uuid::new_v4().to_string();
        connection.engine = Some(EnginePreference::Russh);
        let sent = Host::new(connection.clone());
        let fields: std::collections::BTreeMap<String, serde_json::Value> = sent
            .fields()
            .into_iter()
            .map(|(name, field)| (name.to_owned(), field.value))
            .collect();
        let received = Host::from_fields(None, &connection.id, &fields).unwrap();
        assert_eq!(received.connection.engine, connection.engine);
    }

    #[test]
    fn a_hosts_proxy_travels_with_it_but_its_password_does_not() {
        use easyssh_core::{ProxyKind, ProxySettings};
        let mut connection = Connection::alias("proxy", "proxy");
        connection.id = uuid::Uuid::new_v4().to_string();
        connection.proxy = Some(ProxySettings {
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".into(),
            port: 7890,
            username: Some("u".into()),
        });
        let sent = Host::new(connection.clone());
        let fields = sent.fields();
        assert!(fields.keys().all(|name| !name.contains("proxy_password")));
        let fields: std::collections::BTreeMap<String, serde_json::Value> = fields
            .into_iter()
            .map(|(name, field)| (name.to_owned(), field.value))
            .collect();
        let received = Host::from_fields(None, &connection.id, &fields).unwrap();
        assert_eq!(received.connection.proxy, connection.proxy);
        // A record from a build that has no proxy field leaves the host as it is.
        let mut older = fields.clone();
        older.remove("proxy");
        let kept = Host::from_fields(Some(&received), &connection.id, &older).unwrap();
        assert_eq!(kept.connection.proxy, connection.proxy);
    }
}
