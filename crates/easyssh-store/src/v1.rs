//! Migration of the v1 JSON configuration into the store.
//!
//! The v1 file is read through easyssh-core's `ConfigStore`, so the same
//! rules apply as in v1 (a file holding credential-like keys is refused). The
//! original is never modified or deleted: a copy is kept beside it as
//! `<name>.v1-backup.json` and the import is idempotent.

use crate::entities::Host;
use crate::error::{Result, StoreError};
use crate::store::Store;
use chrono::Utc;
use easyssh_core::security::validate_connection;
use easyssh_core::{AppConfig, ConfigStore};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Setting that records a finished migration.
pub const MIGRATION_SETTING: &str = "migration.v1";

/// Fixed namespace so v1 ids that are not UUIDs map to the same UUID on every
/// run and on every machine.
const ID_NAMESPACE: Uuid = Uuid::from_u128(0x6b1f_7c0e_2d54_4a8e_9b3a_5e1d_0c77_a001);

/// Counts for one kind of entity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EntityCounts {
    pub imported: usize,
    /// Already in the store (from an earlier run), left untouched.
    pub existing: usize,
}

/// Something the migration did not import, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub kind: &'static str,
    pub id: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// This exact file was migrated by an earlier run; nothing was imported.
    pub already_migrated: bool,
    pub backup_path: Option<PathBuf>,
    /// The backup was written by this run (false if one already existed).
    pub backup_created: bool,
    pub hosts: EntityCounts,
    pub groups: EntityCounts,
    pub snippets: EntityCounts,
    pub tunnels: EntityCounts,
    pub settings: EntityCounts,
    pub skipped: Vec<Skipped>,
    /// Data that was imported but adjusted.
    pub warnings: Vec<String>,
}

/// A v1 id as a store id: UUIDs pass through, anything else becomes a stable
/// UUIDv5 so references between records keep working.
pub(crate) fn store_id(v1_id: &str) -> String {
    Uuid::parse_str(v1_id).map_or_else(
        |_| Uuid::new_v5(&ID_NAMESPACE, v1_id.as_bytes()).to_string(),
        |uuid| uuid.to_string(),
    )
}

fn backup_path(source: &Path) -> PathBuf {
    let stem = source.file_stem().map_or_else(
        || "config".into(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    source.with_file_name(format!("{stem}.v1-backup.json"))
}

/// Copies `source` to its backup path unless a backup is already there.
fn write_backup(source: &Path) -> Result<(PathBuf, bool)> {
    let backup = backup_path(source);
    if backup.exists() {
        return Ok((backup, false));
    }
    // A name of its own, so two processes starting together do not trample
    // each other's copy; whoever renames first wins.
    let partial = backup.with_extension(format!("json.{}.partial", Uuid::new_v4().simple()));
    std::fs::copy(source, &partial)?;
    match std::fs::rename(&partial, &backup) {
        Ok(()) => Ok((backup, true)),
        Err(_) if backup.exists() => {
            let _ = std::fs::remove_file(&partial);
            Ok((backup, false))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&partial);
            Err(error.into())
        }
    }
}

impl Store {
    /// Imports the v1 configuration at `v1_config_path`.
    ///
    /// Hosts, groups, snippets, tunnels and settings are added in one
    /// transaction; records already present (by id, even if soft-deleted) are
    /// left alone, so running it again changes nothing. Launch history
    /// (`sessions`) and `transfer_history` are not migrated. v1 stored no
    /// secrets, so no secret is imported.
    pub fn migrate_from_v1(&self, v1_config_path: &Path) -> Result<MigrationReport> {
        let bytes = std::fs::read(v1_config_path)?;
        let raw: serde_json::Value = serde_json::from_slice(&bytes)?;
        match raw
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
        {
            None => {
                return Err(StoreError::Invalid(
                    "not a v1 configuration: it has no schema_version".into(),
                ))
            }
            Some(version) if version > u64::from(AppConfig::SCHEMA_VERSION) => {
                return Err(StoreError::Invalid(format!(
                    "the configuration uses schema {version}, newer than this build understands"
                )))
            }
            Some(_) => {}
        }
        let config = ConfigStore::at(v1_config_path).load()?;
        let source_sha256 = format!("{:x}", Sha256::digest(&bytes));

        let (backup, backup_created) = write_backup(v1_config_path)?;
        let mut report = MigrationReport {
            backup_path: Some(backup),
            backup_created,
            ..MigrationReport::default()
        };

        let done = self.get_setting(MIGRATION_SETTING)?;
        if done
            .as_ref()
            .and_then(|value| value.get("source_sha256"))
            .and_then(serde_json::Value::as_str)
            == Some(source_sha256.as_str())
        {
            report.already_migrated = true;
            return Ok(report);
        }

        self.in_tx(|| {
            // v1 groups may refer to parents, and later records, in any order.
            self.conn.execute_batch("PRAGMA defer_foreign_keys = ON")?;
            self.import_groups(&config, &mut report)?;
            self.import_hosts(&config, &mut report)?;
            self.import_snippets(&config, &mut report)?;
            self.import_tunnels(&config, &mut report)?;
            self.import_settings(&config, &mut report)?;
            if !config.sessions.is_empty() {
                report.skipped.push(Skipped {
                    kind: "sessions",
                    id: None,
                    reason: format!(
                        "{} terminal launch records are history, not configuration",
                        config.sessions.len()
                    ),
                });
            }
            if !config.transfer_history.is_empty() {
                report.skipped.push(Skipped {
                    kind: "transfer_history",
                    id: None,
                    reason: format!(
                        "{} transfer records are history, not configuration",
                        config.transfer_history.len()
                    ),
                });
            }
            self.set_setting(
                MIGRATION_SETTING,
                &json!({
                    "source_sha256": source_sha256,
                    "migrated_at": Utc::now().to_rfc3339(),
                    "v1_schema_version": raw["schema_version"],
                    "imported": {
                        "hosts": report.hosts.imported,
                        "groups": report.groups.imported,
                        "snippets": report.snippets.imported,
                        "tunnels": report.tunnels.imported,
                        "settings": report.settings.imported,
                    },
                }),
            )?;
            Ok(())
        })?;
        Ok(report)
    }

    fn import_groups(&self, config: &AppConfig, report: &mut MigrationReport) -> Result<()> {
        let mut seen = BTreeSet::new();
        let known: BTreeSet<String> = config.groups.iter().map(|g| store_id(&g.id)).collect();
        for group in &config.groups {
            let id = store_id(&group.id);
            if !seen.insert(id.clone()) {
                report.skipped.push(Skipped {
                    kind: "group",
                    id: Some(group.id.clone()),
                    reason: "duplicate group id in the v1 file".into(),
                });
                continue;
            }
            if self.exists_any::<easyssh_core::Group>(&id)? {
                report.groups.existing += 1;
                continue;
            }
            let mut group = group.clone();
            group.id = id.clone();
            group.parent_id = match group.parent_id.as_deref().map(store_id) {
                Some(parent) if parent != id && known.contains(&parent) => Some(parent),
                Some(_) => {
                    report.warnings.push(format!(
                        "group {:?} had a missing or self-referencing parent; imported at top level",
                        group.name
                    ));
                    None
                }
                None => None,
            };
            self.upsert_group(&group)?;
            report.groups.imported += 1;
        }
        Ok(())
    }

    fn import_hosts(&self, config: &AppConfig, report: &mut MigrationReport) -> Result<()> {
        let mut seen = BTreeSet::new();
        let groups: BTreeSet<String> = config.groups.iter().map(|g| store_id(&g.id)).collect();
        for connection in &config.connections {
            let id = store_id(&connection.id);
            if !seen.insert(id.clone()) {
                report.skipped.push(Skipped {
                    kind: "host",
                    id: Some(connection.id.clone()),
                    reason: "duplicate host id in the v1 file".into(),
                });
                continue;
            }
            if let Err(error) = validate_connection(connection) {
                report.skipped.push(Skipped {
                    kind: "host",
                    id: Some(connection.id.clone()),
                    reason: format!("failed validation: {error}"),
                });
                continue;
            }
            if self.exists_any::<Host>(&id)? {
                report.hosts.existing += 1;
                continue;
            }
            let mut connection = connection.clone();
            connection.id = id;
            connection.group_id = match connection.group_id.as_deref().map(store_id) {
                Some(group) if groups.contains(&group) => Some(group),
                Some(_) => {
                    report.warnings.push(format!(
                        "host {:?} referred to a group that does not exist; imported ungrouped",
                        connection.name
                    ));
                    None
                }
                None => None,
            };
            let updated_at = connection.updated_at;
            self.upsert_at(&Host::new(connection), updated_at)?;
            report.hosts.imported += 1;
        }
        Ok(())
    }

    fn import_snippets(&self, config: &AppConfig, report: &mut MigrationReport) -> Result<()> {
        let mut seen = BTreeSet::new();
        for snippet in &config.snippets {
            let id = store_id(&snippet.id);
            if !seen.insert(id.clone()) {
                report.skipped.push(Skipped {
                    kind: "snippet",
                    id: Some(snippet.id.clone()),
                    reason: "duplicate snippet id in the v1 file".into(),
                });
                continue;
            }
            if self.exists_any::<easyssh_core::CommandSnippet>(&id)? {
                report.snippets.existing += 1;
                continue;
            }
            let mut snippet = snippet.clone();
            snippet.id = id;
            self.upsert_snippet(&snippet)?;
            report.snippets.imported += 1;
        }
        Ok(())
    }

    fn import_tunnels(&self, config: &AppConfig, report: &mut MigrationReport) -> Result<()> {
        let mut seen = BTreeSet::new();
        let hosts: BTreeSet<String> = config.connections.iter().map(|c| store_id(&c.id)).collect();
        for tunnel in &config.tunnels {
            let id = store_id(&tunnel.id);
            if !seen.insert(id.clone()) {
                report.skipped.push(Skipped {
                    kind: "tunnel",
                    id: Some(tunnel.id.clone()),
                    reason: "duplicate tunnel id in the v1 file".into(),
                });
                continue;
            }
            if self.exists_any::<easyssh_core::Tunnel>(&id)? {
                report.tunnels.existing += 1;
                continue;
            }
            let mut tunnel = tunnel.clone();
            tunnel.id = id;
            tunnel.host_id = store_id(&tunnel.host_id);
            if !hosts.contains(&tunnel.host_id) {
                report.warnings.push(format!(
                    "tunnel {:?} goes through a host that is not in the v1 file; kept as is",
                    tunnel.name
                ));
            }
            self.upsert_tunnel(&tunnel)?;
            report.tunnels.imported += 1;
        }
        Ok(())
    }

    fn import_settings(&self, config: &AppConfig, report: &mut MigrationReport) -> Result<()> {
        let mut sidebar = serde_json::to_value(&config.sidebar)?;
        sidebar["collapsed_group_ids"] = config
            .sidebar
            .collapsed_group_ids
            .iter()
            .map(|id| store_id(id))
            .collect::<Vec<_>>()
            .into();
        let recent: Vec<String> = config
            .recent_connection_ids
            .iter()
            .map(|id| store_id(id))
            .collect();
        let entries = [
            ("theme", serde_json::to_value(config.theme)?),
            ("locale", serde_json::to_value(config.locale)?),
            (
                "display_density",
                serde_json::to_value(config.display_density)?,
            ),
            ("terminal", serde_json::to_value(&config.terminal)?),
            ("workspace", serde_json::to_value(config.workspace)?),
            ("sidebar", sidebar),
            ("sync", serde_json::to_value(&config.sync)?),
            ("experimental", serde_json::to_value(&config.experimental)?),
            ("recent_connection_ids", serde_json::to_value(recent)?),
        ];
        for (key, value) in entries {
            if self.setting_exists_any(key)? {
                report.settings.existing += 1;
            } else {
                self.set_setting(key, &value)?;
                report.settings.imported += 1;
            }
        }
        Ok(())
    }
}
