//! Importing hosts from other SSH clients into the store: the one
//! implementation behind the daemon's `hosts.import_preview` /
//! `hosts.import_apply` and the app's in-process fallback.
//!
//! [`import_preview`] parses a file with `easyssh-import` and marks the hosts
//! that are already stored (same hostname, port and user). It writes nothing.
//! [`import_apply`] takes the hosts the user selected and merges them with
//! [`Store::upsert_records`], which never removes anything. Secrets cannot
//! pass through: the import model has no field for them.

use crate::error::{Result, StoreError};
use crate::store::Store;
use easyssh_core::security::{validate_alias, validate_connection};
use easyssh_core::{AppConfig, Connection, ConnectionTarget, Group};
use easyssh_import::{Format, ImportReport, ImportedHost};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};

/// The largest file accepted for an import.
pub const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
/// The most hosts one preview or apply handles (the daemon's frames are 4 MiB).
pub const MAX_IMPORT_HOSTS: usize = 5000;

/// One host as the preview shows it and `import_apply` takes it back.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ImportHost {
    pub label: String,
    pub hostname: String,
    pub port: u16,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub group_path: Vec<String>,
    #[serde(default)]
    pub identity_file: Option<String>,
    #[serde(default)]
    pub jump: Vec<String>,
    #[serde(default)]
    pub openssh_only: Vec<String>,
    #[serde(default)]
    pub notes: String,
    /// Preview only: a stored host has the same hostname, port and user.
    /// Ignored by `import_apply`, which looks again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// Preview only: why the host cannot be imported, if it cannot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

impl ImportHost {
    fn from_imported(host: &ImportedHost) -> Self {
        Self {
            label: host.label.clone(),
            hostname: host.hostname.clone(),
            port: host.port,
            username: host.username.clone(),
            group_path: host.group_path.clone(),
            identity_file: host
                .identity_file
                .as_ref()
                .map(|path| path.display().to_string()),
            jump: host.jump.clone(),
            openssh_only: host.openssh_only.clone(),
            notes: host.notes.clone(),
            exists: None,
            problem: None,
        }
    }

    fn to_imported(&self) -> ImportedHost {
        ImportedHost {
            label: self.label.clone(),
            hostname: self.hostname.clone(),
            port: self.port,
            username: self.username.clone(),
            group_path: self.group_path.clone(),
            identity_file: self.identity_file.as_ref().map(PathBuf::from),
            jump: self.jump.clone(),
            openssh_only: self.openssh_only.clone(),
            notes: self.notes.clone(),
        }
    }

    /// The connection this host becomes, without a group, and the jump hosts
    /// that could not be kept. The checks are the ones the host editor and
    /// `hosts.save` apply, so an imported host is one the app can launch.
    fn connection(&self) -> std::result::Result<(Connection, Option<String>), String> {
        let report = ImportReport {
            hosts: vec![self.to_imported()],
            ..ImportReport::default()
        };
        let mut connection = report
            .to_entities()
            .connections
            .into_iter()
            .next()
            .ok_or("no host")?;
        validate_connection(&connection).map_err(|error| error.to_string())?;
        let mut dropped = None;
        if let Some(jump) = connection.proxy_jump.clone() {
            // `-J` takes a validated alias only; anything else would be
            // refused when connecting, so it is kept in the notes instead.
            if validate_alias(&jump).is_err() {
                connection.proxy_jump = None;
                let line = format!("Jump hosts (not applied): {jump}");
                if connection.notes.is_empty() {
                    connection.notes = line;
                } else {
                    connection.notes = format!("{}\n{line}", connection.notes);
                }
                dropped = Some(jump);
            }
        }
        Ok((connection, dropped))
    }
}

/// Matching key of a stored or imported host: lower-case hostname, port, user.
type Key = (String, u16, Option<String>);

fn key_of(target: &ConnectionTarget) -> Option<Key> {
    match target {
        ConnectionTarget::Endpoint {
            hostname,
            username,
            port,
        } => Some((hostname.to_ascii_lowercase(), *port, username.clone())),
        ConnectionTarget::Alias { .. } => None,
    }
}

fn key_of_host(host: &ImportHost) -> Key {
    (
        host.hostname.to_ascii_lowercase(),
        host.port,
        host.username.clone(),
    )
}

/// What to parse and how.
#[derive(Debug, Clone, Default)]
pub struct PreviewRequest<'a> {
    pub bytes: &'a [u8],
    /// The file's name, used where the format has no session name (a lone `.xsh`).
    pub file_name: &'a str,
    /// A format id (see [`Format::id`]); detected when `None`.
    pub format: Option<&'a str>,
    /// Directory for OpenSSH `Include`; only for the user's own `~/.ssh`.
    pub base_dir: Option<&'a Path>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportPreview {
    /// The format that was parsed; `None` when nothing was recognised.
    pub format: Option<String>,
    /// What detection chose, whether or not it was overridden.
    pub detected: Option<String>,
    pub hosts: Vec<ImportHost>,
    /// Every folder path the hosts use, parents first.
    pub groups: Vec<Vec<String>>,
    pub warnings: Vec<String>,
    pub skipped: usize,
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::Invalid(message.into())
}

/// Reads an import file the user picked, refusing one over [`MAX_IMPORT_BYTES`]
/// before it is read into memory.
pub fn read_import_file(path: &Path) -> Result<Vec<u8>> {
    let size = std::fs::metadata(path)
        .map_err(|error| invalid(format!("cannot read {}: {error}", path.display())))?
        .len();
    check_size(size)?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_IMPORT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    check_size(bytes.len() as u64)?;
    Ok(bytes)
}

fn check_size(size: u64) -> Result<()> {
    if size > MAX_IMPORT_BYTES as u64 {
        return Err(invalid(format!(
            "the file is {size} bytes, over the limit of {} MiB ({MAX_IMPORT_BYTES} bytes) for an import",
            MAX_IMPORT_BYTES >> 20
        )));
    }
    Ok(())
}

/// Parses `request` and marks the hosts that are stored already. Writes nothing.
pub fn import_preview(store: &Store, request: &PreviewRequest) -> Result<ImportPreview> {
    check_size(request.bytes.len() as u64)?;
    let override_format = match request.format {
        Some(id) => {
            Some(Format::from_id(id).ok_or_else(|| invalid(format!("unknown format {id}")))?)
        }
        None => None,
    };
    let detected = easyssh_import::detect_format(request.bytes);
    let format = override_format.or(detected);
    let report = match format {
        Some(Format::Xshell) if !request.bytes.starts_with(b"PK") => {
            easyssh_import::xshell::parse_named(request.file_name, request.bytes)
        }
        Some(format) => easyssh_import::import_as(format, request.bytes, request.base_dir),
        None => easyssh_import::import_auto(request.bytes),
    };
    if report.hosts.len() > MAX_IMPORT_HOSTS {
        return Err(invalid(format!(
            "the file holds {} hosts; at most {MAX_IMPORT_HOSTS} can be imported at once, split it",
            report.hosts.len()
        )));
    }

    let stored: BTreeSet<Key> = store
        .list_hosts()?
        .iter()
        .filter_map(|record| key_of(&record.item.connection.target))
        .collect();
    let hosts = report
        .hosts
        .iter()
        .map(|imported| {
            let mut host = ImportHost::from_imported(imported);
            host.exists = Some(stored.contains(&key_of_host(&host)));
            host.problem = host.connection().err();
            host
        })
        .collect();
    Ok(ImportPreview {
        format: format.map(|format| format.id().to_owned()),
        detected: detected.map(|format| format.id().to_owned()),
        hosts,
        groups: report.groups.clone(),
        warnings: report.warnings.iter().map(ToString::to_string).collect(),
        skipped: report.skipped,
    })
}

/// What to do with an imported host that matches a stored one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DuplicatePolicy {
    /// Leave the stored host as it is.
    Skip,
    /// Bring the stored host's name, group, jump host, tags and notes up to
    /// date; its id, favorite, colour and everything else stay.
    Update,
    /// Add the imported host next to the stored one.
    KeepBoth,
}

/// Where the imported hosts go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupMode {
    /// The folders of the file, reusing stored groups of the same name.
    Source,
    /// No group for new hosts; stored hosts keep theirs.
    None,
    /// Every host goes into `group_id`.
    Existing,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyRequest {
    pub hosts: Vec<ImportHost>,
    pub group_mode: GroupMode,
    #[serde(default)]
    pub group_id: Option<String>,
    pub duplicates: DuplicatePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ApplyOutcome {
    pub created: usize,
    pub updated: usize,
    /// Matches left alone: skipped by policy, or updates that changed nothing.
    pub unchanged: usize,
    pub groups_created: usize,
    /// Hosts that could not be imported, with the reason.
    pub rejected: Vec<Rejected>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejected {
    pub label: String,
    pub reason: String,
}

impl ApplyOutcome {
    /// Whether anything was written (and so whether to announce a change).
    pub fn changed(&self) -> bool {
        self.created + self.updated + self.groups_created > 0
    }
}

/// Merges the selected hosts into the store in one transaction. Nothing is
/// removed: stored hosts and groups that the import does not mention stay.
pub fn import_apply(store: &Store, request: &ApplyRequest) -> Result<ApplyOutcome> {
    if request.hosts.len() > MAX_IMPORT_HOSTS {
        return Err(invalid(format!(
            "{} hosts selected; at most {MAX_IMPORT_HOSTS} can be imported at once",
            request.hosts.len()
        )));
    }
    let mut groups: Vec<Group> = store.list_groups()?.into_iter().map(|r| r.item).collect();
    let mut stored: BTreeMap<String, Connection> = BTreeMap::new();
    let mut by_key: HashMap<Key, String> = HashMap::new();
    for record in store.list_hosts()? {
        let connection = record.item.connection;
        if let Some(key) = key_of(&connection.target) {
            by_key.entry(key).or_insert_with(|| connection.id.clone());
        }
        stored.insert(connection.id.clone(), connection);
    }

    let target = match request.group_mode {
        GroupMode::Existing => {
            let id = request
                .group_id
                .as_deref()
                .ok_or_else(|| invalid("a target group is required"))?;
            if !groups.iter().any(|group| group.id == id) {
                return Err(invalid("the target group does not exist"));
            }
            Some(id.to_owned())
        }
        _ => None,
    };

    let mut outcome = ApplyOutcome::default();
    let known_groups = groups.len();
    let mut touched: BTreeSet<String> = BTreeSet::new();
    let mut created: Vec<Connection> = Vec::new();
    for host in &request.hosts {
        let (mut connection, dropped) = match host.connection() {
            Ok(built) => built,
            Err(reason) => {
                outcome.rejected.push(Rejected {
                    label: host.label.clone(),
                    reason,
                });
                continue;
            }
        };
        if dropped.is_some() {
            outcome.warnings.push(format!(
                "{}: jump hosts were put in the notes, not applied (only a plain alias can be used)",
                host.label
            ));
        }
        let group_id = match request.group_mode {
            GroupMode::Source => group_for_path(&mut groups, &host.group_path),
            GroupMode::None => None,
            GroupMode::Existing => target.clone(),
        };
        let key = key_of_host(host);
        let existing = match request.duplicates {
            DuplicatePolicy::KeepBoth => None,
            _ => by_key.get(&key).cloned(),
        };
        match (existing, request.duplicates) {
            (Some(_), DuplicatePolicy::Skip) => outcome.unchanged += 1,
            (Some(id), _) => {
                let current = stored.get_mut(&id).expect("indexed hosts are stored");
                let before = current.clone();
                current.name = connection.name;
                if request.group_mode != GroupMode::None {
                    current.group_id = group_id;
                }
                if connection.proxy_jump.is_some() {
                    current.proxy_jump = connection.proxy_jump;
                }
                for tag in connection.tags {
                    if !current.tags.contains(&tag) {
                        current.tags.push(tag);
                    }
                }
                if !connection.notes.is_empty() {
                    current.notes = connection.notes;
                }
                if *current == before {
                    outcome.unchanged += 1;
                } else {
                    current.updated_at = chrono::Utc::now();
                    touched.insert(id);
                    outcome.updated += 1;
                }
            }
            (None, _) => {
                connection.group_id = group_id;
                // A later host with the same key matches this one unless the
                // user asked to keep both.
                if request.duplicates != DuplicatePolicy::KeepBoth {
                    if let Some(key) = key_of(&connection.target) {
                        by_key.entry(key).or_insert_with(|| connection.id.clone());
                    }
                }
                stored.insert(connection.id.clone(), connection.clone());
                created.push(connection);
                outcome.created += 1;
            }
        }
    }
    outcome.groups_created = groups.len() - known_groups;

    let mut config = AppConfig::new();
    config.groups = groups;
    config.connections = touched
        .iter()
        .filter_map(|id| stored.get(id).cloned())
        .chain(created)
        .collect();
    // Only what changed is written, and `upsert_records` removes nothing; the
    // groups are all listed so that references to them survive.
    store.upsert_records(&config)?;
    Ok(outcome)
}

/// The id of the group at `path` (outermost first), creating the missing
/// links. A stored group of the same name under the same parent is reused,
/// ignoring case. An empty path is no group.
fn group_for_path(groups: &mut Vec<Group>, path: &[String]) -> Option<String> {
    let mut parent: Option<String> = None;
    for name in path {
        let found = groups
            .iter()
            .find(|group| group.parent_id == parent && group.name.eq_ignore_ascii_case(name));
        let id = match found {
            Some(group) => group.id.clone(),
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                groups.push(Group {
                    id: id.clone(),
                    name: name.clone(),
                    color: None,
                    parent_id: parent.clone(),
                });
                id
            }
        };
        parent = Some(id);
    }
    parent
}
