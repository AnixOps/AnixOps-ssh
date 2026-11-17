//! Two devices of one account syncing through the in-memory server.

use easyssh_crypto::{certify_device, AccountKeyPair, DeviceSigningKey, VaultKey};
use easyssh_sync::wire::{device_id_for, ErrorCode, MemberKey};
use easyssh_sync::SyncStore;
use easyssh_sync::{
    DeviceIdentity, MemoryServer, MemoryStore, MemoryTransport, RotationFailure, SyncEngine,
    SyncError, Verifier, VerifyError,
};
use serde_json::{json, Value};

const VAULT: &str = "vault-1";
const REC: &str = "host-1";

fn account() -> AccountKeyPair {
    AccountKeyPair::from_seeds([1; 32], [2; 32])
}

fn engine(
    account: &AccountKeyPair,
    server: &MemoryServer,
    seed: u8,
) -> SyncEngine<MemoryTransport, MemoryStore> {
    let key = DeviceSigningKey::from_seed([seed; 32]);
    let cert = certify_device(
        &account.signing,
        &key.public(),
        "device",
        1_700_000_000,
        4_000_000_000,
    );
    let identity = DeviceIdentity {
        device_id: device_id_for(&key.public()),
        signing_key: key,
        cert,
    };
    let mut e = SyncEngine::new(
        server.transport(),
        MemoryStore::new(),
        identity,
        Verifier::new(account.signing.public()),
    );
    e.add_vault_key(VAULT, 1, VaultKey::from_bytes([7; 32]));
    e
}

fn edit(e: &mut SyncEngine<MemoryTransport, MemoryStore>, changes: &[(&str, Value)]) {
    e.edit_record(
        VAULT,
        REC,
        "hosts",
        changes.iter().map(|(k, v)| ((*k).to_owned(), v.clone())),
    )
    .unwrap();
}

fn field(e: &SyncEngine<MemoryTransport, MemoryStore>, name: &str) -> Value {
    e.store()
        .record(VAULT, REC)
        .unwrap()
        .version
        .fields
        .get(name)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn create_pull_and_edit_round_trip() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);

    edit(
        &mut a,
        &[("name", json!("web")), ("host", json!("10.0.0.1"))],
    );
    let r = a.sync(VAULT).await.unwrap();
    assert_eq!(r.pushed, 1);

    let r = b.sync(VAULT).await.unwrap();
    assert_eq!(r.pulled, 1);
    assert_eq!(field(&b, "name"), json!("web"));
    assert!(!b.store().record(VAULT, REC).unwrap().is_dirty());

    edit(&mut b, &[("name", json!("web-2"))]);
    b.sync(VAULT).await.unwrap();
    a.sync(VAULT).await.unwrap();
    assert_eq!(field(&a, "name"), json!("web-2"));
    assert_eq!(field(&a, "host"), json!("10.0.0.1"));

    // Nothing left to do.
    let r = a.sync(VAULT).await.unwrap();
    assert_eq!((r.pushed, r.merged, r.conflicts), (0, 0, 0));
}

#[tokio::test]
async fn concurrent_edits_merge_field_by_field_and_converge() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit(&mut a, &[("name", json!("web")), ("port", json!(22))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();

    // Both go offline and edit different fields plus one shared field.
    edit(
        &mut a,
        &[("name", json!("web-a")), ("note", json!("from a"))],
    );
    edit(&mut b, &[("port", json!(2222)), ("name", json!("web-b"))]);

    // b syncs first and wins the race; a's push hits a conflict and merges.
    b.sync(VAULT).await.unwrap();
    let ra = a.sync(VAULT).await.unwrap();
    assert!(ra.merged >= 1, "{ra:?}");
    assert_eq!(ra.pushed, 1);
    b.sync(VAULT).await.unwrap();

    for e in [&a, &b] {
        assert_eq!(field(e, "port"), json!(2222), "b's edit survives");
        assert_eq!(field(e, "note"), json!("from a"), "a's edit survives");
        assert!(!e.store().record(VAULT, REC).unwrap().is_dirty());
    }
    // Same-field conflict: both devices agree on one winner and the
    // loser is in history on the device that overwrote it.
    assert_eq!(field(&a, "name"), field(&b, "name"));
    let winner = field(&a, "name");
    let loser = if winner == json!("web-a") {
        json!("web-b")
    } else {
        json!("web-a")
    };
    let lost: Vec<_> = a
        .store()
        .history
        .iter()
        .chain(b.store().history.iter())
        .filter(|(_, _, o)| o.field == "name")
        .collect();
    assert!(
        lost.iter().any(|(_, _, o)| o.value == loser),
        "overwritten value reported to history: {lost:?}"
    );
}

#[tokio::test]
async fn pushes_race_until_merged_without_loss() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit(&mut a, &[("x", json!(0))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();
    edit(&mut a, &[("a", json!(1))]);
    edit(&mut b, &[("b", json!(2))]);
    a.sync(VAULT).await.unwrap();
    let r = b.sync(VAULT).await.unwrap();
    // b pulled a's version first (so merged locally) and then pushed.
    assert_eq!(r.pushed, 1);
    a.sync(VAULT).await.unwrap();
    assert_eq!(field(&a, "a"), json!(1));
    assert_eq!(field(&a, "b"), json!(2));
    assert_eq!(field(&b, "a"), json!(1));
    assert_eq!(field(&b, "b"), json!(2));
}

#[tokio::test]
async fn push_conflict_returns_server_copy_and_client_merges_and_retries() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit(&mut a, &[("x", json!(0))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();
    edit(&mut a, &[("a", json!(1))]);
    a.push(VAULT).await.unwrap();
    // b has not pulled; it pushes straight away and hits the conflict path.
    edit(&mut b, &[("b", json!(2))]);
    let r = b.push(VAULT).await.unwrap();
    assert_eq!(r.conflicts, 1);
    assert_eq!(r.pushed, 1);
    assert_eq!(field(&b, "a"), json!(1));
    a.sync(VAULT).await.unwrap();
    assert_eq!(field(&a, "b"), json!(2));
}

#[tokio::test]
async fn delete_is_a_signed_tombstone_and_edit_after_delete_revives() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit(&mut a, &[("name", json!("web"))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();

    a.delete_record(VAULT, REC).unwrap();
    a.sync(VAULT).await.unwrap();
    assert!(server.current(VAULT, REC).unwrap().deleted);
    b.sync(VAULT).await.unwrap();
    assert!(b.store().record(VAULT, REC).unwrap().version.is_deleted());

    edit(&mut b, &[("name", json!("back"))]);
    b.sync(VAULT).await.unwrap();
    a.sync(VAULT).await.unwrap();
    assert!(!a.store().record(VAULT, REC).unwrap().version.is_deleted());
    assert_eq!(field(&a, "name"), json!("back"));
}

#[tokio::test]
async fn concurrent_delete_and_older_edit_deletes() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit(&mut a, &[("name", json!("web"))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();

    // a edits (lamport 2); b edits then deletes (lamport 3): delete is newest.
    edit(&mut a, &[("note", json!("x"))]);
    edit(&mut b, &[("port", json!(1))]);
    b.delete_record(VAULT, REC).unwrap();
    b.sync(VAULT).await.unwrap();
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();
    for e in [&a, &b] {
        assert!(e.store().record(VAULT, REC).unwrap().version.is_deleted());
        assert_eq!(field(e, "note"), json!("x"), "edit kept for history");
    }
}

#[tokio::test]
async fn history_lists_replaced_versions_and_restore_works() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    edit(&mut a, &[("name", json!("one"))]);
    a.sync(VAULT).await.unwrap();
    edit(&mut a, &[("name", json!("two"))]);
    a.sync(VAULT).await.unwrap();
    edit(&mut a, &[("name", json!("three"))]);
    a.sync(VAULT).await.unwrap();

    let history = a.history(VAULT, REC).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version.fields["name"], json!("two"));
    assert_eq!(history[1].version.fields["name"], json!("one"));
    assert_eq!(history[0].device_name, "device");

    a.restore_field(VAULT, REC, &history[1], "name").unwrap();
    a.sync(VAULT).await.unwrap();
    assert_eq!(field(&a, "name"), json!("one"));
}

#[tokio::test]
async fn rollback_by_the_server_is_rejected() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit(&mut a, &[("name", json!("old"))]);
    a.sync(VAULT).await.unwrap();
    let old = server.current(VAULT, REC).unwrap();
    edit(&mut a, &[("name", json!("new"))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();
    assert_eq!(field(&b, "name"), json!("new"));

    // The server replays the old, validly signed envelope.
    server.force_set(old);
    let r = b.sync(VAULT).await.unwrap();
    assert_eq!(r.rejected.len(), 1);
    assert_eq!(r.rejected[0].reason, VerifyError::Rollback);
    assert_eq!(field(&b, "name"), json!("new"));
}

#[tokio::test]
async fn forged_and_foreign_envelopes_are_rejected_without_stopping_the_sync() {
    let acct = account();
    let other = AccountKeyPair::from_seeds([9; 32], [8; 32]);
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut stranger = engine(&other, &server, 12);
    edit(&mut a, &[("name", json!("mine"))]);
    a.sync(VAULT).await.unwrap();
    // A different account's device writes into the same vault (the server
    // here has no access control); our client must not accept it.
    stranger
        .edit_record(VAULT, "evil", "hosts", [("name".to_owned(), json!("evil"))])
        .unwrap();
    stranger.sync(VAULT).await.unwrap();

    let mut b = engine(&acct, &server, 11);
    let r = b.sync(VAULT).await.unwrap();
    assert_eq!(r.pulled, 1);
    assert_eq!(r.rejected.len(), 1);
    assert_eq!(r.rejected[0].reason, VerifyError::UntrustedCertificate);
    assert!(b.store().record(VAULT, "evil").is_none());

    // Tampered ciphertext on the server.
    let mut env = server.current(VAULT, REC).unwrap();
    env.ciphertext[40] ^= 1;
    server.force_set(env);
    let mut c = engine(&acct, &server, 13);
    let r = c.sync(VAULT).await.unwrap();
    assert!(r
        .rejected
        .iter()
        .any(|x| x.reason == VerifyError::BadSignature));
}

#[tokio::test]
async fn missing_vault_key_is_an_error() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    edit(&mut a, &[("name", json!("x"))]);
    a.sync(VAULT).await.unwrap();
    let mut b = engine(&acct, &server, 11);
    let key = VaultKey::from_bytes([1; 32]);
    b.add_vault_key(VAULT, 1, key); // wrong key replaces the right one
    let err = b.sync(VAULT).await.unwrap_err();
    assert!(matches!(err, SyncError::Crypto(_)), "{err:?}");
}

fn edit_id(e: &mut SyncEngine<MemoryTransport, MemoryStore>, id: &str, changes: &[(&str, Value)]) {
    e.edit_record(
        VAULT,
        id,
        "hosts",
        changes.iter().map(|(k, v)| ((*k).to_owned(), v.clone())),
    )
    .unwrap();
}

#[tokio::test]
async fn first_upload_goes_in_batches_and_falls_back_on_conflict() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    for i in 0..250 {
        edit_id(&mut a, &format!("h{i:03}"), &[("name", json!(i))]);
    }
    let r = a.sync(VAULT).await.unwrap();
    assert_eq!(r.pushed, 250);
    assert_eq!(r.conflicts, 0);
    assert!(a.store().dirty(VAULT).unwrap().is_empty());
    let mut b = engine(&acct, &server, 11);
    assert_eq!(b.sync(VAULT).await.unwrap().pulled, 250);

    // A device that created the same ids offline: the batch is refused as a
    // whole, nothing is half applied, and the per-record path merges.
    let mut c = engine(&acct, &server, 12);
    edit_id(&mut c, "h005", &[("note", json!("c"))]);
    edit_id(&mut c, "fresh-1", &[("name", json!("new"))]);
    edit_id(&mut c, "fresh-2", &[("name", json!("new"))]);
    let r = c.push(VAULT).await.unwrap();
    assert_eq!(r.pushed, 3);
    assert!(r.conflicts >= 1, "{r:?}");
    assert_eq!(
        c.store().record(VAULT, "h005").unwrap().version.fields["note"],
        json!("c")
    );
    assert_eq!(
        c.store().record(VAULT, "h005").unwrap().version.fields["name"],
        json!(5)
    );
    assert!(server.current(VAULT, "fresh-2").is_some());
}

fn sealed_for_nobody() -> Vec<MemberKey> {
    Vec::new()
}

#[tokio::test]
async fn rotation_reencrypts_everything_and_old_keys_stop_working() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit_id(&mut a, "one", &[("name", json!("1"))]);
    edit_id(&mut a, "two", &[("name", json!("2"))]);
    a.edit_record(VAULT, "gone", "hosts", [("name".to_owned(), json!("x"))])
        .unwrap();
    a.delete_record(VAULT, "gone").unwrap();
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();
    let before = server.current(VAULT, "one").unwrap();
    assert_eq!(before.key_generation, 1);

    let new_key = VaultKey::from_bytes([42; 32]);
    let generation = a
        .rotate_vault_key(VAULT, new_key, None, sealed_for_nobody())
        .await
        .unwrap();
    assert_eq!(generation, 2);
    assert_eq!(server.key_generation(VAULT), 2);
    for id in ["one", "two", "gone"] {
        let env = server.current(VAULT, id).unwrap();
        assert_eq!(env.key_generation, 2, "{id}");
        assert_ne!(env.ciphertext, before.ciphertext);
    }
    // New version, signed by the rotating device, same content.
    let after = server.current(VAULT, "one").unwrap();
    assert!(after.vector().get(a.device_id()) > before.vector().get(a.device_id()));
    assert!(!a.store().record(VAULT, "one").unwrap().is_dirty());

    // A device that still has generation 1 only: it can neither read nor
    // write; both fail in a way that says "fetch the new key".
    let err = b.pull(VAULT).await.unwrap_err();
    assert!(err.is_stale_key(), "{err:?}");
    edit_id(&mut b, "one", &[("name", json!("b"))]);
    let err = b.push(VAULT).await.unwrap_err();
    assert!(err.is_stale_key(), "{err:?}");
    match err {
        SyncError::Transport(t) => assert_eq!(t.code(), Some(ErrorCode::StaleKeyGeneration)),
        other => panic!("{other:?}"),
    }

    // With the new key it catches up, keeps its pending edit and merges.
    b.add_vault_key(VAULT, 2, VaultKey::from_bytes([42; 32]));
    let r = b.sync(VAULT).await.unwrap();
    assert_eq!(r.pulled, 3);
    assert!(r.rejected.is_empty());
    assert_eq!(field_of(&b, "one", "name"), json!("b"));
    assert_eq!(server.current(VAULT, "one").unwrap().key_generation, 2);
    assert_eq!(field_of(&b, "two", "name"), json!("2"));
    assert!(b
        .store()
        .record(VAULT, "gone")
        .unwrap()
        .version
        .is_deleted());
}

fn field_of(e: &SyncEngine<MemoryTransport, MemoryStore>, id: &str, name: &str) -> Value {
    e.store()
        .record(VAULT, id)
        .unwrap()
        .version
        .fields
        .get(name)
        .cloned()
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn rotation_that_races_with_a_push_is_refused_whole_and_can_be_retried() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit_id(&mut a, "one", &[("name", json!("1"))]);
    edit_id(&mut a, "two", &[("name", json!("2"))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();

    // b changes a record after a last synced.
    edit_id(&mut b, "two", &[("name", json!("2b"))]);
    b.sync(VAULT).await.unwrap();

    let err = a
        .rotate_vault_key(VAULT, VaultKey::from_bytes([5; 32]), None, Vec::new())
        .await
        .unwrap_err();
    match err {
        SyncError::Rotation(RotationFailure::Conflict(ids)) => assert_eq!(ids, vec!["two"]),
        other => panic!("{other:?}"),
    }
    // Nothing was written: generation and records are as before.
    assert_eq!(server.key_generation(VAULT), 1);
    assert_eq!(server.current(VAULT, "one").unwrap().key_generation, 1);
    assert_eq!(server.current(VAULT, "two").unwrap().key_generation, 1);

    a.sync(VAULT).await.unwrap();
    let generation = a
        .rotate_vault_key(VAULT, VaultKey::from_bytes([5; 32]), None, Vec::new())
        .await
        .unwrap();
    assert_eq!(generation, 2);
    assert_eq!(field_of(&a, "two", "name"), json!("2b"));
}

#[tokio::test]
async fn rotation_cannot_skip_records_the_device_does_not_have() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    edit_id(&mut a, "one", &[("name", json!("1"))]);
    a.sync(VAULT).await.unwrap();
    // Another device adds a record a has not pulled.
    let mut b = engine(&acct, &server, 11);
    edit_id(&mut b, "two", &[("name", json!("2"))]);
    b.sync(VAULT).await.unwrap();
    let err = a
        .rotate_vault_key(VAULT, VaultKey::from_bytes([5; 32]), None, Vec::new())
        .await
        .unwrap_err();
    match err {
        SyncError::Rotation(RotationFailure::Missing(ids)) => assert_eq!(ids, vec!["two"]),
        other => panic!("{other:?}"),
    }
    assert_eq!(server.key_generation(VAULT), 1);
}

#[tokio::test]
async fn a_second_rotation_by_a_stale_device_is_refused() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    let mut b = engine(&acct, &server, 11);
    edit_id(&mut a, "one", &[("name", json!("1"))]);
    a.sync(VAULT).await.unwrap();
    b.sync(VAULT).await.unwrap();
    a.rotate_vault_key(VAULT, VaultKey::from_bytes([5; 32]), None, Vec::new())
        .await
        .unwrap();
    let err = b
        .rotate_vault_key(VAULT, VaultKey::from_bytes([6; 32]), None, Vec::new())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            SyncError::Rotation(RotationFailure::Generation { current: 2 })
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn envelopes_from_a_revoked_device_are_rejected_only_if_received_after_revocation() {
    let acct = account();
    let server = MemoryServer::new();
    let mut a = engine(&acct, &server, 10);
    server.set_time(1_000);
    edit_id(&mut a, "early", &[("name", json!("early"))]);
    a.sync(VAULT).await.unwrap();
    server.set_time(3_000);
    edit_id(&mut a, "late", &[("name", json!("late"))]);
    a.sync(VAULT).await.unwrap();

    let mut b = engine(&acct, &server, 11);
    let revoked = DeviceSigningKey::from_seed([10; 32]).public();
    b.verifier_mut().revoke_device(revoked, 2_000);
    let r = b.sync(VAULT).await.unwrap();
    assert_eq!(r.pulled, 1);
    assert_eq!(r.rejected.len(), 1);
    assert_eq!(r.rejected[0].record_id, "late");
    assert_eq!(r.rejected[0].reason, VerifyError::DeviceRevoked);
    assert!(b.store().record(VAULT, "early").is_some());
    assert!(b.store().record(VAULT, "late").is_none());
}
