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
    store.replace_all(&config).unwrap();
    store.set_setting_as("theme", &config.theme).unwrap();
    store.set_setting_as("locale", &config.locale).unwrap();
    store.set_setting_as("terminal", &config.terminal).unwrap();
    store
        .set_setting_as("recent_connection_ids", &vec![db.id.clone()])
        .unwrap();

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
    store.replace_all(&loaded).unwrap();
    assert_eq!(store.load_app_config().unwrap().connections.len(), 2);
    let mut smaller = loaded;
    smaller.connections.retain(|c| c.name != "Db");
    store.replace_all(&smaller).unwrap();
    store.remove_host(&db.id).unwrap();
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
    store
        .set_setting_as("recent_connection_ids", &vec![host.id.clone()])
        .unwrap();
    assert!(store.remove_host(&host.id).unwrap());
    assert!(store
        .load_app_config()
        .unwrap()
        .recent_connection_ids
        .is_empty());
    assert!(!store.remove_host("unknown").unwrap());
}

/// The bug this guards against: the in-process path loads the configuration,
/// the daemon saves a host, then the in-process path saves a snippet. Saving
/// the whole configuration back removed the new host.
#[test]
fn saving_a_snippet_from_a_stale_config_keeps_a_host_the_daemon_added() {
    let dir = tempfile::tempdir().unwrap();
    let app = open_app_store(&v1(dir.path())).unwrap().store;
    let daemon = open_app_store(&v1(dir.path())).unwrap().store;
    let old = Connection::alias("Old", "old");
    app.save_host(&old, None).unwrap();

    let stale = app.load_app_config().unwrap();
    let added = Connection::alias("Added", "added");
    daemon.save_host(&added, None).unwrap();

    let snippet = CommandSnippet {
        id: uuid::Uuid::new_v4().to_string(),
        name: "uptime".into(),
        content: "uptime".into(),
    };
    app.save_snippet(&snippet).unwrap();
    // The other section savers and an import merge leave hosts alone too.
    let mut sync = stale.sync.clone();
    sync.branch = Some("dev".into());
    app.save_sync_preferences(&sync).unwrap();
    app.upsert_records(&stale).unwrap();

    let names: Vec<_> = daemon
        .load_app_config()
        .unwrap()
        .connections
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
    assert!(names.contains(&"Added".to_owned()));
    let loaded = app.load_app_config().unwrap();
    assert_eq!(loaded.snippets.len(), 1);
    assert_eq!(loaded.sync.branch.as_deref(), Some("dev"));

    assert!(app.remove_snippet(&snippet.id).unwrap());
    assert!(app.load_app_config().unwrap().snippets.is_empty());

    // Only an explicit replace_all drops what the config does not list.
    app.replace_all(&stale).unwrap();
    let names: Vec<_> = app
        .load_app_config()
        .unwrap()
        .connections
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["Old"]);
}

/// Every field of a `Connection` survives save and load. A field added to
/// `Connection` later must be set here too: the check below fails on any
/// field still at its empty value, so a column that was forgotten (and reads
/// back as a default) cannot go unnoticed.
#[test]
fn a_connection_with_every_field_set_roundtrips() {
    use easyssh_core::{ConnectionTarget, TerminalPreferences};

    let dir = tempfile::tempdir().unwrap();
    let store = open_app_store(&v1(dir.path())).unwrap().store;
    let group = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Prod".into(),
        color: None,
        parent_id: None,
    };
    let mut full = Connection::alias("Full", "ignored");
    full.target = ConnectionTarget::Endpoint {
        hostname: "full.example.com".into(),
        username: Some("deploy".into()),
        port: 2222,
    };
    full.group_id = Some(group.id.clone());
    full.tags = vec!["db".into(), "prod".into()];
    full.favorite = true;
    full.color = Some("#aabbcc".into());
    full.notes = "rotate keys".into();
    full.proxy_jump = Some("bastion".into());
    full.local_forwards = vec!["8080:localhost:80".into()];
    full.remote_forwards = vec!["9090:localhost:90".into()];
    full.dynamic_forwards = vec!["1080".into()];
    full.remote_command = Some("tmux attach".into());
    full.local_directories = vec!["/home/me/work".into()];
    full.remote_directories = vec!["/srv/app".into()];
    full.terminal = TerminalPreferences {
        font_size: 17.0,
        scrollback_lines: 54_321,
        startup_command: Some("cd /srv".into()),
    };

    // No field may still be at its empty value (null, false, 0, "", []).
    let json = serde_json::to_value(&full).unwrap();
    for (key, value) in json.as_object().unwrap() {
        let empty = match value {
            serde_json::Value::Null => true,
            serde_json::Value::Bool(b) => !b,
            serde_json::Value::String(s) => s.is_empty(),
            serde_json::Value::Array(a) => a.is_empty(),
            serde_json::Value::Number(n) => n.as_f64() == Some(0.0),
            serde_json::Value::Object(o) => o.is_empty(),
        };
        assert!(!empty, "set `{key}` in this test so it is checked");
    }

    store.save_host(&full, Some(&group)).unwrap();
    let loaded = store
        .load_app_config()
        .unwrap()
        .connections
        .into_iter()
        .find(|c| c.id == full.id)
        .unwrap();
    let mut expected = serde_json::to_value(&full).unwrap();
    let got = serde_json::to_value(&loaded).unwrap();
    // The store stamps its own update time and keeps millisecond precision.
    expected["updated_at"] = got["updated_at"].clone();
    expected["created_at"] = got["created_at"].clone();
    assert_eq!(got, expected);
}
