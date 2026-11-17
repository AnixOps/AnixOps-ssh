//! `open_app_store`: first-start migration shared by the daemon and the app's
//! in-process fallback, and the `AppConfig` bridge the app's v1 code uses.

use easyssh_core::{AppConfig, CommandSnippet, Connection, Group, Locale, Theme};
use easyssh_store::{db_path_for, open_app_store, DB_FILE_NAME};
use std::path::Path;

const V1_FULL: &str = include_str!("../fixtures/v1_full.json");

fn v1(dir: &Path) -> std::path::PathBuf {
    dir.join("connections.json")
}

#[test]
fn the_database_sits_next_to_the_v1_file() {
    assert_eq!(
        db_path_for(Path::new("/x/EasySSH/connections.json")),
        Path::new("/x/EasySSH").join(DB_FILE_NAME)
    );
}

#[test]
fn a_fresh_install_gets_an_empty_database_and_no_migration() {
    let dir = tempfile::tempdir().unwrap();
    let opened = open_app_store(&v1(dir.path())).unwrap();
    assert!(opened.migration.is_none() && opened.migration_error.is_none());
    assert!(opened.store.list_hosts().unwrap().is_empty());
    assert!(dir.path().join(DB_FILE_NAME).is_file());
    assert!(!dir.path().join("connections.v1-backup.json").exists());
}

#[test]
fn first_start_migrates_keeps_a_backup_and_never_runs_twice() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(v1(dir.path()), V1_FULL).unwrap();

    let first = open_app_store(&v1(dir.path())).unwrap();
    let report = first.migration.as_ref().expect("migrated on first start");
    assert_eq!(report.hosts.imported, 2);
    assert_eq!(first.store.list_hosts().unwrap().len(), 2);
    let backup = dir.path().join("connections.v1-backup.json");
    assert_eq!(std::fs::read_to_string(backup).unwrap(), V1_FULL);
    assert_eq!(
        std::fs::read_to_string(v1(dir.path())).unwrap(),
        V1_FULL,
        "the original is not touched"
    );

    // The user deletes a host; opening again must not bring it back.
    let id = first.store.list_hosts().unwrap()[0]
        .item
        .connection
        .id
        .clone();
    assert!(first.store.remove_host(&id).unwrap());
    drop(first);
    let second = open_app_store(&v1(dir.path())).unwrap();
    assert!(second.migration.is_none(), "already migrated");
    assert_eq!(second.store.list_hosts().unwrap().len(), 1);
}

#[test]
fn an_unreadable_v1_file_is_reported_and_the_app_still_opens() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(v1(dir.path()), "{ not json").unwrap();
    let opened = open_app_store(&v1(dir.path())).unwrap();
    assert!(opened.migration.is_none());
    assert!(opened.migration_error.is_some());
    assert!(opened.store.list_hosts().unwrap().is_empty());
}

#[test]
fn two_processes_starting_together_migrate_once() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(v1(dir.path()), V1_FULL).unwrap();
    let path = v1(dir.path());
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || {
                let opened = open_app_store(&path).unwrap();
                assert!(
                    opened.migration_error.is_none(),
                    "{:?}",
                    opened.migration_error
                );
                opened.store.list_hosts().unwrap().len()
            })
        })
        .collect();
    for thread in threads {
        // Whoever came second saw the finished migration or waited for it.
        assert!(thread.join().unwrap() <= 2);
    }
    let opened = open_app_store(&path).unwrap();
    assert_eq!(opened.store.list_hosts().unwrap().len(), 2);
    assert_eq!(opened.store.list_groups().unwrap().len(), 3);
}

#[test]
fn the_device_id_is_stable_across_opens() {
    let dir = tempfile::tempdir().unwrap();
    let first = open_app_store(&v1(dir.path())).unwrap();
    let id = first.store.device_id().to_owned();
    drop(first);
    assert_eq!(
        open_app_store(&v1(dir.path())).unwrap().store.device_id(),
        id
    );
}

#[test]
fn app_config_roundtrips_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_app_store(&v1(dir.path())).unwrap().store;

    let mut config = AppConfig::new();
    config.theme = Theme::Dark;
    config.locale = Locale::En;
    config.terminal.font_size = 15.0;
    // Ids from the v1 era are not UUIDs.
    config.groups.push(Group {
        id: "group-1700000000000".into(),
        name: "Prod".into(),
        color: None,
        parent_id: None,
    });
    let mut web = Connection::alias("Web", "web");
    web.group_id = Some("group-1700000000000".into());
    let db = Connection::alias("Db", "db");
    config.connections = vec![web.clone(), db.clone()];
    config.recent_connection_ids = vec![db.id.clone()];
    config.snippets.push(CommandSnippet {
        id: uuid::Uuid::new_v4().to_string(),
        name: "uptime".into(),
        content: "uptime".into(),
    });
    store.save_app_config(&config).unwrap();

    let loaded = store.load_app_config().unwrap();
    assert_eq!(loaded.connections.len(), 2);
    assert_eq!(loaded.groups.len(), 1);
    assert_eq!(loaded.groups[0].name, "Prod");
    let loaded_web = loaded.connections.iter().find(|c| c.name == "Web").unwrap();
    assert_eq!(
        loaded_web.group_id.as_deref(),
        Some(loaded.groups[0].id.as_str())
    );
    assert_eq!(loaded.theme, Theme::Dark);
    assert_eq!(loaded.locale, Locale::En);
    assert_eq!(loaded.terminal.font_size, 15.0);
    assert_eq!(loaded.recent_connection_ids, std::slice::from_ref(&db.id));
    assert_eq!(loaded.snippets.len(), 1);

    // Saving what was loaded changes nothing; dropping a host removes it.
    store.save_app_config(&loaded).unwrap();
    let mut smaller = loaded;
    smaller.connections.retain(|c| c.name != "Db");
    smaller.recent_connection_ids.clear();
    store.save_app_config(&smaller).unwrap();
    let again = store.load_app_config().unwrap();
    assert_eq!(again.connections.len(), 1);
    assert_eq!(again.connections[0].name, "Web");
    assert!(again.recent_connection_ids.is_empty());

    // A removed host can be saved again.
    store.save_host(&db, None).unwrap();
    assert_eq!(store.load_app_config().unwrap().connections.len(), 2);
}

#[test]
fn removing_a_host_drops_it_from_the_recent_list() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_app_store(&v1(dir.path())).unwrap().store;
    let host = Connection::alias("Web", "web");
    store.save_host(&host, None).unwrap();
    let mut config = store.load_app_config().unwrap();
    config.recent_connection_ids = vec![host.id.clone()];
    store.save_app_config(&config).unwrap();
    assert!(store.remove_host(&host.id).unwrap());
    assert!(store
        .load_app_config()
        .unwrap()
        .recent_connection_ids
        .is_empty());
    assert!(!store.remove_host("unknown").unwrap());
}
