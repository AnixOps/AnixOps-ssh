//! Two devices of one account syncing through the in-memory server.

use easyssh_crypto::{certify_device, AccountKeyPair, DeviceSigningKey, VaultKey};
use easyssh_sync::wire::device_id_for;
use easyssh_sync::{
    DeviceIdentity, MemoryServer, MemoryStore, MemoryTransport, SyncEngine, SyncError, Verifier,
    VerifyError,
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
    let cert = certify_device(&account.signing, &key.public(), "device", 1_700_000_000);
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
    e.add_vault_key(VAULT, VaultKey::from_bytes([7; 32]));
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
    b.add_vault_key(VAULT, key); // wrong key replaces the right one
    let err = b.sync(VAULT).await.unwrap_err();
    assert!(matches!(err, SyncError::Crypto(_)), "{err:?}");
}
