//! End-to-end tests: a real server on a loopback port, real HTTP clients.

use easyssh_crypto::{
    certify_device, AccountKeyPair, DeviceSigningKey, KdfParams, VaultKey, CERT_LIFETIME_SECS,
};
use easyssh_server::{serve, Config};
use easyssh_sync::account::{
    create_vault, enroll_device, login, open_vault, register, renew_device_cert, seal_for_members,
    share_vault, trust_members, trust_revocations, Account,
};
use easyssh_sync::wire::*;
use easyssh_sync::{
    seal_record, AuthError, DeviceIdentity, HttpClient, MemoryStore, RecordVersion,
    RotationFailure, SyncEngine, SyncError, Transport, Verifier, VerifyError,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

const PASS: &[u8] = b"correct horse battery staple";

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

/// A certificate valid for the usual lifetime from now.
fn cert_for(account: &Account, key: &DeviceSigningKey, name: &str) -> easyssh_crypto::DeviceCert {
    let now = unix_now();
    certify_device(
        &account.keys.signing,
        &key.public(),
        name,
        now,
        now + CERT_LIFETIME_SECS,
    )
}

struct TestServer {
    base: String,
    dir: TempDir,
}

async fn start_with(edit: impl FnOnce(&mut Config)) -> TestServer {
    let dir = tempfile::tempdir().unwrap();
    let mut config = Config::for_db(dir.path().join("server.db"));
    config.auth_rate_per_minute = 10_000;
    edit(&mut config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, config, std::future::pending()));
    TestServer { base, dir }
}

async fn start() -> TestServer {
    start_with(|_| {}).await
}

impl TestServer {
    fn client(&self) -> HttpClient {
        HttpClient::new(&self.base).unwrap()
    }

    /// All bytes of every database file (main, WAL, journal).
    fn db_bytes(&self) -> Vec<u8> {
        let mut all = Vec::new();
        for entry in std::fs::read_dir(self.dir.path()).unwrap() {
            all.extend(std::fs::read(entry.unwrap().path()).unwrap());
        }
        all
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

async fn signup(server: &TestServer, name: &str) -> (HttpClient, Account) {
    let client = server.client();
    register(&client, name, PASS, KdfParams::insecure_for_tests())
        .await
        .unwrap();
    let account = login(&client, name, PASS).await.unwrap();
    (client, account)
}

/// Another device of an existing account: fresh session.
async fn second_device(server: &TestServer, name: &str) -> (HttpClient, Account) {
    let client = server.client();
    let account = login(&client, name, PASS).await.unwrap();
    (client, account)
}

/// A device key that can be re-materialised (the identity is not Clone).
#[derive(Clone, Copy)]
struct Dev {
    seed: u8,
    name: &'static str,
}

impl Dev {
    fn key(&self) -> DeviceSigningKey {
        DeviceSigningKey::from_seed([self.seed; 32])
    }

    fn identity(&self, account: &Account) -> DeviceIdentity {
        let key = self.key();
        let cert = cert_for(account, &key, self.name);
        DeviceIdentity {
            device_id: device_id_for(&key.public()),
            signing_key: key,
            cert,
        }
    }

    async fn enroll(&self, client: &HttpClient, account: &Account) -> DeviceIdentity {
        enroll_device(client, account, self.name, self.key(), unix_now())
            .await
            .unwrap()
    }
}

type Engine = SyncEngine<HttpClient, MemoryStore>;

fn engine(
    client: &HttpClient,
    account: &Account,
    identity: DeviceIdentity,
    vault: &str,
    key: VaultKey,
) -> Engine {
    let mut e = SyncEngine::new(
        client.clone(),
        MemoryStore::new(),
        identity,
        Verifier::new(account.keys.signing.public()),
    );
    e.add_vault_key(vault, 1, key);
    e
}

fn edit(e: &mut Engine, vault: &str, record: &str, changes: &[(&str, Value)]) {
    e.edit_record(
        vault,
        record,
        "hosts",
        changes.iter().map(|(k, v)| ((*k).to_owned(), v.clone())),
    )
    .unwrap();
}

fn field(e: &Engine, vault: &str, record: &str, name: &str) -> Value {
    e.store()
        .record(vault, record)
        .map(|r| r.version.fields.get(name).cloned().unwrap_or(Value::Null))
        .unwrap_or(Value::Null)
}

fn transport_code(err: SyncError) -> Option<ErrorCode> {
    match err {
        SyncError::Transport(t) => t.code(),
        other => panic!("expected a server error, got {other:?}"),
    }
}

fn copy_key(key: &VaultKey) -> VaultKey {
    VaultKey::from_bytes(*key.expose())
}

#[tokio::test(flavor = "multi_thread")]
async fn opaque_sign_in_and_wrong_passphrase() {
    let server = start().await;
    let client = server.client();
    let created = register(&client, "alice", PASS, KdfParams::insecure_for_tests())
        .await
        .unwrap();

    // Right passphrase: the same key pairs come back from the wrapped blob.
    let account = login(&client, "alice", PASS).await.unwrap();
    assert_eq!(account.account_id, created.account_id);
    assert_eq!(account.keys.signing.public(), created.keys.signing.public());
    assert!(client.has_token());

    // Wrong passphrase and unknown user fail the same way, client side.
    let other = server.client();
    let err = login(&other, "alice", b"wrong passphrase")
        .await
        .unwrap_err();
    assert!(matches!(err, AuthError::Opaque), "{err:?}");
    assert!(!other.has_token());
    let err = login(&other, "nobody", PASS).await.unwrap_err();
    assert!(matches!(err, AuthError::Opaque), "{err:?}");

    // Duplicate registration is refused.
    let err = register(
        &server.client(),
        "Alice",
        PASS,
        KdfParams::insecure_for_tests(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, AuthError::Transport(t) if t.code() == Some(ErrorCode::Conflict)),
        "{err:?}"
    );

    // Requests without a valid token are rejected; logout ends the session.
    let anon = server.client();
    assert_eq!(
        anon.list_devices().await.unwrap_err().code(),
        Some(ErrorCode::Unauthorized)
    );
    client.list_devices().await.unwrap();
    let copied = server.client();
    copied.set_token(client.token());
    copied.list_devices().await.unwrap();
    client.logout().await.unwrap();
    assert_eq!(
        copied.list_devices().await.unwrap_err().code(),
        Some(ErrorCode::Unauthorized),
        "logout invalidates the token on the server"
    );
    let forged = server.client();
    forged.set_token(Some("es_not-a-real-token".into()));
    assert_eq!(
        forged.list_devices().await.unwrap_err().code(),
        Some(ErrorCode::Unauthorized)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn registration_can_be_closed() {
    let server = start_with(|c| c.allow_registration = false).await;
    let err = register(
        &server.client(),
        "alice",
        PASS,
        KdfParams::insecure_for_tests(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, AuthError::Transport(t) if t.code() == Some(ErrorCode::RegistrationClosed)),
        "{err:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_endpoints_are_rate_limited() {
    let server = start_with(|c| c.auth_rate_per_minute = 5).await;
    let client = server.client();
    let mut limited = false;
    for _ in 0..8 {
        if let Err(e) = client.kdf_header("alice").await {
            assert_eq!(e.code(), Some(ErrorCode::RateLimited));
            limited = true;
        }
    }
    assert!(limited, "the sixth request in a minute must be refused");
}

#[tokio::test(flavor = "multi_thread")]
async fn device_certificate_is_required() {
    let server = start().await;
    let (client, alice) = signup(&server, "alice").await;
    let vault = "v-certs";
    let key = create_vault(&client, &alice, vault).await.unwrap();

    // A device key nobody registered cannot push, even with a valid cert.
    let rogue = Dev {
        seed: 99,
        name: "rogue",
    }
    .identity(&alice);
    let mut e = engine(&client, &alice, rogue, vault, key);
    edit(&mut e, vault, "r1", &[("name", json!("x"))]);
    let err = e.sync(vault).await.unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::DeviceNotRegistered));

    // Registering a device with a certificate from another account fails.
    let mallory = AccountKeyPair::generate().unwrap();
    let dev = DeviceSigningKey::generate().unwrap();
    let far = unix_now() + CERT_LIFETIME_SECS;
    let bad_cert = certify_device(&mallory.signing, &dev.public(), "bad", 1, far);
    let err = client
        .register_device(&RegisterDeviceRequest {
            device_public: dev.public().to_vec(),
            name: "bad".into(),
            cert: bad_cert.to_bytes(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::InvalidSignature));

    // A certificate for a different key than the one presented is refused.
    let good = certify_device(&alice.keys.signing, &dev.public(), "ok", 1, far);
    let other = DeviceSigningKey::generate().unwrap();
    let err = client
        .register_device(&RegisterDeviceRequest {
            device_public: other.public().to_vec(),
            name: "mismatch".into(),
            cert: good.to_bytes(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::BadRequest));
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_edit_concurrently_and_merge_without_loss() {
    let server = start().await;
    let (client_a, alice) = signup(&server, "alice").await;
    let vault = "v-merge";
    let key = create_vault(&client_a, &alice, vault).await.unwrap();
    let dev_a = Dev {
        seed: 1,
        name: "laptop",
    };
    let id_a = dev_a.enroll(&client_a, &alice).await;

    let (client_b, alice_b) = second_device(&server, "alice").await;
    let id_b = Dev {
        seed: 2,
        name: "phone",
    }
    .enroll(&client_b, &alice_b)
    .await;
    let (key_b, generation) = open_vault(&client_b, &alice_b, vault).await.unwrap();
    assert_eq!((key_b.expose(), generation), (key.expose(), 1));

    let mut a = engine(&client_a, &alice, id_a, vault, copy_key(&key));
    let mut b = engine(&client_b, &alice_b, id_b, vault, key_b);

    edit(
        &mut a,
        vault,
        "h1",
        &[
            ("name", json!("db-prod")),
            ("host", json!("203.0.113.9")),
            ("port", json!(22)),
        ],
    );
    a.sync(vault).await.unwrap();
    b.sync(vault).await.unwrap();
    assert_eq!(field(&b, vault, "h1", "host"), json!("203.0.113.9"));

    // Offline edits on both devices: different fields plus one shared field.
    edit(
        &mut a,
        vault,
        "h1",
        &[("name", json!("db-prod-a")), ("user", json!("root"))],
    );
    edit(
        &mut b,
        vault,
        "h1",
        &[("port", json!(2222)), ("name", json!("db-prod-b"))],
    );

    // b pushes first. a's push of the same base is now stale: the server
    // answers with a conflict carrying its copy instead of storing it.
    b.sync(vault).await.unwrap();
    let rec = a.store().record(vault, "h1").unwrap().clone();
    let envelope =
        easyssh_sync::seal_record(&dev_a.identity(&alice), &key, 1, vault, "h1", &rec.version)
            .unwrap();
    let expected = rec
        .server_vector
        .as_ref()
        .map(|v| v.iter().map(|(d, c)| (d.to_owned(), c)).collect())
        .unwrap_or_default();
    match client_a
        .push(vault, &PushRequest { envelope, expected })
        .await
        .unwrap()
    {
        PushResponse::Conflict { reason, current } => {
            assert_eq!(reason, ConflictReason::Cas);
            assert!(current.is_some());
        }
        other => panic!("stale push must conflict, got {other:?}"),
    }

    // The engine merges the server copy and retries.
    let report = a.sync(vault).await.unwrap();
    assert!(report.merged >= 1, "{report:?}");
    assert_eq!(report.pushed, 1);
    b.sync(vault).await.unwrap();
    a.sync(vault).await.unwrap();

    for e in [&a, &b] {
        assert_eq!(field(e, vault, "h1", "port"), json!(2222));
        assert_eq!(field(e, vault, "h1", "user"), json!("root"));
        assert_eq!(field(e, vault, "h1", "host"), json!("203.0.113.9"));
        assert!(!e.store().record(vault, "h1").unwrap().is_dirty());
    }
    let name_a = field(&a, vault, "h1", "name");
    assert_eq!(name_a, field(&b, vault, "h1", "name"));
    assert!(name_a == json!("db-prod-a") || name_a == json!("db-prod-b"));
    // The losing name was reported for history on one of the devices.
    let lost = a
        .store()
        .history
        .iter()
        .chain(b.store().history.iter())
        .any(|(_, _, o)| o.field == "name" && o.value != name_a);
    assert!(lost, "overwritten name must be reported to history");

    // The server's history holds replaced versions, decryptable by clients.
    let hist = a.history(vault, "h1").await.unwrap();
    assert!(hist
        .iter()
        .any(|h| h.version.fields.get("name") == Some(&json!("db-prod"))));
}

#[tokio::test(flavor = "multi_thread")]
async fn history_keeps_fifty_versions() {
    let server = start().await;
    let (client, alice) = signup(&server, "alice").await;
    let vault = "v-hist";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let id = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client, &alice)
    .await;
    let mut e = engine(&client, &alice, id, vault, key);
    for i in 0..55 {
        edit(&mut e, vault, "r", &[("n", json!(i))]);
        e.sync(vault).await.unwrap();
    }
    let hist = e.history(vault, "r").await.unwrap();
    assert_eq!(hist.len(), 50);
    assert_eq!(
        hist[0].version.fields["n"],
        json!(53),
        "newest replaced first"
    );
    assert_eq!(hist[49].version.fields["n"], json!(4));
}

#[tokio::test(flavor = "multi_thread")]
async fn sharing_roles_and_membership() {
    let server = start().await;
    let (alice_client, alice) = signup(&server, "alice").await;
    let (bob_client, bob) = signup(&server, "bob").await;
    let (_carol_client, _carol) = signup(&server, "carol").await;
    let vault = "v-team";
    let key = create_vault(&alice_client, &alice, vault).await.unwrap();
    let alice_id = Dev {
        seed: 1,
        name: "alice-laptop",
    }
    .enroll(&alice_client, &alice)
    .await;
    let bob_dev = Dev {
        seed: 2,
        name: "bob-laptop",
    };
    let bob_id = bob_dev.enroll(&bob_client, &bob).await;

    let mut ea = engine(&alice_client, &alice, alice_id, vault, copy_key(&key));
    edit(&mut ea, vault, "h1", &[("name", json!("shared host"))]);
    ea.sync(vault).await.unwrap();

    // Before sharing, bob cannot see the vault at all.
    let err = bob_client.pull(vault, 0, 10).await.unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::NotFound));
    assert!(bob_client.list_vaults().await.unwrap().is_empty());

    // Share as viewer: the key is sealed to bob; the server cannot open it.
    share_vault(&alice_client, vault, &key, 1, "bob", Role::Viewer)
        .await
        .unwrap();
    let (bob_key, _) = open_vault(&bob_client, &bob, vault).await.unwrap();
    assert_eq!(bob_key, key);

    // Without trusting alice's account, bob rejects her signed envelope.
    let mut untrusting = engine(
        &bob_client,
        &bob,
        bob_dev.identity(&bob),
        vault,
        copy_key(&bob_key),
    );
    let report = untrusting.sync(vault).await.unwrap();
    assert_eq!(report.rejected.len(), 1);
    assert_eq!(report.rejected[0].reason, VerifyError::UntrustedCertificate);

    let mut verifier = Verifier::new(bob.keys.signing.public());
    trust_members(&bob_client, &mut verifier, vault)
        .await
        .unwrap();
    let mut eb = SyncEngine::new(bob_client.clone(), MemoryStore::new(), bob_id, verifier);
    eb.add_vault_key(vault, 1, bob_key);
    eb.sync(vault).await.unwrap();
    assert_eq!(field(&eb, vault, "h1", "name"), json!("shared host"));

    // A viewer cannot push.
    edit(&mut eb, vault, "h1", &[("name", json!("bob was here"))]);
    let err = eb.sync(vault).await.unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::Forbidden));

    // A non-owner cannot change membership, not even to promote themselves.
    let bob_info = alice_client.lookup_account("bob").await.unwrap();
    let err = bob_client
        .put_member(
            vault,
            &bob_info.account_id,
            &PutMemberRequest {
                role: Role::Owner,
                sealed_key: None,
                key_generation: None,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::Forbidden));
    let carol_info = alice_client.lookup_account("carol").await.unwrap();
    let err = bob_client
        .put_member(
            vault,
            &carol_info.account_id,
            &PutMemberRequest {
                role: Role::Viewer,
                sealed_key: Some(vec![0; 105]),
                key_generation: Some(1),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::Forbidden));

    // The owner promotes bob to editor (role change only, no new key).
    alice_client
        .put_member(
            vault,
            &bob_info.account_id,
            &PutMemberRequest {
                role: Role::Editor,
                sealed_key: None,
                key_generation: None,
            },
        )
        .await
        .unwrap();
    let report = eb.sync(vault).await.unwrap();
    assert!(report.pushed >= 1, "{report:?}");

    // Alice trusts bob's account and sees his edit.
    trust_members(&alice_client, ea.verifier_mut(), vault)
        .await
        .unwrap();
    ea.sync(vault).await.unwrap();
    assert_eq!(field(&ea, vault, "h1", "name"), json!("bob was here"));

    // The last owner cannot be removed.
    let alice_info = alice_client.lookup_account("alice").await.unwrap();
    let members = alice_client.members(vault).await.unwrap();
    let only_bob: Vec<MemberInfo> = members
        .iter()
        .filter(|m| m.account_id == bob_info.account_id)
        .cloned()
        .collect();
    let err = ea
        .rotate_vault_key(
            vault,
            VaultKey::generate().unwrap(),
            Some(alice_info.account_id.clone()),
            seal_for_members(&only_bob, &VaultKey::generate().unwrap()).unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::Conflict));
    assert_eq!(alice_client.members(vault).await.unwrap().len(), 2);

    // Members cannot be removed without a rotation: there is no such call.
    // Removing bob rotates the key; he is locked out and has only the old
    // generation.
    let new_key = VaultKey::generate().unwrap();
    let remaining: Vec<MemberInfo> = members
        .iter()
        .filter(|m| m.account_id != bob_info.account_id)
        .cloned()
        .collect();
    let generation = ea
        .rotate_vault_key(
            vault,
            copy_key(&new_key),
            Some(bob_info.account_id.clone()),
            seal_for_members(&remaining, &new_key).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generation, 2);
    let err = bob_client.pull(vault, 0, 10).await.unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::NotFound));
    assert!(bob_client.list_vaults().await.unwrap().is_empty());
    assert_eq!(alice_client.members(vault).await.unwrap().len(), 1);

    // Alice's list shows the new generation and the new key; what the server
    // holds is under it.
    let summary = alice_client.list_vaults().await.unwrap().remove(0);
    assert_eq!(summary.key_generation, 2);
    let (current, generation) = open_vault(&alice_client, &alice, vault).await.unwrap();
    assert_eq!((current.expose(), generation), (new_key.expose(), 2));
    let page = alice_client.pull(vault, 0, 100).await.unwrap();
    assert!(page.records.iter().all(|r| r.envelope.key_generation == 2));

    // Anything still using generation 1 is refused: a device that has not
    // learned of the rotation yet, or bob's copy of the key.
    let mut stale = engine(
        &alice_client,
        &alice,
        Dev {
            seed: 1,
            name: "alice-laptop",
        }
        .identity(&alice),
        vault,
        copy_key(&key),
    );
    edit(&mut stale, vault, "h2", &[("name", json!("old key"))]);
    let err = stale.pull(vault).await.unwrap_err();
    assert!(
        matches!(err, SyncError::UnknownKeyGeneration { generation: 2, .. }),
        "{err:?}"
    );
    let err = stale.push(vault).await.unwrap_err();
    assert!(err.is_stale_key(), "{err:?}");
    assert_eq!(transport_code(err), Some(ErrorCode::StaleKeyGeneration));
    // ... and a new member cannot be given the old key.
    let err = share_vault(&alice_client, vault, &key, 1, "carol", Role::Viewer)
        .await
        .unwrap_err();
    assert!(
        matches!(&err, AuthError::Transport(t) if t.code() == Some(ErrorCode::StaleKeyGeneration)),
        "{err:?}"
    );
    share_vault(&alice_client, vault, &new_key, 2, "carol", Role::Viewer)
        .await
        .unwrap();
    assert_eq!(alice_client.members(vault).await.unwrap().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn revoked_device_is_rejected() {
    let server = start().await;
    let (client_a, alice) = signup(&server, "alice").await;
    let vault = "v-revoke";
    let key = create_vault(&client_a, &alice, vault).await.unwrap();
    let id_a = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client_a, &alice)
    .await;

    let (client_b, alice_b) = second_device(&server, "alice").await;
    let phone = Dev {
        seed: 2,
        name: "stolen phone",
    };
    let id_b = phone.enroll(&client_b, &alice_b).await;
    let phone_id = id_b.device_id.clone();
    let mut eb = engine(&client_b, &alice_b, id_b, vault, copy_key(&key));
    edit(&mut eb, vault, "r", &[("name", json!("before revoke"))]);
    eb.sync(vault).await.unwrap();

    // Alice revokes the phone from her laptop.
    assert!(client_a
        .list_devices()
        .await
        .unwrap()
        .iter()
        .any(|d| d.device_id == phone_id && !d.revoked));
    client_a.revoke_device(&phone_id).await.unwrap();
    assert!(client_a
        .list_devices()
        .await
        .unwrap()
        .iter()
        .any(|d| d.device_id == phone_id && d.revoked));

    // Its session ended with the revocation.
    edit(&mut eb, vault, "r", &[("name", json!("after revoke"))]);
    let err = eb.sync(vault).await.unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::Unauthorized));

    // Signing in again (the passphrase is still known to the thief) does not
    // bring the key back: the server refuses envelopes it signs.
    let relogin = server.client();
    let acct = login(&relogin, "alice", PASS).await.unwrap();
    let mut again = engine(
        &relogin,
        &acct,
        phone.identity(&acct),
        vault,
        copy_key(&key),
    );
    let local = eb.store().record(vault, "r").unwrap().clone();
    again.store_mut().save(local);
    let err = again.sync(vault).await.unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::DeviceRevoked));

    // The revoked key cannot be registered again.
    let identity = phone.identity(&acct);
    let err = relogin
        .register_device(&RegisterDeviceRequest {
            device_public: identity.cert.device_public.to_vec(),
            name: "again".into(),
            cert: identity.cert.to_bytes(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::DeviceRevoked));

    // The still-trusted laptop keeps working and sees the pre-revoke record.
    let mut ea = engine(&client_a, &alice, id_a, vault, key);
    ea.sync(vault).await.unwrap();
    assert_eq!(field(&ea, vault, "r", "name"), json!("before revoke"));
}

#[tokio::test(flavor = "multi_thread")]
async fn server_database_never_holds_plaintext() {
    let server = start().await;
    let (client, alice) = signup(&server, "alice").await;
    let vault = "v-plain";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let id = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client, &alice)
    .await;
    let mut e = engine(&client, &alice, id, vault, key);
    let label = "prod-db-label-7731";
    let address = "203.0.113.77";
    let secret = "hunter2-password-4411";
    edit(
        &mut e,
        vault,
        "h1",
        &[
            ("name", json!(label)),
            ("host", json!(address)),
            ("password", json!(secret)),
        ],
    );
    e.sync(vault).await.unwrap();
    edit(
        &mut e,
        vault,
        "h1",
        &[("name", json!("renamed-label-9921"))],
    );
    e.sync(vault).await.unwrap();

    // Sanity: the server really stored the record and its history.
    assert_eq!(e.history(vault, "h1").await.unwrap().len(), 1);

    let bytes = server.db_bytes();
    assert!(bytes.len() > 4096, "database files were read");
    for needle in [
        label,
        address,
        secret,
        "renamed-label-9921",
        "correct horse battery staple",
    ] {
        assert!(
            !contains(&bytes, needle.as_bytes()),
            "plaintext {needle:?} found in the server database"
        );
    }
    // The routing metadata that is meant to be plain is there.
    assert!(contains(&bytes, b"v-plain"));
}

#[tokio::test(flavor = "multi_thread")]
async fn oversized_requests_are_refused() {
    let server = start_with(|c| c.max_body_bytes = 64 * 1024).await;
    let (client, alice) = signup(&server, "alice").await;
    let vault = "v-big";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let id = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client, &alice)
    .await;
    let mut e = engine(&client, &alice, id, vault, key);
    edit(
        &mut e,
        vault,
        "big",
        &[("blob", json!("x".repeat(200 * 1024)))],
    );
    let err = e.sync(vault).await.unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::TooLarge));
}

/// POST `len` bytes of JSON-ish body to the server over a bare socket and
/// return the whole response. The write must complete: an oversized body is
/// read and discarded before the server answers.
async fn raw_post(server: &TestServer, len: usize) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = server.base.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST /v1/auth/login/start HTTP/1.1\r\nhost: {addr}\r\n\
         content-type: application/json\r\ncontent-length: {len}\r\n\
         connection: close\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await.unwrap();
    stream
        .write_all(&vec![b'x'; len])
        .await
        .expect("the server reads the oversized body instead of resetting");
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    response
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversized_body_gets_a_413_even_while_the_client_is_still_sending() {
    let server = start_with(|c| c.max_body_bytes = 64 * 1024).await;
    // Several times the limit, but within the part the server reads through.
    for _ in 0..5 {
        let response = raw_post(&server, 200 * 1024).await;
        assert!(response.starts_with("HTTP/1.1 413"), "{response}");
        assert!(response.contains("\"too_large\""), "{response}");
        assert!(response.contains("x-easyssh-max-body: 65536"), "{response}");
    }
    // A body at the limit is not refused for its size.
    let response = raw_post(&server, 64 * 1024).await;
    assert!(!response.starts_with("HTTP/1.1 413"), "{response}");
}

/// A signed envelope for `record` made outside any engine.
fn envelope_of(
    identity: &DeviceIdentity,
    key: &VaultKey,
    generation: u32,
    vault: &str,
    record: &str,
    n: u64,
) -> Envelope {
    let mut version = RecordVersion::new("hosts");
    version.edit(&identity.device_id, [("n", json!(n))]);
    seal_record(identity, key, generation, vault, record, &version).unwrap()
}

fn push_of(envelope: Envelope) -> PushRequest {
    PushRequest {
        envelope,
        expected: Default::default(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn batch_push_is_all_or_nothing_with_a_report_per_record() {
    let server = start().await;
    let (client, alice) = signup(&server, "alice").await;
    let (bob_client, _bob) = signup(&server, "bob").await;
    let vault = "v-batch";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let id = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client, &alice)
    .await;
    let make = |record: &str, n: u64| envelope_of(&id, &key, 1, vault, record, n);

    // A clean batch lands as a whole, with the receive time stamped.
    let ok = BatchPushRequest {
        items: vec![
            push_of(make("a", 1)),
            push_of(make("b", 2)),
            push_of(make("c", 3)),
        ],
    };
    let response = client.push_batch(vault, &ok).await.unwrap();
    assert!(response.committed);
    assert!(response
        .results
        .iter()
        .all(|r| matches!(r, BatchItemResult::Accepted { .. })));
    let page = client.pull(vault, 0, 10).await.unwrap();
    assert_eq!(page.records.len(), 3);
    assert!(page.records.iter().all(|r| r.envelope.received_at > 0));

    // One compare-and-set conflict (the record exists, the client thought it
    // did not) keeps the others out too, and says which one it was.
    let mixed = BatchPushRequest {
        items: vec![
            push_of(make("d", 4)),
            push_of(make("a", 5)),
            push_of(make("e", 6)),
        ],
    };
    let response = client.push_batch(vault, &mixed).await.unwrap();
    assert!(!response.committed);
    assert_eq!(response.results[0], BatchItemResult::NotApplied);
    assert!(matches!(
        &response.results[1],
        BatchItemResult::Conflict {
            reason: ConflictReason::Cas,
            current: Some(_)
        }
    ));
    assert_eq!(response.results[2], BatchItemResult::NotApplied);
    assert_eq!(client.pull(vault, 0, 10).await.unwrap().records.len(), 3);

    // A forged item, a stale key generation and a tampered table are refused
    // item by item.
    let mut forged = make("f", 7);
    forged.signature[0] ^= 1;
    let mut retabled = make("g", 8);
    retabled.table = "snippets".into();
    let old_key = envelope_of(&id, &key, 7, vault, "h", 9);
    let response = client
        .push_batch(
            vault,
            &BatchPushRequest {
                items: vec![
                    push_of(make("i", 10)),
                    push_of(forged),
                    push_of(retabled),
                    push_of(old_key),
                ],
            },
        )
        .await
        .unwrap();
    assert!(!response.committed);
    let codes: Vec<_> = response
        .results
        .iter()
        .map(|r| match r {
            BatchItemResult::Rejected { code, .. } => Some(*code),
            _ => None,
        })
        .collect();
    assert_eq!(
        codes,
        vec![
            None,
            Some(ErrorCode::InvalidSignature),
            Some(ErrorCode::InvalidSignature),
            Some(ErrorCode::StaleKeyGeneration)
        ]
    );
    assert_eq!(client.pull(vault, 0, 10).await.unwrap().records.len(), 3);

    // Shape: empty, duplicated ids, too many.
    for items in [
        Vec::new(),
        vec![push_of(make("x", 1)), push_of(make("x", 2))],
    ] {
        let err = client
            .push_batch(vault, &BatchPushRequest { items })
            .await
            .unwrap_err();
        assert_eq!(err.code(), Some(ErrorCode::BadRequest));
    }

    // A viewer cannot batch-push; a stranger does not see the vault.
    share_vault(&client, vault, &key, 1, "bob", Role::Viewer)
        .await
        .unwrap();
    let err = bob_client.push_batch(vault, &ok).await.unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::Forbidden));
    let (carol_client, _carol) = signup(&server, "carol").await;
    let err = carol_client.push_batch(vault, &ok).await.unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::NotFound));
}

#[tokio::test(flavor = "multi_thread")]
async fn first_upload_uses_batches_and_the_data_arrives_whole() {
    let server = start().await;
    let (client, alice) = signup(&server, "alice").await;
    let vault = "v-first";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let id = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client, &alice)
    .await;
    let mut e = engine(&client, &alice, id, vault, copy_key(&key));
    for i in 0..230 {
        edit(&mut e, vault, &format!("h{i:03}"), &[("name", json!(i))]);
    }
    let report = e.sync(vault).await.unwrap();
    assert_eq!((report.pushed, report.conflicts), (230, 0));
    assert_eq!(client.pull(vault, 0, 500).await.unwrap().records.len(), 230);
}

#[tokio::test(flavor = "multi_thread")]
async fn rotation_is_owner_only_all_or_nothing_and_covers_every_record() {
    // The rotation body carries the whole vault: it has its own, larger
    // limit than other requests.
    let server = start_with(|c| c.max_body_bytes = 300 * 1024).await;
    let (client, alice) = signup(&server, "alice").await;
    let (bob_client, _bob) = signup(&server, "bob").await;
    let vault = "v-rot";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let id = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client, &alice)
    .await;
    let mut e = engine(&client, &alice, id, vault, copy_key(&key));
    for i in 0..400 {
        edit(&mut e, vault, &format!("h{i:03}"), &[("name", json!(i))]);
    }
    e.sync(vault).await.unwrap();
    share_vault(&client, vault, &key, 1, "bob", Role::Editor)
        .await
        .unwrap();
    let members = client.members(vault).await.unwrap();
    let new_key = VaultKey::generate().unwrap();
    let sealed = seal_for_members(&members, &new_key).unwrap();

    // A request for another generation, or with the wrong member list, is
    // reported and changes nothing.
    let request = RotateRequest {
        from_generation: 5,
        remove_member: None,
        sealed_keys: sealed.clone(),
        records: Vec::new(),
    };
    let response = client.rotate(vault, &request).await.unwrap();
    assert!(!response.committed);
    assert_eq!(response.key_generation, 1);
    let response = client
        .rotate(
            vault,
            &RotateRequest {
                from_generation: 1,
                sealed_keys: sealed[..1].to_vec(),
                ..request.clone()
            },
        )
        .await
        .unwrap();
    assert!(!response.committed && response.members_changed);

    // Not an owner: refused. Not a member: not found.
    let err = bob_client
        .rotate(
            vault,
            &RotateRequest {
                from_generation: 1,
                ..request.clone()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::Forbidden));

    // Leaving records out is refused and lists them.
    let response = client
        .rotate(
            vault,
            &RotateRequest {
                from_generation: 1,
                ..request.clone()
            },
        )
        .await
        .unwrap();
    assert!(!response.committed);
    assert_eq!(response.missing.len(), 400);

    // The engine does it properly, through the larger body limit.
    e.pull(vault).await.unwrap();
    let remaining: Vec<MemberInfo> = members
        .iter()
        .filter(|m| m.account_id != bob_client_account(&members, "bob"))
        .cloned()
        .collect();
    assert_eq!(remaining.len(), 1);
    let generation = e
        .rotate_vault_key(
            vault,
            copy_key(&new_key),
            Some(bob_client_account(&members, "bob")),
            seal_for_members(&remaining, &new_key).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generation, 2);
    let page = client.pull(vault, 0, 500).await.unwrap();
    assert_eq!(page.records.len(), 400);
    assert!(page.records.iter().all(|r| r.envelope.key_generation == 2));

    // Bob keeps what he had (his copy of generation 1) and nothing else.
    assert_eq!(
        bob_client.pull(vault, 0, 10).await.unwrap_err().code(),
        Some(ErrorCode::NotFound)
    );
    let err = bob_client
        .rotate(
            vault,
            &RotateRequest {
                from_generation: 2,
                ..request
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::NotFound));

    // Records written after the rotation use the new key; a second device
    // signed in as alice picks the new key from the vault list.
    let (second, alice2) = second_device(&server, "alice").await;
    let id2 = Dev {
        seed: 2,
        name: "phone",
    }
    .enroll(&second, &alice2)
    .await;
    let (k2, g2) = open_vault(&second, &alice2, vault).await.unwrap();
    assert_eq!((k2.expose(), g2), (new_key.expose(), 2));
    let mut e2 = engine(&second, &alice2, id2, vault, copy_key(&key));
    e2.add_vault_key(vault, 2, k2);
    let report = e2.sync(vault).await.unwrap();
    assert_eq!(report.pulled, 400);
    // The old generation-1 key stays known to it for reading history, but
    // writes use generation 2.
    edit(&mut e2, vault, "h000", &[("name", json!("renamed"))]);
    e2.sync(vault).await.unwrap();
    let env = client
        .pull(vault, 0, 500)
        .await
        .unwrap()
        .records
        .into_iter()
        .find(|r| r.envelope.record_id == "h000")
        .unwrap()
        .envelope;
    assert_eq!(env.key_generation, 2);
}

fn bob_client_account(members: &[MemberInfo], name: &str) -> String {
    members
        .iter()
        .find(|m| m.username == name)
        .unwrap()
        .account_id
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rotation_that_races_with_a_push_changes_nothing_and_can_be_retried() {
    let server = start().await;
    let (client, alice) = signup(&server, "alice").await;
    let vault = "v-race";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let id = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client, &alice)
    .await;
    let (client2, alice2) = second_device(&server, "alice").await;
    let id2 = Dev {
        seed: 2,
        name: "phone",
    }
    .enroll(&client2, &alice2)
    .await;
    let mut a = engine(&client, &alice, id, vault, copy_key(&key));
    let mut b = engine(&client2, &alice2, id2, vault, copy_key(&key));
    edit(&mut a, vault, "one", &[("name", json!("1"))]);
    edit(&mut a, vault, "two", &[("name", json!("2"))]);
    a.sync(vault).await.unwrap();
    b.sync(vault).await.unwrap();
    edit(&mut b, vault, "two", &[("name", json!("2b"))]);
    b.sync(vault).await.unwrap();

    let members = client.members(vault).await.unwrap();
    let new_key = VaultKey::generate().unwrap();
    let err = a
        .rotate_vault_key(
            vault,
            copy_key(&new_key),
            None,
            seal_for_members(&members, &new_key).unwrap(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&err, SyncError::Rotation(RotationFailure::Conflict(ids)) if ids == &["two"]),
        "{err:?}"
    );
    assert_eq!(client.list_vaults().await.unwrap()[0].key_generation, 1);
    let page = client.pull(vault, 0, 10).await.unwrap();
    assert!(page.records.iter().all(|r| r.envelope.key_generation == 1));

    a.sync(vault).await.unwrap();
    let generation = a
        .rotate_vault_key(
            vault,
            copy_key(&new_key),
            None,
            seal_for_members(&members, &new_key).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(generation, 2);
    assert_eq!(field(&a, vault, "two", "name"), json!("2b"));
}

#[tokio::test(flavor = "multi_thread")]
async fn certificates_expire_and_are_renewed_without_a_new_device() {
    let server = start().await;
    let (client, alice) = signup(&server, "alice").await;
    let vault = "v-expiry";
    let key = create_vault(&client, &alice, vault).await.unwrap();
    let dev = Dev {
        seed: 1,
        name: "laptop",
    };
    let now = unix_now();

    // Already expired: not even registered.
    let stale = certify_device(
        &alice.keys.signing,
        &dev.key().public(),
        "laptop",
        1,
        now - 10,
    );
    let err = client
        .register_device(&RegisterDeviceRequest {
            device_public: dev.key().public().to_vec(),
            name: "laptop".into(),
            cert: stale.to_bytes(),
        })
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::CertificateExpired));

    // A certificate that runs out within seconds works, then stops working.
    let short = certify_device(
        &alice.keys.signing,
        &dev.key().public(),
        "laptop",
        now,
        now + 2,
    );
    client
        .register_device(&RegisterDeviceRequest {
            device_public: dev.key().public().to_vec(),
            name: "laptop".into(),
            cert: short.to_bytes(),
        })
        .await
        .unwrap();
    let identity = DeviceIdentity {
        device_id: device_id_for(&dev.key().public()),
        signing_key: dev.key(),
        cert: short,
    };
    let mut e = engine(&client, &alice, identity, vault, copy_key(&key));
    edit(&mut e, vault, "r", &[("name", json!("valid"))]);
    e.sync(vault).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(3200)).await;
    edit(&mut e, vault, "r", &[("name", json!("too late"))]);
    let err = e.sync(vault).await.unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::CertificateExpired));

    // What it wrote while the certificate was valid is still accepted by
    // other clients, judged at the time the server received it.
    let mut reader = engine(
        &client,
        &alice,
        Dev {
            seed: 2,
            name: "reader",
        }
        .identity(&alice),
        vault,
        copy_key(&key),
    );
    let report = reader.sync(vault).await.unwrap();
    assert!(report.rejected.is_empty(), "{report:?}");
    assert_eq!(field(&reader, vault, "r", "name"), json!("valid"));

    // Signing in renews it: the same device key, a new certificate.
    let renewed = renew_device_cert(&client, &alice, "laptop", &dev.key().public(), unix_now())
        .await
        .unwrap();
    assert!(renewed.expires_at >= unix_now() + CERT_LIFETIME_SECS - 5);
    let listed = client.list_devices().await.unwrap();
    let info = listed
        .iter()
        .find(|d| d.device_id == device_id_for(&dev.key().public()))
        .unwrap();
    assert_eq!(info.cert_expires_at, renewed.expires_at);
    assert_eq!(listed.len(), 1, "renewal does not add a device");
    let identity = DeviceIdentity {
        device_id: device_id_for(&dev.key().public()),
        signing_key: dev.key(),
        cert: renewed,
    };
    let mut again = engine(&client, &alice, identity, vault, copy_key(&key));
    let local = e.store().record(vault, "r").unwrap().clone();
    again.store_mut().save(local);
    again.sync(vault).await.unwrap();
    assert_eq!(
        client
            .pull(vault, 0, 10)
            .await
            .unwrap()
            .records
            .iter()
            .filter(|r| r.envelope.record_id == "r")
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn revocation_is_recorded_and_earlier_records_stay_valid_for_everyone() {
    let server = start().await;
    let (client_a, alice) = signup(&server, "alice").await;
    let (client_b, bob) = signup(&server, "bob").await;
    let vault = "v-rev-time";
    let key = create_vault(&client_a, &alice, vault).await.unwrap();
    let id_a = Dev {
        seed: 1,
        name: "laptop",
    }
    .enroll(&client_a, &alice)
    .await;
    share_vault(&client_a, vault, &key, 1, "bob", Role::Editor)
        .await
        .unwrap();
    let (client_p, alice_p) = second_device(&server, "alice").await;
    let phone = Dev {
        seed: 2,
        name: "phone",
    };
    let id_p = phone.enroll(&client_p, &alice_p).await;
    let phone_id = id_p.device_id.clone();
    let mut ep = engine(&client_p, &alice_p, id_p, vault, copy_key(&key));
    edit(&mut ep, vault, "from-phone", &[("name", json!("before"))]);
    ep.sync(vault).await.unwrap();

    let before = unix_now();
    client_a.revoke_device(&phone_id).await.unwrap();
    let after = unix_now();

    // The recorded time is visible to the account and to vault members.
    let listed = client_a.list_devices().await.unwrap();
    let phone_info = listed.iter().find(|d| d.device_id == phone_id).unwrap();
    let at = phone_info.revoked_at.expect("revocation time");
    assert!((before..=after).contains(&at));
    let revoked = client_b.revoked_devices(vault).await.unwrap();
    assert_eq!(
        revoked,
        vec![RevokedDevice {
            device_id: phone_id.clone(),
            revoked_at: at
        }]
    );
    let err = server.client().revoked_devices(vault).await.unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::Unauthorized));

    // Bob's client takes the revocation list; what the phone wrote before
    // revocation (received at or before `at`) is still valid, pulled with its
    // receive time.
    let bob_dev = Dev {
        seed: 3,
        name: "bob-laptop",
    };
    let id_b = bob_dev.enroll(&client_b, &bob).await;
    let mut verifier = Verifier::new(bob.keys.signing.public());
    trust_members(&client_b, &mut verifier, vault)
        .await
        .unwrap();
    trust_revocations(&client_b, &mut verifier, vault)
        .await
        .unwrap();
    assert!(verifier.is_revoked(&easyssh_sync::wire::device_public_from_id(&phone_id).unwrap()));
    let mut eb = SyncEngine::new(client_b.clone(), MemoryStore::new(), id_b, verifier);
    eb.add_vault_key(vault, 1, copy_key(&key));
    let report = eb.sync(vault).await.unwrap();
    assert!(report.rejected.is_empty(), "{report:?}");
    assert_eq!(field(&eb, vault, "from-phone", "name"), json!("before"));
    let received = client_b.pull(vault, 0, 10).await.unwrap().records[0]
        .envelope
        .received_at;
    assert!(received <= at, "{received} <= {at}");

    // After the revocation the server refuses the phone's keys, even with a
    // fresh session; nothing newer from it can exist.
    let relogin = server.client();
    let acct = login(&relogin, "alice", PASS).await.unwrap();
    let mut again = engine(
        &relogin,
        &acct,
        phone.identity(&acct),
        vault,
        copy_key(&key),
    );
    edit(&mut again, vault, "later", &[("name", json!("after"))]);
    let err = again.sync(vault).await.unwrap_err();
    assert_eq!(transport_code(err), Some(ErrorCode::DeviceRevoked));
    let _ = id_a;
}
