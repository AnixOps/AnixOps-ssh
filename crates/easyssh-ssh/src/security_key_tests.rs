//! `sk-*` keys against a software authenticator: the signatures verify with
//! `ssh-key`, and a whole login runs against an in-process russh server that
//! verifies them too.

use crate::interaction::{Decision, Interaction, KbdPrompt};
use crate::known_hosts::KnownHostStatus;
use crate::security_key::{
    encode_signature, sign_with_security_key, SkAlgorithm, SkAssertion, SkError, SkKeyInfo,
    SkSignRequest, FLAG_USER_PRESENCE, FLAG_USER_VERIFICATION,
};
use crate::security_key_software::SoftwareSecurityKey;
use crate::{decode_identity, AuthMethod, ConnectParams, Error, MemoryIdentity, Session};
use async_trait::async_trait;
use russh::keys::signature::Verifier;
use russh::keys::ssh_key::Signature;
use russh::server::{Auth, Handler, PublicKeyOffer, Server, VerifiedPublicKey};
use russh::{MethodKind, MethodSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;

/// Answers the questions of one login and records them in order.
struct Script {
    pins: Mutex<Vec<String>>,
    events: Mutex<Vec<String>>,
}

impl Script {
    fn new(pins: &[&str]) -> Self {
        Self {
            pins: Mutex::new(pins.iter().rev().map(|p| (*p).to_owned()).collect()),
            events: Mutex::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    fn push(&self, event: impl Into<String>) {
        self.events.lock().unwrap().push(event.into());
    }
}

#[async_trait]
impl Interaction for Script {
    async fn verify_host_key(&self, _: &str, _: u16, _: &str, _: KnownHostStatus) -> Decision {
        self.push("hostkey");
        Decision::AcceptOnce
    }
    async fn password(&self, _: &str) -> Option<String> {
        self.push("password");
        None
    }
    async fn keyboard_interactive(&self, _: &str, _: &str, _: &[KbdPrompt]) -> Option<Vec<String>> {
        None
    }
    async fn passphrase(&self, _: &Path) -> Option<String> {
        self.push("passphrase");
        None
    }
    async fn security_key_touch(&self, key: &str) {
        self.push(format!("touch {key}"));
    }
    async fn security_key_touch_done(&self, _: &str) {
        self.push("touch done");
    }
    async fn security_key_pin(&self, _: &str, retries: Option<u32>) -> Option<String> {
        self.push(format!("pin retries={retries:?}"));
        self.pins.lock().unwrap().pop()
    }
}

/// Verifies like a server does: `SHA256(app) || flags || counter ||
/// SHA256(message)` against the public key.
fn verifies(key: &russh::keys::PrivateKey, message: &[u8], signature: &Signature) -> bool {
    Verifier::verify(key.public_key().key_data(), message, signature).is_ok()
}

fn info_of(text: &str) -> SkKeyInfo {
    let key = russh::keys::decode_secret_key(text, None).unwrap();
    SkKeyInfo::of(&key).expect("an sk key")
}

#[test]
fn key_files_round_trip_through_the_loader() {
    let device = SoftwareSecurityKey::new();
    for algorithm in [SkAlgorithm::Ed25519, SkAlgorithm::EcdsaP256] {
        let file = device.create(algorithm, "ssh:", FLAG_USER_PRESENCE);
        assert!(file.private_text.contains("BEGIN OPENSSH PRIVATE KEY"));
        assert_eq!(
            crate::security_key_type(&file.private_text),
            Some(algorithm.name())
        );
        let info = info_of(&file.private_text);
        assert_eq!(info.algorithm, algorithm);
        assert_eq!(info.application, "ssh:");
        assert_eq!(info.flags, FLAG_USER_PRESENCE);
        assert_eq!(info.key_handle.len(), 32);
        assert!(file.public_line.starts_with(algorithm.name()));
    }
}

#[tokio::test]
async fn the_loader_returns_sk_keys_instead_of_refusing_them() {
    let device = SoftwareSecurityKey::new();
    let file = device.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    let key = decode_identity(
        &file.private_text,
        Path::new("id_ed25519_sk"),
        None,
        &crate::DenyAll,
    )
    .await
    .unwrap();
    assert!(SkKeyInfo::of(&key).is_some());
}

#[tokio::test]
async fn signatures_verify_with_ssh_key() {
    let device = SoftwareSecurityKey::new();
    for algorithm in [SkAlgorithm::Ed25519, SkAlgorithm::EcdsaP256] {
        let file = device.create(algorithm, "ssh:", FLAG_USER_PRESENCE);
        let info = SkKeyInfo::of(&file.private).unwrap();
        let script = Script::new(&[]);
        let message = b"the userauth challenge".to_vec();
        let blob = sign_with_security_key(&device, &script, "key", &info, &message)
            .await
            .unwrap();
        let signature = Signature::try_from(blob.as_slice()).unwrap();
        assert!(
            verifies(&file.private, &message, &signature),
            "{algorithm:?} signature did not verify"
        );
        // Another message does not verify.
        assert!(!verifies(&file.private, b"something else", &signature));
        // The user was asked to touch, then told it was over.
        assert_eq!(script.events(), ["touch key", "touch done"]);
    }
}

#[tokio::test]
async fn the_counter_goes_up_and_the_flags_are_signed() {
    let device = SoftwareSecurityKey::new();
    let file = device.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    let info = SkKeyInfo::of(&file.private).unwrap();
    let request = SkSignRequest {
        info: &info,
        message: b"m",
    };
    use crate::SecurityKey;
    let first = device.sign(&request, None).await.unwrap();
    let second = device.sign(&request, None).await.unwrap();
    assert_eq!((first.counter, second.counter), (1, 2));
    assert_eq!(first.flags, FLAG_USER_PRESENCE);
    // A flipped flag byte breaks the signature, so flags really are signed.
    let forged = SkAssertion {
        flags: first.flags | FLAG_USER_VERIFICATION,
        ..first
    };
    let blob = encode_signature(&info, &forged).unwrap();
    let signature = Signature::try_from(blob.as_slice()).unwrap();
    assert!(!verifies(&file.private, b"m", &signature));
}

#[tokio::test]
async fn a_key_without_touch_does_not_ask_for_one() {
    let device = SoftwareSecurityKey::new();
    let file = device.create(SkAlgorithm::Ed25519, "ssh:", 0);
    let info = SkKeyInfo::of(&file.private).unwrap();
    let script = Script::new(&[]);
    sign_with_security_key(&device, &script, "key", &info, b"m")
        .await
        .unwrap();
    assert!(script.events().is_empty(), "{:?}", script.events());
}

#[tokio::test]
async fn verify_required_keys_ask_for_the_pin_first_and_sign_with_verification() {
    let device = SoftwareSecurityKey::new().with_pin("2468");
    let file = device.create(
        SkAlgorithm::EcdsaP256,
        "ssh:",
        FLAG_USER_PRESENCE | FLAG_USER_VERIFICATION,
    );
    let info = SkKeyInfo::of(&file.private).unwrap();
    // A wrong PIN first: the device says how many tries are left.
    let script = Script::new(&["0000", "2468"]);
    let blob = sign_with_security_key(&device, &script, "key", &info, b"m")
        .await
        .unwrap();
    assert_eq!(
        script.events(),
        [
            "pin retries=None",
            "touch key",
            "touch done",
            "pin retries=Some(7)",
            "touch key",
            "touch done",
        ]
    );
    let signature = Signature::try_from(blob.as_slice()).unwrap();
    assert!(verifies(&file.private, b"m", &signature));
}

#[tokio::test]
async fn a_device_that_wants_a_pin_the_key_did_not_announce_asks_for_it() {
    // Key flags say "no verification needed", but a protected credential
    // makes the device ask anyway: simulated by a device that refuses the
    // first attempt.
    struct Picky {
        inner: SoftwareSecurityKey,
    }
    #[async_trait]
    impl crate::SecurityKey for Picky {
        async fn sign(
            &self,
            request: &SkSignRequest<'_>,
            pin: Option<&str>,
        ) -> Result<SkAssertion, SkError> {
            if pin.is_none() {
                return Err(SkError::PinRequired);
            }
            self.inner.sign(request, pin).await
        }
    }
    let device = Picky {
        inner: SoftwareSecurityKey::new().with_pin("1357"),
    };
    let file = device
        .inner
        .create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    let info = SkKeyInfo::of(&file.private).unwrap();
    let script = Script::new(&["1357"]);
    sign_with_security_key(&device, &script, "key", &info, b"m")
        .await
        .unwrap();
    assert_eq!(
        script.events(),
        [
            "touch key",
            "touch done",
            "pin retries=None",
            "touch key",
            "touch done"
        ]
    );
}

#[tokio::test]
async fn a_cancelled_pin_cancels_the_key() {
    let device = SoftwareSecurityKey::new().with_pin("2468");
    let file = device.create(
        SkAlgorithm::Ed25519,
        "ssh:",
        FLAG_USER_PRESENCE | FLAG_USER_VERIFICATION,
    );
    let info = SkKeyInfo::of(&file.private).unwrap();
    let script = Script::new(&[]);
    let error = sign_with_security_key(&device, &script, "key", &info, b"m")
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Cancelled(_)), "{error:?}");
    assert!(!error.to_string().contains("2468"));
}

#[tokio::test]
async fn too_many_wrong_pins_give_up() {
    let device = SoftwareSecurityKey::new().with_pin("2468");
    let file = device.create(
        SkAlgorithm::Ed25519,
        "ssh:",
        FLAG_USER_PRESENCE | FLAG_USER_VERIFICATION,
    );
    let info = SkKeyInfo::of(&file.private).unwrap();
    let script = Script::new(&["1", "2", "3", "4", "5"]);
    let error = sign_with_security_key(&device, &script, "key", &info, b"m")
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("wrong security key PIN"),
        "{error}"
    );
    // Never more than the first ask plus three retries.
    let asked = script
        .events()
        .iter()
        .filter(|e| e.starts_with("pin"))
        .count();
    assert_eq!(asked, 4);
}

#[tokio::test]
async fn a_key_the_device_does_not_hold_says_so() {
    let device = SoftwareSecurityKey::new();
    let other = SoftwareSecurityKey::new();
    let file = other.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    let info = SkKeyInfo::of(&file.private).unwrap();
    let error = sign_with_security_key(&device, &Script::new(&[]), "key", &info, b"m")
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("does not hold this key"),
        "{error}"
    );
}

#[test]
fn malformed_assertions_are_refused() {
    let info = SkKeyInfo {
        algorithm: SkAlgorithm::Ed25519,
        application: "ssh:".into(),
        key_handle: vec![1],
        flags: 1,
    };
    let short = SkAssertion {
        signature: vec![0; 10],
        flags: 1,
        counter: 1,
        webauthn: None,
    };
    assert!(encode_signature(&info, &short).is_err());
    let ecdsa = SkKeyInfo {
        algorithm: SkAlgorithm::EcdsaP256,
        ..info
    };
    assert!(encode_signature(&ecdsa, &short).is_err());
}

// ---- A whole login against an in-process russh server ----

/// Accepts the key `expected`, after russh verified its signature.
#[derive(Clone)]
struct Gate {
    expected: Option<russh::keys::PublicKey>,
    verified: Arc<Mutex<Vec<String>>>,
}

impl Handler for Gate {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        offer: PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        if self.expected.as_ref().map(|k| k.key_data()) == Some(offer.public_key.key_data()) {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        self.verified
            .lock()
            .unwrap()
            .push(key.algorithm.as_str().to_owned());
        Ok(Auth::Accept)
    }
}

impl Server for Gate {
    type Handler = Gate;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Gate {
        self.clone()
    }
}

/// Serves one connection on a loopback port; returns the port.
async fn serve_once(gate: Gate) -> u16 {
    let host_key =
        russh::keys::decode_secret_key(include_str!("../tests/fixtures/plain_ed25519"), None)
            .unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        auth_rejection_time: std::time::Duration::from_millis(1),
        auth_rejection_time_initial: Some(std::time::Duration::from_millis(1)),
        ..Default::default()
    });
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        if let Ok(session) = russh::server::run_stream(config, stream, gate).await {
            let _ = session.await;
        }
    });
    port
}

fn client(port: u16) -> ConnectParams {
    let mut params = ConnectParams::new("127.0.0.1", port, "someone");
    params.agent = crate::AgentSource::Disabled;
    params.keyboard_interactive = false;
    params.password = false;
    params.connect_timeout = std::time::Duration::from_secs(10);
    params
}

async fn login(
    device: Arc<SoftwareSecurityKey>,
    file_text: &str,
    expected: Option<russh::keys::PublicKey>,
    script: &Script,
) -> (Result<Session, Error>, Vec<String>) {
    let verified = Arc::new(Mutex::new(Vec::new()));
    let port = serve_once(Gate {
        expected,
        verified: verified.clone(),
    })
    .await;
    let params = client(port)
        .security_key(device)
        .memory_identity(MemoryIdentity::new("test key", file_text, None));
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        Session::connect(params, script),
    )
    .await
    .expect("the login finished");
    let verified = verified.lock().unwrap().clone();
    (result, verified)
}

#[tokio::test]
async fn ed25519_sk_key_logs_in_and_the_server_verifies_the_signature() {
    let device = Arc::new(SoftwareSecurityKey::new());
    let file = device.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    let script = Script::new(&[]);
    let (session, verified) = login(
        device.clone(),
        &file.private_text,
        Some(file.private.public_key().clone()),
        &script,
    )
    .await;
    let session = session.expect("login");
    assert_eq!(
        session.auth_method(),
        &AuthMethod::StoredKey("test key".into())
    );
    assert_eq!(verified, ["sk-ssh-ed25519@openssh.com"]);
    assert_eq!(script.events(), ["hostkey", "touch test key", "touch done"]);
    assert_eq!(device.signatures(), 1);
}

#[tokio::test]
async fn ecdsa_sk_key_logs_in_and_the_server_verifies_the_signature() {
    let device = Arc::new(SoftwareSecurityKey::new());
    let file = device.create(SkAlgorithm::EcdsaP256, "ssh:", FLAG_USER_PRESENCE);
    let script = Script::new(&[]);
    let (session, verified) = login(
        device,
        &file.private_text,
        Some(file.private.public_key().clone()),
        &script,
    )
    .await;
    session.expect("login");
    assert_eq!(verified, ["sk-ecdsa-sha2-nistp256@openssh.com"]);
}

#[tokio::test]
async fn a_pin_protected_key_logs_in_after_the_pin() {
    let device = Arc::new(SoftwareSecurityKey::new().with_pin("2468"));
    let file = device.create(
        SkAlgorithm::Ed25519,
        "ssh:",
        FLAG_USER_PRESENCE | FLAG_USER_VERIFICATION,
    );
    let script = Script::new(&["2468"]);
    let (session, verified) = login(
        device,
        &file.private_text,
        Some(file.private.public_key().clone()),
        &script,
    )
    .await;
    session.expect("login");
    assert_eq!(verified.len(), 1);
    assert_eq!(script.events()[1], "pin retries=None");
}

#[tokio::test]
async fn the_user_is_not_asked_to_touch_for_a_key_the_server_refuses() {
    let device = Arc::new(SoftwareSecurityKey::new());
    let file = device.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    let script = Script::new(&[]);
    let (session, verified) = login(device.clone(), &file.private_text, None, &script).await;
    let error = session.expect_err("the server refuses the key");
    assert!(matches!(error, Error::AuthFailed { .. }), "{error:?}");
    assert!(verified.is_empty());
    assert_eq!(script.events(), ["hostkey"]);
    assert_eq!(device.signatures(), 0);
}

#[tokio::test]
async fn a_missing_device_is_a_note_not_a_crash() {
    let signing_device = SoftwareSecurityKey::new();
    let file = signing_device.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    // A different device is plugged in: it does not know the handle.
    let plugged = Arc::new(SoftwareSecurityKey::new());
    let script = Script::new(&[]);
    let (session, _) = login(
        plugged,
        &file.private_text,
        Some(file.private.public_key().clone()),
        &script,
    )
    .await;
    match session.expect_err("no signature") {
        Error::AuthFailed { notes, .. } => assert!(
            notes.iter().any(|n| n.contains("does not hold this key")),
            "{notes:?}"
        ),
        other => panic!("expected AuthFailed, got {other:?}"),
    }
}

// ---- The webauthn signature form (Windows) ----

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn take_string(data: &[u8]) -> (&[u8], &[u8]) {
    let len = u32::from_be_bytes(data[..4].try_into().unwrap()) as usize;
    (&data[4..4 + len], &data[4 + len..])
}

/// A device that signs like Windows WebAuthn names another algorithm in the
/// userauth request, signs over the renamed request, and its signature has
/// the layout sshd parses; r, s, flags and counter verify over the client
/// data, and the challenge in it is the renamed request.
#[tokio::test]
async fn the_signer_makes_webauthn_signatures_for_ecdsa_keys() {
    use crate::security_key::SkSigner;
    use russh::keys::agent::AgentIdentity;
    use russh::Signer;

    let device = Arc::new(SoftwareSecurityKey::new().with_webauthn());
    let file = device.create(SkAlgorithm::EcdsaP256, "ssh:", FLAG_USER_PRESENCE);
    let info = SkKeyInfo::of(&file.private).unwrap();
    let public = file.private.public_key().clone();
    let blob = public.to_bytes().unwrap();

    let userauth = |algorithm: &str| {
        let mut out = Vec::new();
        put_string(&mut out, &[7u8; 32]);
        out.push(50);
        put_string(&mut out, b"someone");
        put_string(&mut out, b"ssh-connection");
        put_string(&mut out, b"publickey");
        out.push(1);
        put_string(&mut out, algorithm.as_bytes());
        put_string(&mut out, &blob);
        out
    };
    let webauthn_name = "webauthn-sk-ecdsa-sha2-nistp256@openssh.com";
    let request = userauth(SkAlgorithm::EcdsaP256.name());
    let renamed = userauth(webauthn_name);

    let script = Script::new(&[]);
    let mut signer = SkSigner {
        backend: device.clone(),
        interaction: &script,
        label: "key",
        info,
    };
    let identity = AgentIdentity::PublicKey {
        key: public.clone(),
        comment: String::new(),
    };
    let out = signer.auth_sign(&identity, None, request).await.unwrap();

    // The signed request carries the webauthn algorithm name.
    assert_eq!(&out[..renamed.len()], renamed.as_slice());

    // string name, string r||s, byte flags, uint32 counter, string origin,
    // string clientData, string extensions, behind a length.
    let (signature, rest) = take_string(&out[renamed.len()..]);
    assert!(rest.is_empty());
    let (name, body) = take_string(signature);
    assert_eq!(name, webauthn_name.as_bytes());
    let (rs, body) = take_string(body);
    let flags = body[0];
    let counter: [u8; 4] = body[1..5].try_into().unwrap();
    let (origin, body) = take_string(&body[5..]);
    let (client_data, body) = take_string(body);
    let (extensions, body) = take_string(body);
    assert_eq!(origin, b"ssh:");
    assert!(extensions.is_empty() && body.is_empty());
    assert_eq!(flags, FLAG_USER_PRESENCE);

    // The challenge is the base64url of what the server reconstructs.
    use base64::Engine;
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&renamed);
    let preamble = format!(r#"{{"type":"webauthn.get","challenge":"{challenge}","origin":"ssh:""#);
    assert!(client_data.starts_with(preamble.as_bytes()));

    // As a plain sk-ecdsa signature over the client data it verifies.
    let mut plain = Vec::new();
    put_string(&mut plain, SkAlgorithm::EcdsaP256.name().as_bytes());
    put_string(&mut plain, rs);
    plain.push(flags);
    plain.extend_from_slice(&counter);
    let signature = Signature::try_from(plain.as_slice()).unwrap();
    Verifier::verify(public.key_data(), client_data, &signature).unwrap();
}

#[tokio::test]
async fn webauthn_devices_refuse_ed25519_keys() {
    use crate::SecurityKey;
    let device = SoftwareSecurityKey::new().with_webauthn();
    let file = device.create(SkAlgorithm::Ed25519, "ssh:", FLAG_USER_PRESENCE);
    let info = SkKeyInfo::of(&file.private).unwrap();
    let request = SkSignRequest {
        info: &info,
        message: b"m",
    };
    assert!(matches!(
        device.sign(&request, None).await,
        Err(SkError::Device(_))
    ));
    assert_eq!(
        device.signature_kind(SkAlgorithm::Ed25519),
        crate::SkSignatureKind::Plain
    );
}
