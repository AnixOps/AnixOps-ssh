use crate::*;
use easyssh_core::Connection;
use easyssh_crypto::{DeviceKey, SecretText};
use std::sync::Arc;

fn open(cipher: Arc<DeviceKeyCipher>) -> Store {
    Store::open_in_memory("dev-a", cipher).unwrap()
}

fn unlocked() -> Arc<DeviceKeyCipher> {
    Arc::new(DeviceKeyCipher::unlocked(&DeviceKey::from_bytes([9; 32])).unwrap())
}

fn host(store: &Store, name: &str) -> String {
    let connection = Connection::alias(name, "example");
    let id = connection.id.clone();
    store.save_host(&connection, None).unwrap();
    id
}

fn set(value: &str) -> SecretChange {
    SecretChange::Set(SecretText::new(value))
}

fn only_password(value: &str) -> SecretsUpdate {
    SecretsUpdate {
        password: set(value),
        ..SecretsUpdate::default()
    }
}

#[test]
fn round_trip_keep_and_clear() {
    let store = open(unlocked());
    let id = host(&store, "web");
    assert_eq!(
        store.host_secret_flags(&id).unwrap(),
        SecretFlags::default()
    );
    store
        .set_host_secrets(
            &id,
            &SecretsUpdate {
                password: set("pw"),
                private_key: set("KEY"),
                ..SecretsUpdate::default()
            },
        )
        .unwrap();
    let flags = store.host_secret_flags(&id).unwrap();
    assert!(flags.password && flags.private_key && !flags.passphrase);
    // Keep leaves values alone; Set replaces; Clear removes.
    store
        .set_host_secrets(
            &id,
            &SecretsUpdate {
                passphrase: set("phrase"),
                password: SecretChange::Clear,
                ..SecretsUpdate::default()
            },
        )
        .unwrap();
    let got = store.host_secrets(&id).unwrap();
    assert!(got.password.is_none());
    assert_eq!(got.private_key.unwrap().expose(), "KEY");
    assert_eq!(got.passphrase.unwrap().expose(), "phrase");
    // Clearing the last ones removes the row.
    store
        .set_host_secrets(
            &id,
            &SecretsUpdate {
                private_key: SecretChange::Clear,
                passphrase: SecretChange::Clear,
                ..SecretsUpdate::default()
            },
        )
        .unwrap();
    assert!(store.hosts_with_secrets().unwrap().is_empty());
    assert_eq!(store.host_secrets(&id).unwrap(), StoredSecrets::default());
}

#[test]
fn unknown_host_is_not_found_and_empty_string_clears() {
    let store = open(unlocked());
    let error = store
        .set_host_secrets("nope", &only_password("x"))
        .unwrap_err();
    assert!(matches!(error, StoreError::NotFound { .. }));
    let id = host(&store, "web");
    store.set_host_secrets(&id, &only_password("x")).unwrap();
    store.set_host_secrets(&id, &only_password("")).unwrap();
    assert!(!store.host_secret_flags(&id).unwrap().any());
}

#[test]
fn locked_vault_refuses_reads_and_writes_but_flags_still_work() {
    let cipher = unlocked();
    let store = open(cipher.clone());
    let id = host(&store, "web");
    store.set_host_secrets(&id, &only_password("pw")).unwrap();
    cipher.lock();
    assert!(store.host_secret_flags(&id).unwrap().password);
    assert!(matches!(
        store.host_secrets(&id),
        Err(StoreError::Cipher(CryptoError::Locked))
    ));
    let write = SecretsUpdate {
        passphrase: set("x"),
        ..SecretsUpdate::default()
    };
    assert!(matches!(
        store.set_host_secrets(&id, &write),
        Err(StoreError::Cipher(CryptoError::Locked))
    ));
    // The failed write changed nothing.
    assert!(!store.host_secret_flags(&id).unwrap().passphrase);
    // Listing hosts never needs the key.
    assert_eq!(store.list_hosts().unwrap().len(), 1);
}

#[test]
fn ciphertext_is_bound_to_host_and_field() {
    let store = open(unlocked());
    let a = host(&store, "a");
    let b = host(&store, "b");
    for id in [&a, &b] {
        store
            .set_host_secrets(
                id,
                &SecretsUpdate {
                    password: set("pw"),
                    passphrase: set("phrase"),
                    ..SecretsUpdate::default()
                },
            )
            .unwrap();
    }
    // Move host a's password blob into host b.
    store
        .conn
        .execute(
            "UPDATE host_secrets SET password_enc =
               (SELECT password_enc FROM host_secrets WHERE host_id = ?1)
             WHERE host_id = ?2",
            [&a, &b],
        )
        .unwrap();
    assert!(matches!(
        store.host_secrets(&b),
        Err(StoreError::Cipher(CryptoError::Authentication))
    ));
    assert!(store.host_secrets(&a).is_ok());
    // Swap two fields of one host.
    store
        .conn
        .execute(
            "UPDATE host_secrets SET password_enc = passphrase_enc,
                                     passphrase_enc = password_enc
             WHERE host_id = ?1",
            [&a],
        )
        .unwrap();
    assert!(matches!(
        store.host_secrets(&a),
        Err(StoreError::Cipher(CryptoError::Authentication))
    ));
}

#[test]
fn forget_all_works_while_locked_and_removing_a_host_drops_its_secrets() {
    let cipher = unlocked();
    let store = open(cipher.clone());
    let a = host(&store, "a");
    let b = host(&store, "b");
    for id in [&a, &b] {
        store.set_host_secrets(id, &only_password("pw")).unwrap();
    }
    assert!(store.remove_host(&a).unwrap());
    assert_eq!(store.hosts_with_secrets().unwrap(), vec![b.clone()]);
    cipher.lock();
    assert_eq!(store.forget_all_secrets().unwrap(), 1);
    assert!(store.hosts_with_secrets().unwrap().is_empty());
}

#[test]
fn nothing_readable_reaches_the_database_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.db");
    let store = Store::open(&path, "dev-a", unlocked()).unwrap();
    let id = host(&store, "web");
    store
        .set_host_secrets(
            &id,
            &SecretsUpdate {
                password: set("hunter2-very-unique"),
                private_key: set("-----BEGIN OPENSSH PRIVATE KEY-----unique-body"),
                passphrase: set("passphrase-very-unique"),
                proxy_password: set("proxy-pass-very-unique"),
            },
        )
        .unwrap();
    drop(store);
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        for needle in [
            "hunter2-very-unique",
            "unique-body",
            "passphrase-very-unique",
            "proxy-pass-very-unique",
        ] {
            assert!(!text.contains(needle), "{needle} found on disk");
        }
    }
}

#[test]
fn migration_three_adds_the_table_to_a_version_two_database() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))
            .unwrap();
        conn.execute_batch(include_str!(
            "../migrations/0002_host_shell_integration.sql"
        ))
        .unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL);
             INSERT INTO schema_migrations VALUES (1, 'init', 'x'), (2, 'host_shell_integration', 'x');",
        )
        .unwrap();
    }
    let store = Store::open(&path, "dev-a", unlocked()).unwrap();
    assert_eq!(store.schema_version().unwrap(), latest_version());
    assert!(latest_version() >= 3);
    let id = host(&store, "web");
    store.set_host_secrets(&id, &only_password("pw")).unwrap();
    assert!(store.host_secret_flags(&id).unwrap().password);
}

#[test]
fn debug_never_shows_values() {
    let update = only_password("hunter2");
    assert!(!format!("{update:?}").contains("hunter2"));
    let stored = StoredSecrets {
        password: Some(SecretText::new("hunter2")),
        ..StoredSecrets::default()
    };
    assert!(!format!("{stored:?}").contains("hunter2"));
}

#[test]
fn the_proxy_password_is_a_sealed_secret_like_the_others() {
    let cipher = unlocked();
    let store = open(cipher.clone());
    let id = host(&store, "web");
    store
        .set_host_secrets(&id, &only_password("login"))
        .unwrap();
    store
        .set_host_secrets(
            &id,
            &SecretsUpdate {
                proxy_password: set("proxy-secret"),
                ..SecretsUpdate::default()
            },
        )
        .unwrap();
    let flags = store.host_secret_flags(&id).unwrap();
    assert!(flags.password && flags.proxy_password && !flags.private_key);
    let got = store.host_secrets(&id).unwrap();
    assert_eq!(got.password.unwrap().expose(), "login");
    assert_eq!(got.proxy_password.unwrap().expose(), "proxy-secret");
    assert_eq!(store.hosts_with_secrets().unwrap(), vec![id.clone()]);

    // Clearing the login keeps the proxy password; clearing both removes the row.
    store
        .set_host_secrets(
            &id,
            &SecretsUpdate {
                password: SecretChange::Clear,
                ..SecretsUpdate::default()
            },
        )
        .unwrap();
    assert!(store.host_secret_flags(&id).unwrap().proxy_password);
    cipher.lock();
    assert!(matches!(
        store.host_secrets(&id),
        Err(StoreError::Cipher(CryptoError::Locked))
    ));
    assert!(store.host_secret_flags(&id).unwrap().proxy_password);
    cipher.unlock(&DeviceKey::from_bytes([9; 32])).unwrap();
    store
        .set_host_secrets(
            &id,
            &SecretsUpdate {
                proxy_password: SecretChange::Clear,
                ..SecretsUpdate::default()
            },
        )
        .unwrap();
    assert!(store.hosts_with_secrets().unwrap().is_empty());
}

#[test]
fn the_proxy_settings_of_a_host_round_trip_and_stay_out_of_the_secrets() {
    use easyssh_core::{ProxyKind, ProxySettings};
    let store = open(unlocked());
    let mut connection = Connection::alias("web", "example");
    connection.proxy = Some(ProxySettings {
        kind: ProxyKind::Http,
        host: "proxy.example.org".into(),
        port: 3128,
        username: Some("me".into()),
    });
    let id = connection.id.clone();
    store.save_host(&connection, None).unwrap();
    let loaded = store.get_host(&id).unwrap().unwrap();
    assert_eq!(loaded.item.connection.proxy, connection.proxy);
    // Without a saved password the flag is off.
    assert!(!store.host_secret_flags(&id).unwrap().proxy_password);
    // Editing the host without a proxy removes it.
    connection.proxy = None;
    store.save_host(&connection, None).unwrap();
    assert_eq!(
        store.get_host(&id).unwrap().unwrap().item.connection.proxy,
        None
    );
}
