//! Typed repositories: hosts, groups, tags, snippets, tunnels, jump chains.
//!
//! Where v1 already has a type (`Connection`, `Group`, `CommandSnippet`,
//! `Tunnel`) it is reused as is, so the v1 migration and later callers share
//! one shape.

use crate::error::{Result, StoreError};
use crate::meta::{fmt_ts, parse_ts, Meta};
use crate::store::{int, json, opt_int, opt_text, text, Entity, Field, Fields, Record, Store};
use chrono::Utc;
use easyssh_core::{
    CommandSnippet, Connection, ConnectionTarget, Group, SerialSettings, TerminalPreferences,
    Tunnel, TunnelKind,
};
use rusqlite::types::Value;
use rusqlite::Row;
use serde::{Deserialize, Serialize};

/// Generates the list/get/upsert/soft-delete/restore methods of an entity.
macro_rules! crud {
    ($ty:ty, $list:ident, $list_deleted:ident, $get:ident, $upsert:ident, $delete:ident, $restore:ident) => {
        impl Store {
            /// Live (not deleted) records.
            pub fn $list(&self) -> Result<Vec<Record<$ty>>> {
                self.list::<$ty>(false)
            }
            /// Soft-deleted records, which a restore can bring back.
            pub fn $list_deleted(&self) -> Result<Vec<Record<$ty>>> {
                self.list::<$ty>(true)
            }
            /// A live record, or `None` if missing or deleted.
            pub fn $get(&self, id: &str) -> Result<Option<Record<$ty>>> {
                self.get::<$ty>(id)
            }
            /// Inserts or updates by id. Unchanged input writes nothing.
            /// Fails on a soft-deleted record: restore it first.
            pub fn $upsert(&self, item: &$ty) -> Result<Record<$ty>> {
                self.upsert(item)
            }
            /// Tombstones a record; returns whether it exists.
            pub fn $delete(&self, id: &str) -> Result<bool> {
                self.set_deleted::<$ty>(id, true)
            }
            /// Clears a tombstone; returns whether the record exists.
            pub fn $restore(&self, id: &str) -> Result<bool> {
                self.set_deleted::<$ty>(id, false)
            }
        }
    };
}

// ---- hosts ---------------------------------------------------------------

/// Secret values of a host. Debug output never shows them.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct HostSecrets {
    pub password: Option<String>,
    pub private_key: Option<String>,
    pub passphrase: Option<String>,
}

impl std::fmt::Debug for HostSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mark = |value: &Option<String>| if value.is_some() { "<set>" } else { "<none>" };
        f.debug_struct("HostSecrets")
            .field("password", &mark(&self.password))
            .field("private_key", &mark(&self.private_key))
            .field("passphrase", &mark(&self.passphrase))
            .finish()
    }
}

impl HostSecrets {
    /// Empty strings count as unset.
    fn normalized(&self) -> Self {
        let clean = |value: &Option<String>| value.clone().filter(|value| !value.is_empty());
        Self {
            password: clean(&self.password),
            private_key: clean(&self.private_key),
            passphrase: clean(&self.passphrase),
        }
    }
}

/// A host: the v1 `Connection` plus secrets that exist only in encrypted form.
#[derive(Debug, Clone, PartialEq)]
pub struct Host {
    pub connection: Connection,
    /// When set, `connection.notes` is a secret and is stored encrypted.
    pub notes_secret: bool,
    /// `Some` replaces the stored secrets on upsert; `None` leaves them
    /// unchanged. Reads always return `Some`.
    pub secrets: Option<HostSecrets>,
}

impl Host {
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            notes_secret: false,
            secrets: None,
        }
    }
}

fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut tags: Vec<String> = tags
        .iter()
        .map(|tag| tag.trim().to_owned())
        .filter(|tag| !tag.is_empty())
        .collect();
    tags.sort();
    tags.dedup();
    tags
}

impl Entity for Host {
    const TABLE: &'static str = "hosts";
    const NAME: &'static str = "host";
    const ORDER: &'static str = "name COLLATE NOCASE, id";

    fn id(&self) -> &str {
        &self.connection.id
    }

    fn from_row(store: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self> {
        let serial = row
            .get::<_, Option<String>>("serial")?
            .map(|json| serde_json::from_str::<SerialSettings>(&json))
            .transpose()?;
        let target = match serial {
            Some(settings) => ConnectionTarget::Serial(settings),
            None => match row.get::<_, String>("target_kind")?.as_str() {
                "alias" => ConnectionTarget::Alias {
                    alias: row.get::<_, Option<String>>("alias")?.unwrap_or_default(),
                },
                _ => ConnectionTarget::Endpoint {
                    hostname: row
                        .get::<_, Option<String>>("hostname")?
                        .unwrap_or_default(),
                    username: row.get("username")?,
                    port: row.get::<_, Option<u16>>("port")?.unwrap_or(22),
                },
            },
        };
        let notes_secret = row.get::<_, i64>("notes_secret")? != 0;
        let open = |column: &str, field: &str| -> Result<Option<String>> {
            row.get::<_, Option<Vec<u8>>>(column)?
                .map(|blob| store.open_sealed(Self::TABLE, &meta.vault_id, &meta.id, field, &blob))
                .transpose()
        };
        let notes = if notes_secret {
            open("notes_enc", "notes")?.unwrap_or_default()
        } else {
            row.get("notes")?
        };
        let secrets = HostSecrets {
            password: open("password_enc", "password")?,
            private_key: open("private_key_enc", "private_key")?,
            passphrase: open("passphrase_enc", "passphrase")?,
        };
        let list = |column: &str| -> Result<Vec<String>> {
            Ok(serde_json::from_str(&row.get::<_, String>(column)?)?)
        };
        let mut tag_stmt = store.conn.prepare(
            "SELECT tags.name FROM host_tags JOIN tags ON tags.id = host_tags.tag_id
             WHERE host_tags.host_id = ?1 AND tags.deleted = 0 ORDER BY tags.name",
        )?;
        let tags = tag_stmt
            .query_map([&meta.id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let connection = Connection {
            id: meta.id.clone(),
            name: row.get("name")?,
            target,
            group_id: row.get("group_id")?,
            tags,
            favorite: row.get::<_, i64>("favorite")? != 0,
            color: row.get("color")?,
            notes,
            proxy_jump: row.get("proxy_jump")?,
            local_forwards: list("local_forwards")?,
            remote_forwards: list("remote_forwards")?,
            dynamic_forwards: list("dynamic_forwards")?,
            remote_command: row.get("remote_command")?,
            local_directories: list("local_directories")?,
            remote_directories: list("remote_directories")?,
            terminal: TerminalPreferences {
                font_size: row.get::<_, f64>("term_font_size")? as f32,
                scrollback_lines: row.get::<_, i64>("term_scrollback")?.max(0) as usize,
                startup_command: row.get("term_startup_command")?,
            },
            shell_integration: row.get::<_, i64>("shell_integration")? != 0,
            created_at: parse_ts(&row.get::<_, String>("created_at")?)?,
            updated_at: meta.updated_at,
            engine: row
                .get::<_, Option<String>>("engine")?
                .and_then(|name| serde_json::from_value(serde_json::Value::String(name)).ok()),
            forward_x11: row.get::<_, i64>("forward_x11")? != 0,
            // A value this build cannot read (written by a newer one) means
            // no proxy rather than a host that cannot be listed.
            proxy: row
                .get::<_, Option<String>>("proxy")?
                .and_then(|json| serde_json::from_str(&json).ok()),
        };
        Ok(Self {
            connection,
            notes_secret,
            secrets: Some(secrets),
        })
    }

    fn fields(&self) -> Fields {
        let c = &self.connection;
        let secrets = self.secrets.clone().unwrap_or_default().normalized();
        let mut fields = Fields::new();
        fields.insert("name", Field::plain(&c.name));
        fields.insert("target", Field::plain(&c.target));
        fields.insert("group_id", Field::plain(&c.group_id));
        fields.insert("favorite", Field::plain(c.favorite));
        fields.insert("color", Field::plain(&c.color));
        fields.insert(
            "notes",
            Field {
                value: serde_json::json!(c.notes),
                secret: self.notes_secret,
            },
        );
        fields.insert("notes_secret", Field::plain(self.notes_secret));
        fields.insert("proxy_jump", Field::plain(&c.proxy_jump));
        fields.insert("local_forwards", Field::plain(&c.local_forwards));
        fields.insert("remote_forwards", Field::plain(&c.remote_forwards));
        fields.insert("dynamic_forwards", Field::plain(&c.dynamic_forwards));
        fields.insert("remote_command", Field::plain(&c.remote_command));
        fields.insert("local_directories", Field::plain(&c.local_directories));
        fields.insert("remote_directories", Field::plain(&c.remote_directories));
        fields.insert("terminal", Field::plain(&c.terminal));
        fields.insert("shell_integration", Field::plain(c.shell_integration));
        fields.insert("engine", Field::plain(c.engine));
        fields.insert("forward_x11", Field::plain(c.forward_x11));
        fields.insert("proxy", Field::plain(&c.proxy));
        fields.insert("tags", Field::plain(clean_tags(&c.tags)));
        fields.insert("password", Field::secret(secrets.password));
        fields.insert("private_key", Field::secret(secrets.private_key));
        fields.insert("passphrase", Field::secret(secrets.passphrase));
        fields
    }

    fn columns(&self, store: &Store, vault_id: &str) -> Result<Vec<(&'static str, Value)>> {
        let c = &self.connection;
        let secrets = self.secrets.clone().unwrap_or_default().normalized();
        let seal = |field: &str, value: &Option<String>| -> Result<Value> {
            Ok(match value {
                Some(value) => {
                    Value::Blob(store.seal(Self::TABLE, vault_id, &c.id, field, value)?)
                }
                None => Value::Null,
            })
        };
        let mut serial = Value::Null;
        let (kind, alias, hostname, username, port) = match &c.target {
            ConnectionTarget::Alias { alias } => (
                "alias",
                opt_text(Some(alias)),
                Value::Null,
                Value::Null,
                Value::Null,
            ),
            ConnectionTarget::Endpoint {
                hostname,
                username,
                port,
            } => (
                "endpoint",
                Value::Null,
                opt_text(Some(hostname)),
                opt_text(username.as_deref()),
                int(*port),
            ),
            // Stored as an endpoint whose hostname is the port path; the
            // `serial` column says what it really is.
            ConnectionTarget::Serial(settings) => {
                serial = json(settings)?;
                (
                    "endpoint",
                    Value::Null,
                    opt_text(Some(&settings.port)),
                    Value::Null,
                    Value::Null,
                )
            }
        };
        let secret_notes = self.notes_secret && !c.notes.is_empty();
        Ok(vec![
            ("created_at", text(fmt_ts(c.created_at))),
            ("name", text(&c.name)),
            ("target_kind", text(kind)),
            ("alias", alias),
            ("hostname", hostname),
            ("username", username),
            ("port", port),
            ("serial", serial),
            ("group_id", opt_text(c.group_id.as_deref())),
            ("favorite", int(c.favorite)),
            ("color", opt_text(c.color.as_deref())),
            ("notes", text(if self.notes_secret { "" } else { &c.notes })),
            ("notes_secret", int(self.notes_secret)),
            (
                "notes_enc",
                seal("notes", &secret_notes.then(|| c.notes.clone()))?,
            ),
            ("proxy_jump", opt_text(c.proxy_jump.as_deref())),
            ("local_forwards", json(&c.local_forwards)?),
            ("remote_forwards", json(&c.remote_forwards)?),
            ("dynamic_forwards", json(&c.dynamic_forwards)?),
            ("remote_command", opt_text(c.remote_command.as_deref())),
            ("local_directories", json(&c.local_directories)?),
            ("remote_directories", json(&c.remote_directories)?),
            ("shell_integration", int(i64::from(c.shell_integration))),
            ("forward_x11", int(i64::from(c.forward_x11))),
            (
                "engine",
                match c.engine {
                    Some(engine) => text(
                        serde_json::to_value(engine)
                            .ok()
                            .and_then(|value| value.as_str().map(str::to_owned))
                            .unwrap_or_default(),
                    ),
                    None => Value::Null,
                },
            ),
            (
                "proxy",
                match &c.proxy {
                    Some(proxy) => json(proxy)?,
                    None => Value::Null,
                },
            ),
            (
                "term_font_size",
                Value::Real(f64::from(c.terminal.font_size)),
            ),
            (
                "term_scrollback",
                int(i64::try_from(c.terminal.scrollback_lines).unwrap_or(i64::MAX)),
            ),
            (
                "term_startup_command",
                opt_text(c.terminal.startup_command.as_deref()),
            ),
            ("password_enc", seal("password", &secrets.password)?),
            (
                "private_key_enc",
                seal("private_key", &secrets.private_key)?,
            ),
            ("passphrase_enc", seal("passphrase", &secrets.passphrase)?),
        ])
    }

    fn keep_from(&mut self, old: &Self) {
        if self.secrets.is_none() {
            self.secrets = old.secrets.clone();
        }
        self.connection.created_at = old.connection.created_at;
    }

    fn sync_meta(&mut self, meta: &Meta) {
        self.connection.updated_at = meta.updated_at;
        self.connection.tags = clean_tags(&self.connection.tags);
        self.connection.created_at = crate::meta::trunc_us(self.connection.created_at);
        self.secrets = Some(self.secrets.take().unwrap_or_default().normalized());
    }

    fn after_write(&self, store: &Store, meta: &Meta) -> Result<()> {
        store
            .conn
            .execute("DELETE FROM host_tags WHERE host_id = ?1", [&meta.id])?;
        for name in clean_tags(&self.connection.tags) {
            let tag_id = store.ensure_tag_in(&meta.vault_id, &name, meta.updated_at)?;
            store.conn.execute(
                "INSERT OR IGNORE INTO host_tags (host_id, tag_id) VALUES (?1, ?2)",
                [&meta.id, &tag_id],
            )?;
        }
        Ok(())
    }
}

crud!(
    Host,
    list_hosts,
    list_deleted_hosts,
    get_host,
    upsert_host,
    soft_delete_host,
    restore_host
);

// ---- groups --------------------------------------------------------------

impl Entity for Group {
    const TABLE: &'static str = "groups";
    const NAME: &'static str = "group";
    const ORDER: &'static str = "name COLLATE NOCASE, id";
    fn id(&self) -> &str {
        &self.id
    }
    fn from_row(_: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self> {
        Ok(Self {
            id: meta.id.clone(),
            name: row.get("name")?,
            color: row.get("color")?,
            parent_id: row.get("parent_id")?,
        })
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.insert("name", Field::plain(&self.name));
        fields.insert("color", Field::plain(&self.color));
        fields.insert("parent_id", Field::plain(&self.parent_id));
        fields
    }
    fn columns(&self, _: &Store, _: &str) -> Result<Vec<(&'static str, Value)>> {
        Ok(vec![
            ("name", text(&self.name)),
            ("color", opt_text(self.color.as_deref())),
            ("parent_id", opt_text(self.parent_id.as_deref())),
        ])
    }
}
crud!(
    Group,
    list_groups,
    list_deleted_groups,
    get_group,
    upsert_group,
    soft_delete_group,
    restore_group
);

// ---- snippets ------------------------------------------------------------

impl Entity for CommandSnippet {
    const TABLE: &'static str = "snippets";
    const NAME: &'static str = "snippet";
    const ORDER: &'static str = "name COLLATE NOCASE, id";
    fn id(&self) -> &str {
        &self.id
    }
    fn from_row(_: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self> {
        Ok(Self {
            id: meta.id.clone(),
            name: row.get("name")?,
            content: row.get("content")?,
        })
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.insert("name", Field::plain(&self.name));
        fields.insert("content", Field::plain(&self.content));
        fields
    }
    fn columns(&self, _: &Store, _: &str) -> Result<Vec<(&'static str, Value)>> {
        Ok(vec![
            ("name", text(&self.name)),
            ("content", text(&self.content)),
        ])
    }
}
crud!(
    CommandSnippet,
    list_snippets,
    list_deleted_snippets,
    get_snippet,
    upsert_snippet,
    soft_delete_snippet,
    restore_snippet
);

// ---- tunnels -------------------------------------------------------------

fn kind_str(kind: TunnelKind) -> &'static str {
    match kind {
        TunnelKind::Local => "local",
        TunnelKind::Remote => "remote",
        TunnelKind::Dynamic => "dynamic",
    }
}

impl Entity for Tunnel {
    const TABLE: &'static str = "tunnels";
    const NAME: &'static str = "tunnel";
    const ORDER: &'static str = "name COLLATE NOCASE, id";
    fn id(&self) -> &str {
        &self.id
    }
    fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.id)
            .map(|_| ())
            .map_err(|_| StoreError::Invalid("tunnel id must be a UUID".into()))
    }
    fn from_row(_: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self> {
        let kind = match row.get::<_, String>("kind")?.as_str() {
            "remote" => TunnelKind::Remote,
            "dynamic" => TunnelKind::Dynamic,
            _ => TunnelKind::Local,
        };
        Ok(Self {
            id: meta.id.clone(),
            name: row.get("name")?,
            host_id: row.get("host_id")?,
            kind,
            bind_address: row.get("bind_address")?,
            bind_port: row.get("bind_port")?,
            target_host: row.get("target_host")?,
            target_port: row.get("target_port")?,
        })
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.insert("name", Field::plain(&self.name));
        fields.insert("host_id", Field::plain(&self.host_id));
        fields.insert("kind", Field::plain(self.kind));
        fields.insert("bind_address", Field::plain(&self.bind_address));
        fields.insert("bind_port", Field::plain(self.bind_port));
        fields.insert("target_host", Field::plain(&self.target_host));
        fields.insert("target_port", Field::plain(self.target_port));
        fields
    }
    fn columns(&self, _: &Store, _: &str) -> Result<Vec<(&'static str, Value)>> {
        Ok(vec![
            ("name", text(&self.name)),
            ("host_id", text(&self.host_id)),
            ("kind", text(kind_str(self.kind))),
            ("bind_address", opt_text(self.bind_address.as_deref())),
            ("bind_port", int(self.bind_port)),
            ("target_host", opt_text(self.target_host.as_deref())),
            ("target_port", opt_int(self.target_port.map(i64::from))),
        ])
    }
}
crud!(
    Tunnel,
    list_tunnels,
    list_deleted_tunnels,
    get_tunnel,
    upsert_tunnel,
    soft_delete_tunnel,
    restore_tunnel
);

impl Store {
    /// Live tunnels that go through `host_id`.
    pub fn tunnels_for_host(&self, host_id: &str) -> Result<Vec<Record<Tunnel>>> {
        self.select::<Tunnel>("deleted = 0 AND host_id = ?1", &[&host_id])
    }
}

// ---- tags ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tag {
    pub id: String,
    pub name: String,
}

impl Entity for Tag {
    const TABLE: &'static str = "tags";
    const NAME: &'static str = "tag";
    const ORDER: &'static str = "name COLLATE NOCASE, id";
    fn id(&self) -> &str {
        &self.id
    }
    fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(StoreError::Invalid("tag name is empty".into()));
        }
        uuid::Uuid::parse_str(&self.id)
            .map(|_| ())
            .map_err(|_| StoreError::Invalid("tag id must be a UUID".into()))
    }
    fn from_row(_: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self> {
        Ok(Self {
            id: meta.id.clone(),
            name: row.get("name")?,
        })
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.insert("name", Field::plain(&self.name));
        fields
    }
    fn columns(&self, _: &Store, _: &str) -> Result<Vec<(&'static str, Value)>> {
        Ok(vec![("name", text(&self.name))])
    }
}
crud!(
    Tag,
    list_tags,
    list_deleted_tags,
    get_tag,
    upsert_tag,
    soft_delete_tag,
    restore_tag
);

impl Store {
    /// The id of the live tag called `name` in the active vault, creating or
    /// restoring it as needed.
    pub fn ensure_tag(&self, name: &str) -> Result<String> {
        let vault_id = self.vault_id.clone();
        self.in_tx(|| self.ensure_tag_in(&vault_id, name.trim(), Utc::now()))
    }

    pub(crate) fn ensure_tag_in(
        &self,
        vault_id: &str,
        name: &str,
        now: chrono::DateTime<Utc>,
    ) -> Result<String> {
        if name.is_empty() {
            return Err(StoreError::Invalid("tag name is empty".into()));
        }
        let existing = self
            .select::<Tag>("vault_id = ?1 AND name = ?2", &[&vault_id, &name])?
            .into_iter()
            .next();
        if let Some(record) = existing {
            if record.meta.deleted {
                self.set_deleted::<Tag>(&record.meta.id, false)?;
            }
            return Ok(record.meta.id);
        }
        let tag = Tag {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.to_owned(),
        };
        let fields = tag.fields();
        let names: Vec<&str> = fields.keys().copied().collect();
        let mut meta = Meta::new(&tag.id, vault_id, now);
        meta.bump(&names, &self.device_id, now);
        let mut columns = tag.columns(self, vault_id)?;
        columns.extend([
            ("id", text(&meta.id)),
            ("vault_id", text(vault_id)),
            ("updated_at", text(fmt_ts(now))),
            ("version_vector", json(&meta.version_vector)?),
            ("field_stamps", json(&meta.field_stamps)?),
            ("deleted", int(0)),
        ]);
        self.write_row(Tag::TABLE, columns)?;
        Ok(tag.id)
    }
}

// ---- jump chains ---------------------------------------------------------

/// The ordered hops (host ids, first hop first) used to reach `host_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JumpChain {
    pub id: String,
    pub host_id: String,
    pub hops: Vec<String>,
}

impl Entity for JumpChain {
    const TABLE: &'static str = "jump_chains";
    const NAME: &'static str = "jump chain";
    const ORDER: &'static str = "host_id";
    fn id(&self) -> &str {
        &self.id
    }
    fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.id)
            .map_err(|_| StoreError::Invalid("jump chain id must be a UUID".into()))?;
        let mut seen = std::collections::BTreeSet::new();
        for hop in &self.hops {
            if hop == &self.host_id {
                return Err(StoreError::Invalid(
                    "a jump chain cannot pass through its own target".into(),
                ));
            }
            if !seen.insert(hop) {
                return Err(StoreError::Invalid(
                    "a jump chain cannot repeat a hop".into(),
                ));
            }
        }
        Ok(())
    }
    fn from_row(store: &Store, row: &Row<'_>, meta: &Meta) -> Result<Self> {
        let mut stmt = store.conn.prepare(
            "SELECT hop_host_id FROM jump_chain_hops WHERE chain_id = ?1 ORDER BY position",
        )?;
        let hops = stmt
            .query_map([&meta.id], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Self {
            id: meta.id.clone(),
            host_id: row.get("host_id")?,
            hops,
        })
    }
    fn fields(&self) -> Fields {
        let mut fields = Fields::new();
        fields.insert("host_id", Field::plain(&self.host_id));
        fields.insert("hops", Field::plain(&self.hops));
        fields
    }
    fn columns(&self, _: &Store, _: &str) -> Result<Vec<(&'static str, Value)>> {
        Ok(vec![("host_id", text(&self.host_id))])
    }
    fn after_write(&self, store: &Store, meta: &Meta) -> Result<()> {
        store.conn.execute(
            "DELETE FROM jump_chain_hops WHERE chain_id = ?1",
            [&meta.id],
        )?;
        for (position, hop) in self.hops.iter().enumerate() {
            store.conn.execute(
                "INSERT INTO jump_chain_hops (chain_id, position, hop_host_id) VALUES (?1, ?2, ?3)",
                rusqlite::params![meta.id, position as i64, hop],
            )?;
        }
        Ok(())
    }
}
crud!(
    JumpChain,
    list_jump_chains,
    list_deleted_jump_chains,
    get_jump_chain,
    upsert_jump_chain,
    soft_delete_jump_chain,
    restore_jump_chain
);

impl Store {
    /// The live chain that leads to `host_id`, if one is defined.
    pub fn jump_chain_for_host(&self, host_id: &str) -> Result<Option<Record<JumpChain>>> {
        Ok(self
            .select::<JumpChain>("deleted = 0 AND host_id = ?1", &[&host_id])?
            .into_iter()
            .next())
    }
}
