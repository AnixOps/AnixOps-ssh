use crate::*;
use easyssh_core::{CommandSnippet, Connection, ConnectionTarget, Group, Tunnel, TunnelKind};
use std::path::Path;
use std::sync::Arc;

const DEFAULT_FIXTURE: &str = include_str!("../fixtures/v1_default.json");
const FULL_FIXTURE: &str = include_str!("../fixtures/v1_full.json");
const OLD_FIXTURE: &str = include_str!("../fixtures/v1_old.json");

fn store() -> Store {
    Store::open_in_memory("dev-a", Arc::new(PlaintextForTests)).unwrap()
}

fn locked_store() -> Store {
    Store::open_in_memory("dev-a", Arc::new(NotConfigured)).unwrap()
}

fn file_store(path: &Path, device: &str) -> Store {
    Store::open(path, device, Arc::new(PlaintextForTests)).unwrap()
}

fn host(name: &str) -> Host {
    Host::new(Connection::alias(name, name.to_lowercase()))
}

fn count(store: &Store, table: &str) -> i64 {
    store
        .conn
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---- migrations ------------------------------------------------------------

#[test]
fn migrations_apply_from_empty() {
    let store = store();
    assert_eq!(store.schema_version().unwrap(), latest_version());
    for table in [
        "hosts",
        "groups",
        "tags",
        "host_tags",
        "snippets",
        "tunnels",
        "known_hosts",
        "jump_chains",
        "jump_chain_hops",
        "settings",
        "vaults",
        "record_history",
        "schema_migrations",
    ] {
        let found: i64 = store
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(found, 1, "missing table {table}");
    }
    let vaults = store.list_vaults().unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].id, PERSONAL_VAULT_ID);
    assert_eq!(vaults[0].kind, VaultKind::Personal);
}

#[test]
fn migrations_are_idempotent_across_opens() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    {
        let store = file_store(&path, "dev-a");
        store.upsert_host(&host("Web")).unwrap();
    }
    let store = file_store(&path, "dev-a");
    assert_eq!(count(&store, "schema_migrations"), latest_version());
    assert_eq!(count(&store, "vaults"), 1);
    assert_eq!(store.list_hosts().unwrap().len(), 1);
}

#[test]
fn file_database_uses_wal_foreign_keys_and_full_sync() {
    let dir = tempfile::tempdir().unwrap();
    let store = file_store(&dir.path().join("store.db"), "dev-a");
    let mode: String = store
        .conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let foreign_keys: i64 = store
        .conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    let synchronous: i64 = store
        .conn
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(foreign_keys, 1);
    assert_eq!(synchronous, 2, "FULL");
}

#[test]
fn newer_database_is_refused_and_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    drop(file_store(&path, "dev-a"));
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (99, 'future', 'x')",
            [],
        )
        .unwrap();
    }
    let error = Store::open(&path, "dev-a", Arc::new(PlaintextForTests)).unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::DatabaseTooNew {
                found: 99,
                supported
            } if supported == latest_version()
        ),
        "{error:?}"
    );
    let conn = rusqlite::Connection::open(&path).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(rows, latest_version() + 1);
}

#[test]
fn foreign_keys_are_enforced() {
    let store = store();
    let mut h = host("Web");
    h.connection.group_id = Some(new_id());
    assert!(matches!(store.upsert_host(&h), Err(StoreError::Sqlite(_))));
    assert!(store.list_hosts().unwrap().is_empty(), "rolled back");
}

// ---- CRUD --------------------------------------------------------------------

#[test]
fn host_crud_roundtrips_every_field() {
    let store = store();
    let group = Group {
        id: new_id(),
        name: "Prod".into(),
        color: Some("#fff".into()),
        parent_id: None,
    };
    store.upsert_group(&group).unwrap();
    let mut connection = Connection::alias("DB", "db");
    connection.target = ConnectionTarget::Endpoint {
        hostname: "db.example".into(),
        username: Some("ops".into()),
        port: 2222,
    };
    connection.group_id = Some(group.id.clone());
    connection.tags = vec!["prod".into(), " db ".into(), "prod".into(), "".into()];
    connection.favorite = true;
    connection.color = Some("#123456".into());
    connection.notes = "plain note".into();
    connection.proxy_jump = Some("bastion".into());
    connection.local_forwards = vec!["1:localhost:1".into()];
    connection.remote_forwards = vec!["2:localhost:2".into()];
    connection.dynamic_forwards = vec!["1080".into()];
    connection.remote_command = Some("tmux".into());
    connection.local_directories = vec!["/a".into()];
    connection.remote_directories = vec!["/b".into(), "/c".into()];
    connection.terminal.font_size = 17.5;
    connection.terminal.scrollback_lines = 123;
    connection.terminal.startup_command = Some("ls".into());
    let saved = store.upsert_host(&Host::new(connection.clone())).unwrap();
    let loaded = store.get_host(&connection.id).unwrap().unwrap();
    assert_eq!(saved, loaded);
    let mut expected = connection;
    expected.tags = vec!["db".into(), "prod".into()];
    expected.updated_at = loaded.meta.updated_at;
    expected.created_at = loaded.item.connection.created_at; // stored at microseconds
    assert_eq!(loaded.item.connection, expected);
    assert_eq!(loaded.meta.vault_id, PERSONAL_VAULT_ID);
    assert!(!loaded.item.notes_secret);
    assert_eq!(store.list_hosts().unwrap().len(), 1);
    let tag_names: Vec<String> = store
        .list_tags()
        .unwrap()
        .into_iter()
        .map(|record| record.item.name)
        .collect();
    assert_eq!(tag_names, ["db", "prod"]);
}

#[test]
fn host_update_changes_tags_and_keeps_created_at() {
    let store = store();
    let mut h = host("Web");
    h.connection.tags = vec!["a".into(), "b".into()];
    let first = store.upsert_host(&h).unwrap();
    let mut changed = first.item.clone();
    changed.connection.tags = vec!["b".into(), "c".into()];
    changed.connection.created_at = chrono::DateTime::UNIX_EPOCH;
    changed.connection.name = "Web 2".into();
    let second = store.upsert_host(&changed).unwrap();
    assert_eq!(second.item.connection.tags, ["b", "c"]);
    assert_eq!(
        second.item.connection.created_at,
        first.item.connection.created_at
    );
    assert_eq!(
        store
            .get_host(&h.connection.id)
            .unwrap()
            .unwrap()
            .item
            .connection
            .name,
        "Web 2"
    );
    assert_eq!(count(&store, "host_tags"), 2);
}

#[test]
fn host_id_must_be_a_uuid() {
    let store = store();
    let mut h = host("Web");
    h.connection.id = "not-a-uuid".into();
    assert!(matches!(store.upsert_host(&h), Err(StoreError::Invalid(_))));
}

#[test]
fn tombstones_hide_rows_and_restore_brings_them_back() {
    let store = store();
    let saved = store.upsert_host(&host("Web")).unwrap();
    let id = saved.meta.id.clone();
    assert!(store.soft_delete_host(&id).unwrap());
    assert!(store.get_host(&id).unwrap().is_none());
    assert!(store.list_hosts().unwrap().is_empty());
    let deleted = store.list_deleted_hosts().unwrap();
    assert_eq!(deleted.len(), 1);
    assert!(deleted[0].meta.deleted);
    assert_eq!(count(&store, "hosts"), 1, "row is kept as a tombstone");
    // Saving a tombstoned record is an error, not a silent resurrection.
    assert!(matches!(
        store.upsert_host(&saved.item),
        Err(StoreError::Deleted { .. })
    ));
    assert!(store.restore_host(&id).unwrap());
    let restored = store.get_host(&id).unwrap().unwrap().item;
    assert_eq!(restored.connection.name, saved.item.connection.name);
    assert_eq!(restored.connection.id, saved.item.connection.id);
    assert!(store.list_deleted_hosts().unwrap().is_empty());
    assert!(!store.soft_delete_host(&new_id()).unwrap());
    assert!(!store.restore_host(&new_id()).unwrap());
}

#[test]
fn deleting_twice_does_not_bump_again() {
    let store = store();
    let id = store.upsert_host(&host("Web")).unwrap().meta.id;
    store.soft_delete_host(&id).unwrap();
    let after_first = store.list_deleted_hosts().unwrap()[0].meta.clone();
    store.soft_delete_host(&id).unwrap();
    assert_eq!(store.list_deleted_hosts().unwrap()[0].meta, after_first);
}

#[test]
fn group_crud() {
    let store = store();
    let parent = Group {
        id: new_id(),
        name: "Parent".into(),
        color: None,
        parent_id: None,
    };
    let child = Group {
        id: new_id(),
        name: "Child".into(),
        color: Some("red".into()),
        parent_id: Some(parent.id.clone()),
    };
    store.upsert_group(&parent).unwrap();
    store.upsert_group(&child).unwrap();
    assert_eq!(
        format!("{:?}", store.get_group(&child.id).unwrap().unwrap().item),
        format!("{child:?}")
    );
    let renamed = Group {
        name: "Kid".into(),
        ..child.clone()
    };
    store.upsert_group(&renamed).unwrap();
    assert_eq!(
        store.get_group(&child.id).unwrap().unwrap().item.name,
        "Kid"
    );
    assert_eq!(store.list_groups().unwrap().len(), 2);
    store.soft_delete_group(&child.id).unwrap();
    assert_eq!(store.list_groups().unwrap().len(), 1);
    assert_eq!(store.list_deleted_groups().unwrap().len(), 1);
    store.restore_group(&child.id).unwrap();
    assert_eq!(store.list_groups().unwrap().len(), 2);
}

#[test]
fn snippet_crud() {
    let store = store();
    let snippet = CommandSnippet {
        id: new_id(),
        name: "Disk".into(),
        content: "df -h".into(),
    };
    store.upsert_snippet(&snippet).unwrap();
    assert_eq!(
        format!(
            "{:?}",
            store.get_snippet(&snippet.id).unwrap().unwrap().item
        ),
        format!("{snippet:?}")
    );
    store
        .upsert_snippet(&CommandSnippet {
            content: "df -hT".into(),
            ..snippet.clone()
        })
        .unwrap();
    assert_eq!(
        store
            .get_snippet(&snippet.id)
            .unwrap()
            .unwrap()
            .item
            .content,
        "df -hT"
    );
    store.soft_delete_snippet(&snippet.id).unwrap();
    assert!(store.list_snippets().unwrap().is_empty());
    store.restore_snippet(&snippet.id).unwrap();
    assert_eq!(store.list_snippets().unwrap().len(), 1);
}

#[test]
fn tunnel_crud() {
    let store = store();
    let host_id = new_id();
    let tunnel = Tunnel {
        id: new_id(),
        name: "DB".into(),
        host_id: host_id.clone(),
        kind: TunnelKind::Local,
        bind_address: Some("127.0.0.1".into()),
        bind_port: 5432,
        target_host: Some("localhost".into()),
        target_port: Some(5432),
    };
    let socks = Tunnel {
        id: new_id(),
        name: "SOCKS".into(),
        kind: TunnelKind::Dynamic,
        bind_address: None,
        bind_port: 1080,
        target_host: None,
        target_port: None,
        ..tunnel.clone()
    };
    store.upsert_tunnel(&tunnel).unwrap();
    store.upsert_tunnel(&socks).unwrap();
    assert_eq!(store.get_tunnel(&tunnel.id).unwrap().unwrap().item, tunnel);
    assert_eq!(store.get_tunnel(&socks.id).unwrap().unwrap().item, socks);
    assert_eq!(store.tunnels_for_host(&host_id).unwrap().len(), 2);
    store.soft_delete_tunnel(&tunnel.id).unwrap();
    assert_eq!(store.tunnels_for_host(&host_id).unwrap().len(), 1);
    store.restore_tunnel(&tunnel.id).unwrap();
    assert_eq!(store.list_tunnels().unwrap().len(), 2);
}

#[test]
fn tag_crud_and_ensure() {
    let store = store();
    let id = store.ensure_tag(" prod ").unwrap();
    assert_eq!(store.ensure_tag("prod").unwrap(), id);
    assert_eq!(store.get_tag(&id).unwrap().unwrap().item.name, "prod");
    store
        .upsert_tag(&Tag {
            id: id.clone(),
            name: "production".into(),
        })
        .unwrap();
    assert_eq!(store.list_tags().unwrap()[0].item.name, "production");
    store.soft_delete_tag(&id).unwrap();
    assert!(store.list_tags().unwrap().is_empty());
    // Ensuring a deleted tag's name brings the same tag back.
    assert_eq!(store.ensure_tag("production").unwrap(), id);
    assert_eq!(store.list_tags().unwrap().len(), 1);
    assert!(matches!(
        store.ensure_tag("  "),
        Err(StoreError::Invalid(_))
    ));
}

#[test]
fn deleted_tags_are_not_listed_on_hosts() {
    let store = store();
    let mut h = host("Web");
    h.connection.tags = vec!["a".into(), "b".into()];
    let saved = store.upsert_host(&h).unwrap();
    let tag = store.ensure_tag("a").unwrap();
    store.soft_delete_tag(&tag).unwrap();
    let loaded = store.get_host(&saved.meta.id).unwrap().unwrap();
    assert_eq!(loaded.item.connection.tags, ["b"]);
}

#[test]
fn jump_chain_crud_and_validation() {
    let store = store();
    let target = store.upsert_host(&host("Target")).unwrap().meta.id;
    let hop1 = store.upsert_host(&host("Hop1")).unwrap().meta.id;
    let hop2 = store.upsert_host(&host("Hop2")).unwrap().meta.id;
    let chain = JumpChain {
        id: new_id(),
        host_id: target.clone(),
        hops: vec![hop2.clone(), hop1.clone()],
    };
    store.upsert_jump_chain(&chain).unwrap();
    assert_eq!(
        store.get_jump_chain(&chain.id).unwrap().unwrap().item,
        chain
    );
    assert_eq!(
        store
            .jump_chain_for_host(&target)
            .unwrap()
            .unwrap()
            .item
            .hops,
        [hop2.clone(), hop1.clone()]
    );
    let reordered = JumpChain {
        hops: vec![hop1.clone(), hop2.clone()],
        ..chain.clone()
    };
    store.upsert_jump_chain(&reordered).unwrap();
    assert_eq!(
        store.get_jump_chain(&chain.id).unwrap().unwrap().item.hops,
        [hop1.clone(), hop2.clone()]
    );
    let looping = JumpChain {
        hops: vec![target.clone()],
        ..chain.clone()
    };
    assert!(matches!(
        store.upsert_jump_chain(&looping),
        Err(StoreError::Invalid(_))
    ));
    let repeated = JumpChain {
        hops: vec![hop1.clone(), hop1.clone()],
        ..chain.clone()
    };
    assert!(matches!(
        store.upsert_jump_chain(&repeated),
        Err(StoreError::Invalid(_))
    ));
    let unknown = JumpChain {
        hops: vec![new_id()],
        ..chain.clone()
    };
    assert!(
        store.upsert_jump_chain(&unknown).is_err(),
        "hop must be a host"
    );
    assert_eq!(
        store.get_jump_chain(&chain.id).unwrap().unwrap().item.hops,
        [hop1, hop2],
        "failed upsert rolled back"
    );
    store.soft_delete_jump_chain(&chain.id).unwrap();
    assert!(store.jump_chain_for_host(&target).unwrap().is_none());
    store.restore_jump_chain(&chain.id).unwrap();
    assert!(store.jump_chain_for_host(&target).unwrap().is_some());
}

#[test]
fn settings_crud_and_history() {
    let store = store();
    assert!(store.get_setting("theme").unwrap().is_none());
    store
        .set_setting("theme", &serde_json::json!("dark"))
        .unwrap();
    store.set_setting_as("font", &14.5).unwrap();
    assert_eq!(
        store.get_setting_as::<String>("theme").unwrap().unwrap(),
        "dark"
    );
    store
        .set_setting("theme", &serde_json::json!("light"))
        .unwrap();
    assert_eq!(store.list_settings().unwrap().len(), 2);
    let history = store.history("theme").unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].old_value, Some(serde_json::json!("dark")));
    assert!(store.delete_setting("theme").unwrap());
    assert!(store.get_setting("theme").unwrap().is_none());
    assert!(!store.delete_setting("nope").unwrap());
    store
        .set_setting("theme", &serde_json::json!("system"))
        .unwrap();
    assert_eq!(
        store.get_setting_as::<String>("theme").unwrap().unwrap(),
        "system"
    );
    assert!(store.set_setting("", &serde_json::json!(1)).is_err());
}

#[test]
fn vaults_are_a_placeholder_table() {
    let mut store = store();
    let team = store.create_vault("Ops", VaultKind::Team).unwrap();
    assert_eq!(store.list_vaults().unwrap().len(), 2);
    store.set_active_vault(&team.id).unwrap();
    let saved = store.upsert_host(&host("Web")).unwrap();
    assert_eq!(saved.meta.vault_id, team.id);
    assert!(store.set_active_vault(&new_id()).is_err());
}

// ---- known hosts --------------------------------------------------------------

const ED25519: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";
const RSA: &str = "AAAAB3NzaC1yc2EAAAADAQABAAABAQC7vbqajDhA";

#[test]
fn fingerprint_matches_sha256_of_key() {
    // SHA-256("abc"), base64 without padding.
    assert_eq!(
        fingerprint_of(b"abc"),
        "SHA256:ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0"
    );
}

#[test]
fn known_host_crud_and_lookup() {
    let store = store();
    let entry = KnownHost::new("example.com", 22, "ssh-ed25519", vec![1, 2, 3]);
    let other_port = KnownHost::new("example.com", 2222, "ssh-ed25519", vec![4, 5, 6]);
    assert_eq!(other_port.host, "[example.com]:2222");
    store.upsert_known_host(&entry).unwrap();
    store.upsert_known_host(&other_port).unwrap();
    let loaded = store.get_known_host(&other_port.id).unwrap().unwrap().item;
    assert_eq!(loaded.port, 2222);
    assert_eq!(loaded.fingerprint, fingerprint_of(&[4, 5, 6]));
    assert_eq!(loaded.key_blob, vec![4, 5, 6]);
    assert_eq!(store.find_known_hosts("EXAMPLE.com", 22).unwrap().len(), 1);
    assert_eq!(
        store.find_known_hosts("example.com", 2222).unwrap().len(),
        1
    );
    assert!(store
        .find_known_hosts("example.com", 23)
        .unwrap()
        .is_empty());
    store.soft_delete_known_host(&entry.id).unwrap();
    assert!(store
        .find_known_hosts("example.com", 22)
        .unwrap()
        .is_empty());
    store.restore_known_host(&entry.id).unwrap();
    assert_eq!(store.list_known_hosts().unwrap().len(), 2);
    let invalid = KnownHost::new("", 22, "ssh-ed25519", vec![1]);
    assert!(store.upsert_known_host(&invalid).is_err());
}

#[test]
fn known_hosts_import_keeps_plain_and_hashed_entries_as_written() {
    let store = store();
    let hashed = "|1|F1E1KeoE/eEWhi10WpGv4OdiO6Y=|3988QV0VE8wmZL7suNrYQLITLCg=";
    let text = format!(
        "# a comment\n\
         \n\
         example.com ssh-ed25519 {ED25519} me@laptop\n\
         [example.com]:2222 ssh-rsa {RSA}\n\
         a.example,b.example,10.0.0.1 ssh-ed25519 {ED25519}\n\
         {hashed} ssh-ed25519 {ED25519}\n\
         @cert-authority *.corp ssh-ed25519 {ED25519} corp CA\n\
         @revoked old.example ssh-rsa {RSA}\n\
         example.com ssh-ed25519 {ED25519} me@laptop\n\
         broken.example ssh-ed25519\n\
         bad64.example ssh-ed25519 !!!notbase64!!!\n\
         @weird host ssh-ed25519 {ED25519}\n"
    );
    let report = store.import_known_hosts(&text).unwrap();
    assert_eq!(report.imported, 6);
    assert_eq!(report.duplicates, 1);
    assert_eq!(
        report.skipped.iter().map(|s| s.line).collect::<Vec<_>>(),
        [10, 11, 12]
    );
    let all = store.list_known_hosts().unwrap();
    let by_host = |host: &str| {
        all.iter()
            .find(|r| r.item.host == host)
            .unwrap()
            .item
            .clone()
    };
    let plain = by_host("example.com");
    assert_eq!(plain.comment.as_deref(), Some("me@laptop"));
    assert_eq!(plain.port, 22);
    assert!(!plain.hashed);
    assert_eq!(plain.key_type, "ssh-ed25519");
    assert!(plain.fingerprint.starts_with("SHA256:"));
    assert_eq!(by_host("[example.com]:2222").port, 2222);
    let hashed_entry = by_host(hashed);
    assert!(hashed_entry.hashed);
    assert_eq!(hashed_entry.host, hashed, "kept verbatim");
    assert!(store.find_known_hosts("a.example", 22).unwrap().len() == 1);
    assert!(store.find_known_hosts("10.0.0.1", 22).unwrap().len() == 1);
    assert_eq!(by_host("*.corp").marker.as_deref(), Some("@cert-authority"));
    assert_eq!(by_host("*.corp").comment.as_deref(), Some("corp CA"));
    // Re-importing is a no-op.
    let again = store.import_known_hosts(&text).unwrap();
    assert_eq!((again.imported, again.duplicates), (0, 7));
    assert_eq!(count(&store, "known_hosts"), 6);
}

#[test]
fn known_hosts_file_import_reads_an_explicit_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("known_hosts");
    std::fs::write(&path, format!("host.example ssh-ed25519 {ED25519}\n")).unwrap();
    let store = store();
    assert_eq!(store.import_known_hosts_file(&path).unwrap().imported, 1);
    assert!(store
        .import_known_hosts_file(&dir.path().join("missing"))
        .is_err());
}

// ---- bump and history ---------------------------------------------------------

#[test]
fn upsert_stamps_fields_and_vector() {
    let store = store();
    let created = store.upsert_host(&host("Web")).unwrap();
    assert_eq!(created.meta.version_vector["dev-a"], 1);
    assert!(created
        .meta
        .field_stamps
        .values()
        .all(|s| s.lamport == 1 && s.device == "dev-a"));
    assert!(
        store.history(&created.meta.id).unwrap().is_empty(),
        "nothing overwritten yet"
    );

    let mut edited = created.item.clone();
    edited.connection.name = "Web 2".into();
    edited.connection.favorite = true;
    let updated = store.upsert_host(&edited).unwrap();
    assert_eq!(updated.meta.version_vector["dev-a"], 2);
    assert_eq!(updated.meta.field_stamps["name"].lamport, 2);
    assert_eq!(updated.meta.field_stamps["favorite"].lamport, 2);
    assert_eq!(
        updated.meta.field_stamps["color"].lamport, 1,
        "untouched field keeps its stamp"
    );
    assert!(updated.meta.updated_at >= created.meta.updated_at);

    let history = store.history(&created.meta.id).unwrap();
    let mut changed: Vec<_> = history.iter().map(|h| h.field.as_str()).collect();
    changed.sort_unstable();
    assert_eq!(changed, ["favorite", "name"]);
    let name = history.iter().find(|h| h.field == "name").unwrap();
    assert_eq!(name.old_value, Some(serde_json::json!("Web")));
    assert_eq!(name.device, "dev-a");
    assert_eq!(name.record_table, "hosts");
}

#[test]
fn unchanged_upsert_writes_nothing() {
    let store = store();
    let created = store.upsert_host(&host("Web")).unwrap();
    let again = store.upsert_host(&created.item).unwrap();
    assert_eq!(again.meta, created.meta);
    assert_eq!(count(&store, "record_history"), 0);
}

#[test]
fn another_device_gets_its_own_vector_entry_and_lamport_goes_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let first = file_store(&path, "dev-a")
        .upsert_host(&host("Web"))
        .unwrap();
    let store_b = file_store(&path, "dev-b");
    let mut edited = first.item.clone();
    edited.connection.name = "Renamed".into();
    let second = store_b.upsert_host(&edited).unwrap();
    assert_eq!(second.meta.version_vector["dev-a"], 1);
    assert_eq!(second.meta.version_vector["dev-b"], 1);
    assert_eq!(second.meta.field_stamps["name"].device, "dev-b");
    assert_eq!(second.meta.field_stamps["name"].lamport, 2);
    assert_eq!(store_b.history(&first.meta.id).unwrap()[0].device, "dev-b");
}

#[test]
fn soft_delete_and_restore_are_stamped_and_recorded() {
    let store = store();
    let id = store.upsert_host(&host("Web")).unwrap().meta.id;
    store.soft_delete_host(&id).unwrap();
    store.restore_host(&id).unwrap();
    let meta = store.get_host(&id).unwrap().unwrap().meta;
    assert_eq!(meta.version_vector["dev-a"], 3);
    assert_eq!(meta.field_stamps["deleted"].lamport, 3);
    let history = store.history(&id).unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].old_value, Some(serde_json::json!(true)));
    assert_eq!(history[1].old_value, Some(serde_json::json!(false)));
}

#[test]
fn public_bump_helper_stamps_and_writes_history() {
    let store = store();
    let mut meta = Meta::new("rec-1", PERSONAL_VAULT_ID, chrono::Utc::now());
    let changes = [
        FieldChange {
            field: "name",
            old: Some(serde_json::json!("old")),
            secret: false,
        },
        FieldChange {
            field: "color",
            old: None,
            secret: false,
        },
    ];
    store
        .bump("hosts", &mut meta, &changes, chrono::Utc::now())
        .unwrap();
    assert_eq!(meta.version_vector["dev-a"], 1);
    assert_eq!(meta.field_stamps.len(), 2);
    let history = store.history("rec-1").unwrap();
    assert_eq!(
        history.len(),
        1,
        "a field with no old value has nothing to restore"
    );
    assert_eq!(history[0].field, "name");
}

// ---- secrets -------------------------------------------------------------------

fn with_password(name: &str, password: &str) -> Host {
    let mut h = host(name);
    h.secrets = Some(HostSecrets {
        password: Some(password.into()),
        ..HostSecrets::default()
    });
    h
}

#[test]
fn not_configured_refuses_to_store_secrets() {
    let store = locked_store();
    for secrets in [
        HostSecrets {
            password: Some("hunter2".into()),
            ..HostSecrets::default()
        },
        HostSecrets {
            private_key: Some("-----BEGIN OPENSSH PRIVATE KEY-----".into()),
            ..HostSecrets::default()
        },
        HostSecrets {
            passphrase: Some("correct horse".into()),
            ..HostSecrets::default()
        },
    ] {
        let mut h = host("Web");
        h.secrets = Some(secrets);
        let error = store.upsert_host(&h).unwrap_err();
        assert!(
            matches!(error, StoreError::Cipher(CryptoError::NotConfigured)),
            "{error:?}"
        );
    }
    let mut secret_note = host("Web");
    secret_note.notes_secret = true;
    secret_note.connection.notes = "the vault code is 1234".into();
    assert!(matches!(
        store.upsert_host(&secret_note),
        Err(StoreError::Cipher(CryptoError::NotConfigured))
    ));
    assert_eq!(count(&store, "hosts"), 0, "nothing was written");
    assert_eq!(count(&store, "record_history"), 0);
}

#[test]
fn not_configured_still_stores_hosts_without_secrets() {
    let store = locked_store();
    let mut h = host("Web");
    h.secrets = Some(HostSecrets {
        password: Some(String::new()),
        ..HostSecrets::default()
    });
    h.notes_secret = true; // empty secret note needs no encryption
    let saved = store.upsert_host(&h).unwrap();
    let loaded = store.get_host(&saved.meta.id).unwrap().unwrap();
    assert_eq!(loaded.item.secrets, Some(HostSecrets::default()));
}

#[test]
fn secrets_are_encrypted_at_rest_and_roundtrip() {
    let store = store();
    let mut h = host("Web");
    h.notes_secret = true;
    h.connection.notes = "the vault code is 1234".into();
    h.secrets = Some(HostSecrets {
        password: Some("hunter2".into()),
        private_key: Some("KEYDATA".into()),
        passphrase: Some("pass".into()),
    });
    let saved = store.upsert_host(&h).unwrap();
    let loaded = store.get_host(&saved.meta.id).unwrap().unwrap().item;
    assert_eq!(loaded.secrets, h.secrets);
    assert_eq!(loaded.connection.notes, "the vault code is 1234");
    assert!(loaded.notes_secret);

    let (notes, password_enc, key_enc): (String, Vec<u8>, Vec<u8>) = store
        .conn
        .query_row(
            "SELECT notes, password_enc, private_key_enc FROM hosts WHERE id = ?1",
            [&saved.meta.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(notes, "", "a secret note never reaches the plain column");
    assert!(!password_enc.is_empty() && !key_enc.is_empty());
}

#[test]
fn omitted_secrets_are_kept_on_update() {
    let store = store();
    let saved = store.upsert_host(&with_password("Web", "hunter2")).unwrap();
    let mut edit = Host::new(saved.item.connection.clone());
    edit.connection.name = "Web 2".into();
    assert!(edit.secrets.is_none());
    store.upsert_host(&edit).unwrap();
    let loaded = store.get_host(&saved.meta.id).unwrap().unwrap().item;
    assert_eq!(loaded.secrets.unwrap().password.as_deref(), Some("hunter2"));
    // Some(default) clears them.
    edit.secrets = Some(HostSecrets::default());
    store.upsert_host(&edit).unwrap();
    let cleared = store.get_host(&saved.meta.id).unwrap().unwrap().item;
    assert_eq!(cleared.secrets, Some(HostSecrets::default()));
}

#[test]
fn secret_history_is_encrypted() {
    let store = store();
    let saved = store.upsert_host(&with_password("Web", "hunter2")).unwrap();
    store
        .upsert_host(&with_password_for(&saved.item, "correct-horse"))
        .unwrap();
    let raw: (Vec<u8>, i64) = store
        .conn
        .query_row(
            "SELECT old_value, encrypted FROM record_history WHERE field = 'password'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(raw.1, 1);
    let history = store.history(&saved.meta.id).unwrap();
    let entry = history.iter().find(|h| h.field == "password").unwrap();
    assert_eq!(entry.old_value, Some(serde_json::json!("hunter2")));
}

fn with_password_for(base: &Host, password: &str) -> Host {
    let mut h = base.clone();
    h.secrets = Some(HostSecrets {
        password: Some(password.into()),
        ..HostSecrets::default()
    });
    h
}

#[test]
fn ciphertext_is_bound_to_its_record() {
    let store = store();
    let a = store.upsert_host(&with_password("A", "secret-a")).unwrap();
    let b = store.upsert_host(&host("B")).unwrap();
    store
        .conn
        .execute(
            "UPDATE hosts SET password_enc = (SELECT password_enc FROM hosts WHERE id = ?1) WHERE id = ?2",
            [&a.meta.id, &b.meta.id],
        )
        .unwrap();
    assert!(matches!(
        store.get_host(&b.meta.id),
        Err(StoreError::Cipher(CryptoError::Authentication))
    ));
}

#[test]
fn secrets_never_appear_in_debug_output() {
    let secrets = HostSecrets {
        password: Some("hunter2".into()),
        private_key: None,
        passphrase: Some("pass".into()),
    };
    let text =
        format!("{:?}", with_password_for(&host("Web"), "hunter2")) + &format!("{secrets:?}");
    assert!(!text.contains("hunter2") && !text.contains("pass\""));
}

// ---- v1 migration ----------------------------------------------------------------

fn write_v1(dir: &Path, content: &str) -> std::path::PathBuf {
    let path = dir.join("connections.json");
    std::fs::write(&path, content).unwrap();
    path
}

#[test]
fn migrating_a_default_config_imports_only_settings() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), DEFAULT_FIXTURE);
    let store = store();
    let report = store.migrate_from_v1(&path).unwrap();
    assert!(!report.already_migrated);
    assert_eq!(report.hosts, EntityCounts::default());
    assert_eq!(report.groups, EntityCounts::default());
    assert_eq!(report.settings.imported, 10);
    assert!(report.skipped.is_empty() && report.warnings.is_empty());
    assert_eq!(
        store.get_setting("theme").unwrap(),
        Some(serde_json::json!("system"))
    );
    assert_eq!(
        store.get_setting("terminal").unwrap().unwrap()["scrollback_lines"],
        10000
    );
    assert!(store.get_setting(MIGRATION_SETTING).unwrap().is_some());
}

#[test]
fn migrating_a_full_config_keeps_everything() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), FULL_FIXTURE);
    let store = store();
    let report = store.migrate_from_v1(&path).unwrap();
    assert_eq!(report.hosts.imported, 2);
    assert_eq!(report.groups.imported, 3);
    assert_eq!(report.snippets.imported, 2);
    assert_eq!(report.tunnels.imported, 3);
    assert_eq!(report.settings.imported, 10);
    let kinds: Vec<&str> = report.skipped.iter().map(|s| s.kind).collect();
    assert_eq!(kinds, ["host", "sessions"]);
    assert_eq!(report.skipped[0].id.as_deref(), Some("bad-host"));
    assert_eq!(report.warnings.len(), 3, "{:?}", report.warnings);

    // The fully populated host, field for field.
    let prod = store
        .get_host("7d9b3a52-1c40-4c8e-8a53-0f6b9d2c1e01")
        .unwrap()
        .unwrap();
    let c = &prod.item.connection;
    assert_eq!(c.name, "Production DB");
    assert_eq!(
        c.target,
        ConnectionTarget::Endpoint {
            hostname: "db.prod.example".into(),
            username: Some("ops".into()),
            port: 2222
        }
    );
    assert_eq!(c.tags, ["database", "prod"]);
    assert!(c.favorite);
    assert_eq!(c.color.as_deref(), Some("#d94f4f"));
    assert_eq!(c.notes, "Primary. Failover is db2.");
    assert_eq!(c.proxy_jump.as_deref(), Some("ops@bastion.example:22"));
    assert_eq!(c.local_forwards.len(), 2);
    assert_eq!(c.remote_forwards, ["8080:localhost:80"]);
    assert_eq!(c.dynamic_forwards, ["1080"]);
    assert_eq!(c.remote_command.as_deref(), Some("tmux new -A -s main"));
    assert_eq!(c.local_directories, ["/home/me/work"]);
    assert_eq!(c.remote_directories, ["/var/log", "/srv"]);
    assert_eq!(c.terminal.scrollback_lines, 50000);
    assert_eq!(c.terminal.startup_command.as_deref(), Some("cd /srv"));
    assert_eq!(c.created_at.to_rfc3339(), "2025-03-01T08:30:00+00:00");
    assert_eq!(c.updated_at.to_rfc3339(), "2025-06-15T17:45:12+00:00");
    assert_eq!(
        prod.item.secrets,
        Some(HostSecrets::default()),
        "v1 had no secrets"
    );

    // Non-UUID ids became stable UUIDs and references follow them.
    let hosts = store.list_hosts().unwrap();
    assert_eq!(hosts.len(), 2);
    let bastion = hosts
        .iter()
        .find(|h| h.item.connection.name == "Bastion")
        .unwrap();
    assert!(uuid::Uuid::parse_str(&bastion.meta.id).is_ok());
    assert_eq!(
        bastion.item.connection.group_id, None,
        "dangling group dropped"
    );
    let socks = store
        .get_tunnel("c2f4e6a8-0b1d-4f3a-8e5c-7a9b1d3f5e70")
        .unwrap()
        .unwrap();
    assert_eq!(socks.item.host_id, bastion.meta.id);
    assert_eq!(store.list_tunnels().unwrap().len(), 3);
    let groups = store.list_groups().unwrap();
    let prod_group = groups.iter().find(|g| g.item.name == "Production").unwrap();
    let eu = groups.iter().find(|g| g.item.name == "EU").unwrap();
    assert_eq!(
        eu.item.parent_id.as_deref(),
        Some(prod_group.meta.id.as_str())
    );
    assert_eq!(
        prod.item.connection.group_id.as_deref(),
        Some(prod_group.meta.id.as_str())
    );
    let orphan = groups.iter().find(|g| g.item.name == "Orphan").unwrap();
    assert_eq!(orphan.item.parent_id, None);
    assert_eq!(store.list_snippets().unwrap().len(), 2);
    assert_eq!(store.list_tags().unwrap().len(), 3);

    // Settings.
    assert_eq!(
        store.get_setting("theme").unwrap(),
        Some(serde_json::json!("dark"))
    );
    assert_eq!(
        store.get_setting("locale").unwrap(),
        Some(serde_json::json!("zh_cn"))
    );
    assert_eq!(
        store.get_setting("workspace").unwrap(),
        Some(serde_json::json!("hosts"))
    );
    assert_eq!(
        store.get_setting("experimental").unwrap().unwrap()["remote_file_browser"],
        true
    );
    let sidebar = store.get_setting("sidebar").unwrap().unwrap();
    assert_eq!(
        sidebar["collapsed_group_ids"][0],
        prod_group.meta.id.as_str()
    );
    let recent = store.get_setting("recent_connection_ids").unwrap().unwrap();
    assert_eq!(recent[0], bastion.meta.id.as_str());
    assert_eq!(
        store.get_setting("sync").unwrap().unwrap()["branch"],
        "main"
    );
}

#[test]
fn migrating_an_old_config_without_default_fields() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), OLD_FIXTURE);
    let store = store();
    let report = store.migrate_from_v1(&path).unwrap();
    assert_eq!(report.hosts.imported, 1);
    assert_eq!(report.groups.imported, 1);
    assert_eq!(report.snippets.imported, 1);
    assert!(report.skipped.is_empty());
    // Missing sections take v1's defaults, and the legacy workspace maps to hosts.
    assert_eq!(
        store.get_setting("workspace").unwrap(),
        Some(serde_json::json!("hosts"))
    );
    assert_eq!(
        store.get_setting("display_density").unwrap(),
        Some(serde_json::json!("comfortable"))
    );
    assert_eq!(
        store.get_setting("theme").unwrap(),
        Some(serde_json::json!("light"))
    );
    assert_eq!(
        store.get_setting("terminal").unwrap().unwrap()["font_size"],
        14.0
    );
    assert_eq!(
        store.list_hosts().unwrap()[0].item.connection.name,
        "Old box"
    );
}

#[test]
fn migration_backs_up_and_never_touches_the_original() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), FULL_FIXTURE);
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let store = store();
    let report = store.migrate_from_v1(&path).unwrap();
    let backup = dir.path().join("connections.v1-backup.json");
    assert_eq!(report.backup_path.as_deref(), Some(backup.as_path()));
    assert!(report.backup_created);
    assert_eq!(std::fs::read_to_string(&backup).unwrap(), FULL_FIXTURE);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), FULL_FIXTURE);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        modified
    );
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "no stray temp files: {names:?}");
}

#[test]
fn second_migration_run_is_a_no_op() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), FULL_FIXTURE);
    let store = store();
    store.migrate_from_v1(&path).unwrap();
    let tables = [
        "hosts",
        "groups",
        "tags",
        "host_tags",
        "snippets",
        "tunnels",
        "settings",
        "record_history",
    ];
    let before: Vec<i64> = tables.iter().map(|t| count(&store, t)).collect();
    let second = store.migrate_from_v1(&path).unwrap();
    assert!(second.already_migrated);
    assert!(!second.backup_created, "existing backup is kept");
    assert_eq!(second.hosts, EntityCounts::default());
    let after: Vec<i64> = tables.iter().map(|t| count(&store, t)).collect();
    assert_eq!(before, after);
}

#[test]
fn rerun_after_the_file_changed_adds_only_new_records_and_keeps_edits() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), OLD_FIXTURE);
    let store = store();
    store.migrate_from_v1(&path).unwrap();
    // The user renames the migrated host in v2.
    let mut edited = store.list_hosts().unwrap()[0].item.clone();
    edited.connection.name = "Renamed in v2".into();
    store.upsert_host(&edited).unwrap();
    // v1 is used again and gains a snippet.
    let changed = OLD_FIXTURE.replace(
        r#""snippets": [{ "id": "sn-old", "name": "uptime", "content": "uptime" }]"#,
        r#""snippets": [{ "id": "sn-old", "name": "uptime", "content": "uptime" }, { "id": "sn-new", "name": "who", "content": "who" }]"#,
    );
    assert_ne!(changed, OLD_FIXTURE);
    std::fs::write(&path, &changed).unwrap();
    let report = store.migrate_from_v1(&path).unwrap();
    assert!(!report.already_migrated);
    assert_eq!(
        report.hosts,
        EntityCounts {
            imported: 0,
            existing: 1
        }
    );
    assert_eq!(
        report.snippets,
        EntityCounts {
            imported: 1,
            existing: 1
        }
    );
    assert_eq!(report.settings.imported, 0);
    assert_eq!(
        store.list_hosts().unwrap()[0].item.connection.name,
        "Renamed in v2"
    );
    assert_eq!(store.list_snippets().unwrap().len(), 2);
    // The first backup still holds the first file.
    let backup = dir.path().join("connections.v1-backup.json");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), OLD_FIXTURE);
}

#[test]
fn a_soft_deleted_migrated_host_is_not_resurrected() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), OLD_FIXTURE);
    let store = store();
    store.migrate_from_v1(&path).unwrap();
    let id = store.list_hosts().unwrap()[0].meta.id.clone();
    store.soft_delete_host(&id).unwrap();
    std::fs::write(&path, OLD_FIXTURE.replace("Old box", "Old box!")).unwrap();
    store.migrate_from_v1(&path).unwrap();
    assert!(store.list_hosts().unwrap().is_empty());
}

#[test]
fn migration_works_on_a_file_database_and_survives_reopening() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_v1(dir.path(), FULL_FIXTURE);
    let db = dir.path().join("v2.db");
    file_store(&db, "dev-a").migrate_from_v1(&path).unwrap();
    let reopened = file_store(&db, "dev-a");
    assert_eq!(reopened.list_hosts().unwrap().len(), 2);
    assert!(reopened.migrate_from_v1(&path).unwrap().already_migrated);
}

#[test]
fn unusable_v1_files_are_refused_without_a_backup() {
    let dir = tempfile::tempdir().unwrap();
    let store = store();
    let backup = dir.path().join("connections.v1-backup.json");

    let path = write_v1(dir.path(), r#"{"connections":[]}"#);
    assert!(matches!(
        store.migrate_from_v1(&path),
        Err(StoreError::Invalid(_))
    ));

    let path = write_v1(dir.path(), r#"{"schema_version":99}"#);
    assert!(matches!(
        store.migrate_from_v1(&path),
        Err(StoreError::Invalid(_))
    ));

    let path = write_v1(
        dir.path(),
        r#"{"schema_version":3,"connections":[{"id":"x","password":"nope"}]}"#,
    );
    assert!(matches!(
        store.migrate_from_v1(&path),
        Err(StoreError::V1Config(_))
    ));

    let path = write_v1(dir.path(), "not json");
    assert!(matches!(
        store.migrate_from_v1(&path),
        Err(StoreError::Json(_))
    ));

    assert!(matches!(
        store.migrate_from_v1(&dir.path().join("absent.json")),
        Err(StoreError::Io(_))
    ));
    assert!(!backup.exists());
    assert_eq!(count(&store, "hosts"), 0);
    assert!(store.get_setting(MIGRATION_SETTING).unwrap().is_none());
}

#[test]
fn a_failed_migration_rolls_back_completely() {
    let dir = tempfile::tempdir().unwrap();
    // Two tunnels sharing a store id make the second insert collide? They are
    // skipped as duplicates instead; force a real failure with a bad vault.
    let path = write_v1(dir.path(), FULL_FIXTURE);
    let store = store();
    store.conn.execute_batch("DROP TABLE tunnels").unwrap();
    assert!(store.migrate_from_v1(&path).is_err());
    assert_eq!(count(&store, "hosts"), 0);
    assert_eq!(count(&store, "groups"), 0);
    assert_eq!(count(&store, "settings"), 0);
}

#[test]
fn a_serial_host_roundtrips_and_stays_a_serial_host() {
    use easyssh_core::{SerialFlowControl, SerialParity, SerialSettings};
    let store = store();
    let mut connection = Connection::alias("Router console", "x");
    connection.target = ConnectionTarget::Serial(SerialSettings {
        port: "/dev/ttyUSB0".into(),
        baud: 9600,
        data_bits: 7,
        parity: SerialParity::Even,
        stop_bits: 2,
        flow_control: SerialFlowControl::Hardware,
    });
    let saved = store.upsert_host(&Host::new(connection.clone())).unwrap();
    let loaded = store.get_host(&connection.id).unwrap().unwrap();
    assert_eq!(saved, loaded);
    assert_eq!(loaded.item.connection.target, connection.target);
    assert!(loaded.item.connection.is_serial());

    // Changing it back to an SSH host clears the serial settings.
    let mut ssh = loaded.item.clone();
    ssh.connection.target = ConnectionTarget::Endpoint {
        hostname: "example.test".into(),
        username: None,
        port: 22,
    };
    store.upsert_host(&ssh).unwrap();
    let again = store.get_host(&connection.id).unwrap().unwrap();
    assert!(!again.item.connection.is_serial());
    assert_eq!(again.item.connection.target, ssh.connection.target);
}

#[test]
fn hosts_saved_before_serial_support_still_load() {
    // A host row written without the `serial` column value (NULL) is an SSH
    // host, as every row is after migration 6.
    let store = store();
    let saved = store.upsert_host(&host("Web")).unwrap();
    let serial: Option<String> = store
        .conn
        .query_row(
            "SELECT serial FROM hosts WHERE id = ?1",
            [&saved.meta.id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(serial.is_none());
    assert!(!store.list_hosts().unwrap()[0].item.connection.is_serial());
}
