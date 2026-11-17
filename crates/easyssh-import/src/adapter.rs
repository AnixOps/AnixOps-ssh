//! Conversion of an [`ImportReport`] into EasySSH entities
//! (`easyssh_core::Connection` and `Group`, which `easyssh-store` persists
//! as `Host` and `Group` records).
//!
//! Mapping:
//!
//! | import                    | entity                                                    |
//! |---------------------------|-----------------------------------------------------------|
//! | `label`                   | `Connection.name`                                         |
//! | `hostname`/`port`/`username` | `ConnectionTarget::Endpoint`                           |
//! | `group_path`              | `Group` chain (`parent_id`), shared by prefix             |
//! | `jump`                    | `proxy_jump`, comma-joined                                |
//! | `openssh_only` non-empty  | tag `openssh-only`, reasons appended to `notes`           |
//! | `identity_file`           | appended to `notes` (the entity has no key path field)    |
//! | `notes`                   | `notes`                                                   |
//!
//! Secrets cannot appear: the import model has no field for them.

use crate::model::ImportReport;
use easyssh_core::{Connection, ConnectionTarget, Group};
use std::collections::HashMap;
use uuid::Uuid;

/// Tag put on hosts that need the OpenSSH engine.
pub const OPENSSH_ONLY_TAG: &str = "openssh-only";

/// Entities ready to be upserted by the caller. Ids are fresh random UUIDs;
/// deduplicating against existing hosts is the caller's decision.
#[derive(Debug, Clone)]
pub struct EntityBatch {
    /// Parents before children.
    pub groups: Vec<Group>,
    pub connections: Vec<Connection>,
}

impl ImportReport {
    /// Converts the report into entities. See the module docs for the mapping.
    pub fn to_entities(&self) -> EntityBatch {
        let mut ids: HashMap<Vec<String>, String> = HashMap::new();
        let mut groups = Vec::new();
        let mut paths = self.groups.clone();
        for host in &self.hosts {
            paths.push(host.group_path.clone());
        }
        paths.sort_by_key(Vec::len);
        for path in paths {
            if path.is_empty() || ids.contains_key(&path) {
                continue;
            }
            let id = Uuid::new_v4().to_string();
            let parent_id = ids.get(&path[..path.len() - 1]).cloned();
            groups.push(Group {
                id: id.clone(),
                name: path[path.len() - 1].clone(),
                color: None,
                parent_id,
            });
            ids.insert(path, id);
        }

        let connections = self
            .hosts
            .iter()
            .map(|host| {
                let mut conn = Connection::alias(&host.label, &host.hostname);
                conn.target = ConnectionTarget::Endpoint {
                    hostname: host.hostname.clone(),
                    username: host.username.clone(),
                    port: host.port,
                };
                conn.group_id = ids.get(&host.group_path).cloned();
                if !host.jump.is_empty() {
                    conn.proxy_jump = Some(host.jump.join(","));
                }
                let mut notes = Vec::new();
                if !host.notes.is_empty() {
                    notes.push(host.notes.clone());
                }
                if let Some(path) = &host.identity_file {
                    notes.push(format!("Identity file: {}", path.display()));
                }
                if !host.openssh_only.is_empty() {
                    conn.tags.push(OPENSSH_ONLY_TAG.to_owned());
                    notes.push(format!(
                        "Needs the OpenSSH engine: {}",
                        host.openssh_only.join(", ")
                    ));
                }
                conn.notes = notes.join("\n");
                conn
            })
            .collect();
        EntityBatch {
            groups,
            connections,
        }
    }
}
