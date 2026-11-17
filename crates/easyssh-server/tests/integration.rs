//! End-to-end tests: a real server on a loopback port, real HTTP clients.

use easyssh_crypto::{certify_device, AccountKeyPair, DeviceSigningKey, KdfParams, VaultKey};
use easyssh_server::{serve, Config};
use easyssh_sync::account::{
    create_vault, enroll_device, login, open_vault, register, share_vault, trust_members, Account,
};
use easyssh_sync::wire::*;
use easyssh_sync::{
    AuthError, DeviceIdentity, HttpClient, MemoryStore, SyncEngine, SyncError, Transport, Verifier,
    VerifyError,
};
use serde_json::{json, Value};
use tempfile::TempDir;
use tokio::net::TcpListener;

const PASS: &[u8] = b"correct horse battery staple";
const ISSUED: i64 = 1_700_000_000;

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
        let cert = certify_device(&account.keys.signing, &key.public(), self.name, ISSUED);
        DeviceIdentity {
            device_id: device_id_for(&key.public()),
            signing_key: key,
            cert,
        }
    }

    async fn enroll(&self, client: &HttpClient, account: &Account) -> DeviceIdentity {
        enroll_device(client, account, self.name, self.key(), ISSUED)
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
    e.add_vault_key(vault, key);
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
    let bad_cert = certify_device(&mallory.signing, &dev.public(), "bad", 1);
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
    let good = certify_device(&alice.keys.signing, &dev.public(), "ok", 1);
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
    let key_b = open_vault(&client_b, &alice_b, vault).await.unwrap();
    assert_eq!(key_b, key);

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
        easyssh_sync::seal_record(&dev_a.identity(&alice), &key, vault, "h1", &rec.version)
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
    share_vault(&alice_client, vault, &key, "bob", Role::Viewer)
        .await
        .unwrap();
    let bob_key = open_vault(&bob_client, &bob, vault).await.unwrap();
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
    eb.add_vault_key(vault, bob_key);
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
    let err = alice_client
        .remove_member(vault, &alice_info.account_id)
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::Conflict));

    // Removing bob locks him out.
    alice_client
        .remove_member(vault, &bob_info.account_id)
        .await
        .unwrap();
    let err = bob_client.pull(vault, 0, 10).await.unwrap_err();
    assert_eq!(err.code(), Some(ErrorCode::NotFound));
    assert_eq!(alice_client.members(vault).await.unwrap().len(), 1);
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
