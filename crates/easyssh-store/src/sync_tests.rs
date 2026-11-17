//! The `SyncStore` adapter against two stores and the in-memory server.

use crate::sync::{personal_vault_server_id, StoreSync};
use crate::*;
use easyssh_core::{CommandSnippet, Connection, Group, Tunnel, TunnelKind};
use easyssh_crypto::{certify_device, AccountKeyPair, DeviceSigningKey, SecretText, VaultKey};
use easyssh_sync::{
    DeviceIdentity, MemoryServer, MemoryTransport, SyncEngine, SyncStore, Verifier, VerifyError,
};
use std::sync::{Arc, Mutex};

type Engine = SyncEngine<MemoryTransport, StoreSync>;

struct Dev {
    store: Arc<Mutex<Store>>,
    engine: Engine,
    personal: String,
}

fn account() -> AccountKeyPair {
    AccountKeyPair::from_seeds([1; 32], [2; 32])
}

fn open_store(name: &str) -> Arc<Mutex<Store>> {
    Arc::new(Mutex::new(
        Store::open_in_memory(name, Arc::new(PlaintextForTests)).unwrap(),
    ))
}

fn engine_for(
    store: &Arc<Mutex<Store>>,
    name: &str,
    seed: u8,
    server: &MemoryServer,
    verifier: Verifier,
    personal: &str,
) -> Engine {
    let acct = account();
    let key = DeviceSigningKey::from_seed([seed; 32]);
    let cert = certify_device(&acct.signing, &key.public(), name, 1_700_000_000);
    let identity = DeviceIdentity {
        // The stamps in the store use the store's device id, so the sync
        // identity must be the same string.
        device_id: name.to_owned(),
        signing_key: key,
        cert,
    };
    let mut engine = SyncEngine::new(
        server.transport(),
        StoreSync::new(store.clone(), personal),
        identity,
        verifier,
    );
    engine.add_vault_key(personal, VaultKey::from_bytes([7; 32]));
    engine
}

fn dev(name: &str, seed: u8, server: &MemoryServer) -> Dev {
    let personal = personal_vault_server_id("acct-1");
    let store = open_store(name);
    let verifier = Verifier::new(account().signing.public());
    let engine = engine_for(&store, name, seed, server, verifier, &personal);
    Dev {
        store,
        engine,
        personal,
    }
}

impl Dev {
    fn store(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store.lock().unwrap()
    }

    async fn sync(&mut self) -> easyssh_sync::SyncReport {
        let personal = self.personal.clone();
        self.engine.sync(&personal).await.unwrap()
    }

    fn host(&self, id: &str) -> Option<Record<Host>> {
        self.store().get_host(id).unwrap()
    }
}

fn new_host(name: &str) -> Connection {
    Connection::alias(name, name.to_lowercase())
}

#[tokio::test]
async fn a_host_travels_with_its_fields_but_not_its_secrets() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);

    let mut connection = new_host("Web");
    connection.tags = vec!["prod".into()];
    connection.favorite = true;
    connection.proxy_jump = Some("bastion".into());
    let id = connection.id.clone();
    {
        let store = a.store();
        store.save_host(&connection, None).unwrap();
        store
            .set_host_secrets(
                &id,
                &SecretsUpdate {
                    password: SecretChange::Set(SecretText::new("hunter2-SECRET")),
                    ..SecretsUpdate::default()
                },
            )
            .unwrap();
    }
    let report = a.sync().await;
    assert_eq!(report.pushed, 1);

    let report = b.sync().await;
    assert_eq!(report.pulled, 1);
    let got = b.host(&id).expect("host arrived").item.connection;
    assert_eq!(got.name, "Web");
    assert_eq!(got.tags, vec!["prod".to_owned()]);
    assert!(got.favorite);
    assert_eq!(got.proxy_jump.as_deref(), Some("bastion"));
    assert_eq!(got.target, connection.target);
    // The secret stayed on A.
    assert!(!b.store().host_secret_flags(&id).unwrap().any());
    assert!(a.store().host_secret_flags(&id).unwrap().password);
    // It landed in the personal vault of B under the shared local id.
    assert_eq!(b.host(&id).unwrap().meta.vault_id, PERSONAL_VAULT_ID);
    // Nothing is pending after a sync; syncing again moves nothing.
    assert_eq!(b.store().sync_pending(&b.personal).unwrap(), 0);
    let again = b.sync().await;
    assert_eq!((again.pulled, again.pushed, again.merged), (0, 0, 0));
}

#[tokio::test]
async fn groups_snippets_tunnels_and_known_hosts_sync() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);

    let group = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Prod".into(),
        color: Some("#fff".into()),
        parent_id: None,
    };
    let mut connection = new_host("db");
    connection.group_id = Some(group.id.clone());
    let snippet = CommandSnippet {
        id: uuid::Uuid::new_v4().to_string(),
        name: "uptime".into(),
        content: "uptime".into(),
    };
    let tunnel = Tunnel {
        id: uuid::Uuid::new_v4().to_string(),
        name: "pg".into(),
        host_id: connection.id.clone(),
        kind: TunnelKind::Local,
        bind_address: Some("127.0.0.1".into()),
        bind_port: 5433,
        target_host: Some("localhost".into()),
        target_port: Some(5432),
    };
    let known = KnownHost::new("example.org", 22, "ssh-ed25519", vec![1, 2, 3, 4]);
    {
        let s = a.store();
        s.save_host(&connection, Some(&group)).unwrap();
        s.save_snippet(&snippet).unwrap();
        s.save_tunnel(&tunnel).unwrap();
        s.upsert_known_host(&known).unwrap();
    }
    a.sync().await;
    let report = b.sync().await;
    assert_eq!(report.pulled, 5);
    let s = b.store();
    assert_eq!(s.get_group(&group.id).unwrap().unwrap().item.name, "Prod");
    assert_eq!(
        s.get_host(&connection.id)
            .unwrap()
            .unwrap()
            .item
            .connection
            .group_id,
        Some(group.id.clone())
    );
    assert_eq!(
        s.get_snippet(&snippet.id).unwrap().unwrap().item.content,
        "uptime"
    );
    assert_eq!(
        s.get_tunnel(&tunnel.id).unwrap().unwrap().item.bind_port,
        5433
    );
    assert_eq!(
        s.get_known_host(&known.id).unwrap().unwrap().item.key_blob,
        vec![1, 2, 3, 4]
    );
}

#[tokio::test]
async fn concurrent_edits_to_different_fields_both_survive() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);
    let connection = new_host("api");
    let id = connection.id.clone();
    a.store().save_host(&connection, None).unwrap();
    a.sync().await;
    b.sync().await;

    // Both edit offline, different fields.
    let mut on_a = a.host(&id).unwrap().item.connection;
    on_a.color = Some("red".into());
    a.store().save_host(&on_a, None).unwrap();
    let mut on_b = b.host(&id).unwrap().item.connection;
    on_b.notes = "deploys on friday".into();
    b.store().save_host(&on_b, None).unwrap();

    a.sync().await;
    let report = b.sync().await;
    assert_eq!(report.merged, 1, "B merged A's concurrent edit");
    a.sync().await;

    for dev in [&a, &b] {
        let got = dev.host(&id).unwrap().item.connection;
        assert_eq!(
            got.color.as_deref(),
            Some("red"),
            "{}",
            dev.store().device_id()
        );
        assert_eq!(got.notes, "deploys on friday");
    }
}

#[tokio::test]
async fn same_field_conflict_keeps_one_winner_and_the_loser_in_history() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);
    let connection = new_host("web");
    let id = connection.id.clone();
    a.store().save_host(&connection, None).unwrap();
    a.sync().await;
    b.sync().await;

    let mut on_a = a.host(&id).unwrap().item.connection;
    on_a.name = "web-from-a".into();
    a.store().save_host(&on_a, None).unwrap();
    let mut on_b = b.host(&id).unwrap().item.connection;
    on_b.name = "web-from-b".into();
    b.store().save_host(&on_b, None).unwrap();

    a.sync().await;
    b.sync().await;
    a.sync().await;

    let name_a = a.host(&id).unwrap().item.connection.name;
    let name_b = b.host(&id).unwrap().item.connection.name;
    assert_eq!(name_a, name_b, "both devices converge");
    // Equal Lamport clocks: the larger device id wins.
    assert_eq!(name_a, "web-from-b");

    // The loser is restorable from history on the device that lost it, as a
    // value of `name`.
    let history = a.store().history(&id).unwrap();
    assert!(
        history.iter().any(|entry| entry.field == "name"
            && entry.old_value == Some(serde_json::json!("web-from-a"))),
        "{history:?}"
    );
    // ... and restoring is an ordinary edit that syncs.
    let seq = history
        .iter()
        .find(|entry| entry.old_value == Some(serde_json::json!("web-from-a")))
        .unwrap()
        .seq;
    a.store().restore_host_field(&id, seq).unwrap();
    assert_eq!(a.host(&id).unwrap().item.connection.name, "web-from-a");
    a.sync().await;
    b.sync().await;
    assert_eq!(b.host(&id).unwrap().item.connection.name, "web-from-a");
}

#[tokio::test]
async fn a_remote_edit_that_replaces_a_local_value_writes_history() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);
    let connection = new_host("old-name");
    let id = connection.id.clone();
    a.store().save_host(&connection, None).unwrap();
    a.sync().await;
    b.sync().await;

    let mut edited = a.host(&id).unwrap().item.connection;
    edited.name = "new-name".into();
    a.store().save_host(&edited, None).unwrap();
    a.sync().await;
    let report = b.sync().await;
    assert_eq!(report.pulled, 1);
    assert_eq!(b.host(&id).unwrap().item.connection.name, "new-name");
    assert!(b.store().history(&id).unwrap().iter().any(
        |entry| entry.field == "name" && entry.old_value == Some(serde_json::json!("old-name"))
    ));
}

#[tokio::test]
async fn deletes_sync_and_a_later_restore_wins() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);
    let connection = new_host("gone");
    let id = connection.id.clone();
    a.store().save_host(&connection, None).unwrap();
    a.sync().await;
    b.sync().await;

    a.store().remove_host(&id).unwrap();
    a.sync().await;
    b.sync().await;
    assert!(b.host(&id).is_none(), "the delete reached B");
    assert_eq!(b.store().list_deleted_hosts().unwrap().len(), 1);

    // B brings it back; the restore outranks the delete everywhere.
    assert!(b.store().restore_host(&id).unwrap());
    b.sync().await;
    a.sync().await;
    assert!(a.host(&id).is_some());
}

#[tokio::test]
async fn cursor_record_tokens_and_high_water_survive_a_restart() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);
    let connection = new_host("persist");
    let id = connection.id.clone();
    a.store().save_host(&connection, None).unwrap();
    a.sync().await;
    b.sync().await;

    let personal = b.personal.clone();
    let cursor = b.store().sync_cursor(&personal).unwrap();
    assert!(cursor > 0);
    b.store()
        .save_high_water(b.engine.verifier().high_water())
        .unwrap();

    // A "restarted" B: a new engine over the same store, high water reloaded.
    let high_water = b.store().load_high_water().unwrap();
    assert!(high_water.get(&personal, &id).is_some());
    let verifier = Verifier::new(account().signing.public()).with_high_water(high_water);
    let mut b2 = Dev {
        engine: engine_for(&b.store, "dev-b", 11, &server, verifier, &personal),
        store: b.store.clone(),
        personal: personal.clone(),
    };
    let report = b2.sync().await;
    assert_eq!(
        report.pulled, 0,
        "the cursor was kept, nothing is pulled twice"
    );

    // Rollback protection survives the restart: a server that serves a
    // version older than one already seen is refused.
    let old = server.current(&personal, &id).unwrap();
    let mut edited = a.host(&id).unwrap().item.connection;
    edited.name = "v2".into();
    a.store().save_host(&edited, None).unwrap();
    a.sync().await;
    let report = b2.sync().await;
    assert_eq!(report.pulled, 1);
    b.store()
        .save_high_water(b2.engine.verifier().high_water())
        .unwrap();
    server.force_set(old);
    let high_water = b.store().load_high_water().unwrap();
    let verifier = Verifier::new(account().signing.public()).with_high_water(high_water);
    let mut b3 = Dev {
        engine: engine_for(&b.store, "dev-b", 11, &server, verifier, &personal),
        store: b.store.clone(),
        personal: personal.clone(),
    };
    let report = b3.sync().await;
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.rejected[0].reason, VerifyError::Rollback);
    assert_eq!(b3.host(&id).unwrap().item.connection.name, "v2");
}

#[tokio::test]
async fn team_vaults_are_separate_and_carry_a_name() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);
    let team = uuid::Uuid::new_v4().to_string();
    for d in [&mut a, &mut b] {
        d.engine.add_vault_key(&team, VaultKey::from_bytes([9; 32]));
    }
    a.store().create_team_vault(&team, "Platform team").unwrap();
    b.store()
        .ensure_team_vault(&team, "Shared vault", "editor")
        .unwrap();

    let connection = new_host("shared-host");
    let id = connection.id.clone();
    {
        let s = a.store();
        s.save_host(&connection, None).unwrap();
        // Move it into the team vault.
        let moved = s.move_host_to_vault(&id, &team).unwrap();
        assert_ne!(moved, id);
    }
    let moved_id = a
        .store()
        .list_hosts()
        .unwrap()
        .into_iter()
        .find(|r| r.meta.vault_id == team)
        .unwrap()
        .item
        .connection
        .id;

    let personal = a.personal.clone();
    a.engine.sync(&personal).await.unwrap();
    a.engine.sync(&team).await.unwrap();
    b.engine.sync(&personal).await.unwrap();
    b.engine.sync(&team).await.unwrap();

    // The personal vault of B has no trace of the shared host; the team one does.
    let on_b = b.store().get_host(&moved_id).unwrap().unwrap();
    assert_eq!(on_b.meta.vault_id, team);
    assert!(
        b.store().get_host(&id).unwrap().is_none(),
        "the original was removed"
    );
    let vault = b
        .store()
        .list_vaults()
        .unwrap()
        .into_iter()
        .find(|v| v.id == team)
        .unwrap();
    assert_eq!(vault.name, "Platform team", "the vault name travelled");
    assert_eq!(vault.role.as_deref(), Some("editor"));
}

#[tokio::test]
async fn an_invalid_remote_host_is_skipped_not_fatal() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 10, &server);
    let mut b = dev("dev-b", 11, &server);
    let mut evil = new_host("evil");
    evil.target = easyssh_core::ConnectionTarget::Alias {
        alias: "-oProxyCommand=touch".into(),
    };
    // Written straight into A's row, bypassing the daemon's validation.
    a.store().save_host(&evil, None).unwrap();
    a.sync().await;
    b.sync().await;
    assert!(b.host(&evil.id).is_none());
    // The sync still advanced.
    let personal = b.personal.clone();
    assert!(b.store().sync_cursor(&personal).unwrap() > 0);
}

/// Settings are device-local: the AI keys (`ai_key.*`, sealed by the vault) and
/// the session log preference never leave the device, and no log file is a
/// synced record. Only the five entity tables are mapped.
#[tokio::test]
async fn settings_and_ai_keys_are_not_synced() {
    let server = MemoryServer::new();
    let mut a = dev("dev-a", 20, &server);
    let mut b = dev("dev-b", 21, &server);
    {
        let store = a.store();
        store
            .set_setting("ai_key.anthropic", &serde_json::json!("SEALED-API-KEY"))
            .unwrap();
        store
            .set_setting("session_logs", &serde_json::json!({"enabled": true}))
            .unwrap();
        store.save_host(&new_host("Web"), None).unwrap();
    }
    let report = a.sync().await;
    assert_eq!(report.pushed, 1, "only the host is a record");
    b.sync().await;
    let store = b.store();
    assert!(store.get_setting("ai_key.anthropic").unwrap().is_none());
    assert!(store.get_setting("session_logs").unwrap().is_none());
}

#[test]
fn account_secrets_are_sealed_and_cleared_with_sync_state() {
    let store = Store::open_in_memory("dev-a", Arc::new(PlaintextForTests)).unwrap();
    store
        .set_sync_account(
            "https://sync.example.org",
            "ann",
            b"{\"token\":\"TOP-SECRET\"}",
        )
        .unwrap();
    let row = store.sync_account().unwrap().unwrap();
    assert_eq!(row.username, "ann");
    assert_eq!(
        store.open_sync_secrets(&row).unwrap(),
        b"{\"token\":\"TOP-SECRET\"}"
    );
    store.set_sync_cursor("v", 5).unwrap();
    store.set_sync_pin("acct", "bob", "00000", false).unwrap();
    store.clear_sync().unwrap();
    assert!(store.sync_account().unwrap().is_none());
    assert_eq!(store.sync_cursor("v").unwrap(), 0);
    assert!(store.sync_pin("acct").unwrap().is_none());
    // A locked cipher refuses to store them at all.
    let locked = Store::open_in_memory("dev-a", Arc::new(NotConfigured)).unwrap();
    assert!(locked.set_sync_account("https://x", "u", b"x").is_err());
}

#[test]
fn personal_vault_ids_differ_per_account_and_are_stable() {
    assert_eq!(
        personal_vault_server_id("one"),
        personal_vault_server_id("one")
    );
    assert_ne!(
        personal_vault_server_id("one"),
        personal_vault_server_id("two")
    );
}

#[test]
fn sync_store_trait_reads_what_the_store_holds() {
    let store = open_store("dev-a");
    let personal = personal_vault_server_id("acct-1");
    let connection = new_host("direct");
    store.lock().unwrap().save_host(&connection, None).unwrap();
    let sync = StoreSync::new(store.clone(), &personal);
    let dirty = sync.dirty(&personal).unwrap();
    assert_eq!(dirty.len(), 1);
    assert_eq!(dirty[0].record_id, connection.id);
    assert!(dirty[0].server_vector.is_none());
    assert!(!dirty[0].version.fields.contains_key("password"));
    assert_eq!(dirty[0].version.table, "hosts");
    let got = sync.get(&personal, &connection.id).unwrap().unwrap();
    assert_eq!(got.version, dirty[0].version);
    // Another vault's id does not find it.
    assert!(sync.get("someone-elses", &connection.id).unwrap().is_none());
}
