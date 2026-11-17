//! How the app uses the store: where the database lives, the first-start
//! migration of the v1 JSON file, and the bridge between the store and the
//! `AppConfig` the v1 code still passes around.
//!
//! The daemon and the app's in-process fallback both call [`open_app_store`],
//! so they open the same file, migrate the same way and agree. Several
//! processes may hold the database open at once: it runs in WAL mode with a
//! busy timeout (see [`Store::open`]), and a migration is one transaction.

use crate::cipher::NotConfigured;
use crate::entities::Host;
use crate::error::{Result, StoreError};
use crate::store::{Entity, Store};
use crate::v1::{store_id, MigrationReport, MIGRATION_SETTING};
use easyssh_core::domain::{SyncPreferences, TelemetryPreferences};
use easyssh_core::{
    AppConfig, CommandSnippet, Connection, DisplayDensity, ExperimentalFeatures, Group, Locale,
    SidebarPreferences, TerminalPreferences, Theme, Tunnel, Workspace,
};
use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The database file, next to the v1 JSON file.
pub const DB_FILE_NAME: &str = "easyssh.db";
/// This installation's id, stamped on every write.
const DEVICE_ID_FILE_NAME: &str = "device-id";

/// The database that belongs to the v1 configuration file at `v1_config_path`.
pub fn db_path_for(v1_config_path: &Path) -> PathBuf {
    v1_config_path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .join(DB_FILE_NAME)
}

/// A store opened by [`open_app_store`] and what the first-start migration did.
#[derive(Debug)]
pub struct AppStore {
    pub store: Store,
    /// Set when this call migrated the v1 file.
    pub migration: Option<MigrationReport>,
    /// Set when a v1 file exists but could not be migrated (it is left
    /// untouched and the next start tries again).
    pub migration_error: Option<String>,
}

/// Opens `<dir of the v1 file>/easyssh.db` and, if the v1 JSON exists and has
/// not been migrated into it yet, imports it (keeping `<stem>.v1-backup.json`).
/// Calling this again, or from another process, never migrates twice.
pub fn open_app_store(v1_config_path: &Path) -> Result<AppStore> {
    let db = db_path_for(v1_config_path);
    let dir = db.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let device = device_id(dir)?;
    let store = Store::open(&db, device, Arc::new(NotConfigured))?;
    let mut opened = AppStore {
        store,
        migration: None,
        migration_error: None,
    };
    if opened.store.get_setting(MIGRATION_SETTING)?.is_none() && v1_config_path.is_file() {
        match opened.store.migrate_from_v1(v1_config_path) {
            Ok(report) => opened.migration = Some(report),
            Err(error) => opened.migration_error = Some(error.to_string()),
        }
    }
    Ok(opened)
}

/// This installation's id: created once, then read back by every process.
fn device_id(dir: &Path) -> Result<String> {
    let path = dir.join(DEVICE_ID_FILE_NAME);
    let read = || {
        std::fs::read_to_string(&path)
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| uuid::Uuid::parse_str(text).is_ok())
    };
    if let Some(id) = read() {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().to_string();
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            file.write_all(id.as_bytes())?;
            Ok(id)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // Another process is creating it right now.
            for _ in 0..50 {
                if let Some(id) = read() {
                    return Ok(id);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(id)
        }
        Err(error) => Err(error.into()),
    }
}

impl Store {
    /// Inserts or updates a record, bringing it back first if it was removed.
    fn put<T: Entity>(&self, item: &T) -> Result<()> {
        self.in_tx(|| {
            self.set_deleted::<T>(item.id(), false)?;
            self.upsert(item).map(|_| ())
        })
    }

    /// Saves a host, and `group` first when given and not stored yet. Group ids
    /// that are not UUIDs (clients of the v1 era made up their own) become the
    /// UUIDs the migration would give them, in the group and in the host's
    /// reference alike. A `group_id` that names no stored group is dropped, so
    /// the host stays usable instead of failing on a dangling reference.
    pub fn save_host(&self, connection: &Connection, group: Option<&Group>) -> Result<()> {
        self.in_tx(|| {
            if let Some(group) = group {
                let mut group = group.clone();
                group.id = store_id(&group.id);
                group.parent_id = group.parent_id.as_deref().map(store_id);
                if self.get_group(&group.id)?.is_none() {
                    if group.parent_id.as_deref().is_some_and(|parent| {
                        parent == group.id || self.get_group(parent).ok().flatten().is_none()
                    }) {
                        group.parent_id = None;
                    }
                    self.put(&group)?;
                }
            }
            let mut connection = connection.clone();
            connection.group_id = connection.group_id.as_deref().map(store_id);
            if let Some(id) = connection.group_id.clone() {
                if self.get_group(&id)?.is_none() {
                    connection.group_id = None;
                }
            }
            self.put(&Host::new(connection))
        })
    }

    /// Removes a host and its mention in the recent list. Unknown ids are not
    /// an error; returns whether the host existed.
    pub fn remove_host(&self, id: &str) -> Result<bool> {
        self.in_tx(|| {
            let existed = self.soft_delete_host(id)?;
            let recent = self.recent_connection_ids();
            if recent.iter().any(|recent| recent == id) {
                let kept: Vec<String> = recent.into_iter().filter(|r| r != id).collect();
                self.set_setting_as("recent_connection_ids", &kept)?;
            }
            Ok(existed)
        })
    }

    fn recent_connection_ids(&self) -> Vec<String> {
        self.get_setting_as("recent_connection_ids")
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Everything the store holds, as the `AppConfig` the v1 code works with.
    /// Launch history is not stored and comes back empty.
    pub fn load_app_config(&self) -> Result<AppConfig> {
        let mut config = AppConfig::new();
        config.connections = self
            .list_hosts()?
            .into_iter()
            .map(|record| record.item.connection)
            .collect();
        config.groups = self.list_groups()?.into_iter().map(|r| r.item).collect();
        config.snippets = self.list_snippets()?.into_iter().map(|r| r.item).collect();
        config.tunnels = self.list_tunnels()?.into_iter().map(|r| r.item).collect();
        // A setting that does not parse falls back to its default rather than
        // keeping the whole app from starting.
        fn setting<T: serde::de::DeserializeOwned>(store: &Store, key: &str) -> Option<T> {
            store.get_setting_as(key).ok().flatten()
        }
        config.theme = setting::<Theme>(self, "theme").unwrap_or(Theme::System);
        config.locale = setting::<Locale>(self, "locale").unwrap_or(Locale::System);
        config.display_density =
            setting::<DisplayDensity>(self, "display_density").unwrap_or_default();
        config.terminal = setting::<TerminalPreferences>(self, "terminal").unwrap_or_default();
        config.workspace = setting::<Workspace>(self, "workspace")
            .unwrap_or(Workspace::Home)
            .migrated();
        config.sidebar = setting::<SidebarPreferences>(self, "sidebar").unwrap_or_default();
        config.sync = setting::<SyncPreferences>(self, "sync").unwrap_or_default();
        config.experimental =
            setting::<ExperimentalFeatures>(self, "experimental").unwrap_or_default();
        config.telemetry = setting::<TelemetryPreferences>(self, "telemetry").unwrap_or_default();
        config.recent_connection_ids = self.recent_connection_ids();
        Ok(config)
    }

    /// Saves one snippet. Touches nothing else.
    pub fn save_snippet(&self, snippet: &CommandSnippet) -> Result<()> {
        self.put(&CommandSnippet {
            id: store_id(&snippet.id),
            ..snippet.clone()
        })
    }

    /// Removes a snippet; returns whether it existed.
    pub fn remove_snippet(&self, id: &str) -> Result<bool> {
        self.set_deleted::<CommandSnippet>(&store_id(id), true)
    }

    /// Saves one tunnel. Touches nothing else.
    pub fn save_tunnel(&self, tunnel: &Tunnel) -> Result<()> {
        self.put(&Tunnel {
            id: store_id(&tunnel.id),
            host_id: store_id(&tunnel.host_id),
            ..tunnel.clone()
        })
    }

    /// Removes a tunnel; returns whether it existed.
    pub fn remove_tunnel(&self, id: &str) -> Result<bool> {
        self.set_deleted::<Tunnel>(&store_id(id), true)
    }

    /// Stores the Git sync settings. Touches nothing else.
    pub fn save_sync_preferences(&self, sync: &SyncPreferences) -> Result<()> {
        self.set_setting_as("sync", sync)
    }

    /// Adds or updates every group, host, snippet and tunnel in `config`
    /// (unchanged ones write nothing) and removes nothing: a record that is
    /// stored but missing from `config` stays. For merging an import.
    pub fn upsert_records(&self, config: &AppConfig) -> Result<()> {
        self.in_tx(|| {
            self.conn.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            let records = Records::of(config);
            for item in &records.groups {
                self.put(item)?;
            }
            for item in &records.hosts {
                self.put(item)?;
            }
            for item in &records.snippets {
                self.put(item)?;
            }
            for item in &records.tunnels {
                self.put(item)?;
            }
            Ok(())
        })
    }

    /// Makes the groups, hosts, snippets and tunnels in the store exactly
    /// those of `config`: stored records missing from it are removed. One
    /// transaction. This is the only call that can remove a record it was not
    /// told about, so use it for an explicit replacement (a restore, a sync
    /// pull) and never for an ordinary edit, which would drop whatever another
    /// process saved since `config` was loaded. Settings are not touched.
    pub fn replace_all(&self, config: &AppConfig) -> Result<()> {
        self.in_tx(|| {
            self.conn.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            let records = Records::of(config);
            self.sync_records(records.groups)?;
            self.sync_records(records.hosts)?;
            self.sync_records(records.snippets)?;
            self.sync_records(records.tunnels)
        })
    }

    /// Saves `wanted` and removes every other live record of the same kind.
    fn sync_records<T: Entity>(&self, wanted: Vec<T>) -> Result<()> {
        let keep: BTreeSet<String> = wanted.iter().map(|item| item.id().to_owned()).collect();
        for item in &wanted {
            self.put(item)?;
        }
        for record in self.list::<T>(false)? {
            if !keep.contains(record.item.id()) {
                self.set_deleted::<T>(record.item.id(), true)?;
            }
        }
        Ok(())
    }
}

/// The records of an `AppConfig` as the store keeps them: ids that are not
/// UUIDs (a v1 leftover) become the UUIDs the migration gives them, and a
/// reference to a group that is not in `config` is dropped.
struct Records {
    groups: Vec<Group>,
    hosts: Vec<Host>,
    snippets: Vec<CommandSnippet>,
    tunnels: Vec<Tunnel>,
}

impl Records {
    fn of(config: &AppConfig) -> Self {
        let group_ids: BTreeSet<String> = config.groups.iter().map(|g| store_id(&g.id)).collect();
        let known_group = |id: &Option<String>| {
            id.as_deref()
                .map(store_id)
                .filter(|id| group_ids.contains(id))
        };
        Self {
            groups: config
                .groups
                .iter()
                .map(|group| {
                    let mut group = group.clone();
                    group.id = store_id(&group.id);
                    group.parent_id = known_group(&group.parent_id).filter(|p| *p != group.id);
                    group
                })
                .collect(),
            hosts: config
                .connections
                .iter()
                .map(|connection| {
                    let mut connection = connection.clone();
                    connection.id = store_id(&connection.id);
                    connection.group_id = known_group(&connection.group_id);
                    Host::new(connection)
                })
                .collect(),
            snippets: config
                .snippets
                .iter()
                .map(|snippet| CommandSnippet {
                    id: store_id(&snippet.id),
                    ..snippet.clone()
                })
                .collect(),
            tunnels: config
                .tunnels
                .iter()
                .map(|tunnel| Tunnel {
                    id: store_id(&tunnel.id),
                    host_id: store_id(&tunnel.host_id),
                    ..tunnel.clone()
                })
                .collect(),
        }
    }
}

/// True for errors that are the caller's value being refused (as opposed to a
/// failure of the database or the disk).
pub fn is_invalid_input(error: &StoreError) -> bool {
    matches!(error, StoreError::Invalid(_))
}
