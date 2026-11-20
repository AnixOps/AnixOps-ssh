//! Whole logins against an in-process russh server: when passphrases are
//! asked for, the saved password answering a keyboard-interactive password
//! round, and methods running again after a partial success.

use super::*;
use crate::interaction::{Decision, Interaction, KbdPrompt};
use crate::known_hosts::KnownHostStatus;
use crate::Session;
use async_trait::async_trait;
use russh::server::{Auth, Handler, PublicKeyOffer, Response, Server, VerifiedPublicKey};
use russh::MethodSet;
use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use tokio::net::TcpListener;

const PLAIN: &str = include_str!("../tests/fixtures/plain_ed25519");
// `ssh-keygen -N 'pass phrase'`, test only.
const ENCRYPTED: &str = include_str!("../tests/fixtures/encrypted_ed25519");
const ENCRYPTED_RSA: &str = include_str!("../tests/fixtures/encrypted_rsa");
const ENCRYPTED_RSA_PEM: &str = include_str!("../tests/fixtures/encrypted_rsa_pem");
const ENCRYPTED_RSA_PEM_PUB: &str = include_str!("../tests/fixtures/encrypted_rsa_pem.pub.txt");

fn public_of(text: &str) -> PublicKey {
    public_part(text, None).key.expect("a public key")
}

/// Answers everything and counts what was asked.
#[derive(Default)]
struct Asked {
    passphrases: AtomicUsize,
    kbd_rounds: AtomicUsize,
    passwords: AtomicUsize,
    saved: Mutex<Option<String>>,
    kbd_answer: Option<String>,
}

#[async_trait]
impl Interaction for Asked {
    async fn verify_host_key(&self, _: &str, _: u16, _: &str, _: KnownHostStatus) -> Decision {
        Decision::AcceptOnce
    }
    async fn password(&self, _: &str) -> Option<String> {
        self.passwords.fetch_add(1, Ordering::SeqCst);
        None
    }
    async fn saved_password(&self, prompt: &str) -> Option<String> {
        assert_eq!(prompt, "someone@127.0.0.1's password");
        self.saved.lock().unwrap().take()
    }
    async fn keyboard_interactive(
        &self,
        _: &str,
        _: &str,
        prompts: &[KbdPrompt],
    ) -> Option<Vec<String>> {
        self.kbd_rounds.fetch_add(1, Ordering::SeqCst);
        let answer = self.kbd_answer.clone()?;
        Some(prompts.iter().map(|_| answer.clone()).collect())
    }
    async fn passphrase(&self, _: &Path) -> Option<String> {
        self.passphrases.fetch_add(1, Ordering::SeqCst);
        Some("pass phrase".into())
    }
}

/// A server: accepts `key` for publickey and `password` for a
/// keyboard-interactive "Password:" round. With `kbd_first`, the
/// keyboard-interactive step is only a partial success after which the key
/// is required (`AuthenticationMethods keyboard-interactive,publickey`).
#[derive(Clone, Default)]
struct Door {
    key: Option<PublicKey>,
    password: Option<String>,
    kbd_first: bool,
    kbd_done: Arc<Mutex<bool>>,
    offers: Arc<AtomicUsize>,
}

impl Handler for Door {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        offer: PublicKeyOffer<'_>,
    ) -> std::result::Result<Auth, Self::Error> {
        self.offers.fetch_add(1, Ordering::SeqCst);
        let allowed = !self.kbd_first || *self.kbd_done.lock().unwrap();
        if allowed && self.key.as_ref().map(|k| k.key_data()) == Some(offer.public_key.key_data()) {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: VerifiedPublicKey<'_>,
    ) -> std::result::Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        response: Option<Response<'a>>,
    ) -> std::result::Result<Auth, Self::Error> {
        let Some(mut response) = response else {
            return Ok(Auth::Partial {
                name: Cow::Borrowed(""),
                instructions: Cow::Borrowed(""),
                prompts: Cow::Owned(vec![(Cow::Borrowed("Password: "), false)]),
            });
        };
        let answer = response.next().map(|b| b.to_vec()).unwrap_or_default();
        if Some(answer.as_slice()) != self.password.as_deref().map(str::as_bytes) {
            return Ok(Auth::reject());
        }
        if self.kbd_first {
            *self.kbd_done.lock().unwrap() = true;
            return Ok(Auth::Reject {
                proceed_with_methods: Some(MethodSet::from(&[MethodKind::PublicKey][..])),
                partial_success: true,
            });
        }
        Ok(Auth::Accept)
    }
}

impl Server for Door {
    type Handler = Door;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Door {
        self.clone()
    }
}

async fn serve_once(door: Door, methods: &[MethodKind]) -> u16 {
    let host_key = russh::keys::decode_secret_key(PLAIN, None).unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        methods: MethodSet::from(methods),
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
        if let Ok(session) = russh::server::run_stream(config, stream, door).await {
            let _ = session.await;
        }
    });
    port
}

fn client(port: u16) -> ConnectParams {
    let mut params = ConnectParams::new("127.0.0.1", port, "someone");
    params.agent = AgentSource::Disabled;
    params.keyboard_interactive = false;
    params.password = false;
    params.connect_timeout = std::time::Duration::from_secs(10);
    params
}

async fn connect(params: ConnectParams, asked: &Asked) -> Result<Session> {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        Session::connect(params, asked),
    )
    .await
    .expect("the login finished")
}

fn key_file(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path
}

#[tokio::test]
async fn an_encrypted_key_the_server_refuses_never_asks_for_its_passphrase() {
    let dir = tempfile::tempdir().unwrap();
    let encrypted = key_file(dir.path(), "id_encrypted", ENCRYPTED);
    let plain = key_file(dir.path(), "id_plain", PLAIN);
    let door = Door {
        key: Some(public_of(PLAIN)),
        ..Door::default()
    };
    let port = serve_once(door, &[MethodKind::PublicKey]).await;
    let mut params = client(port);
    // The encrypted key comes first, and the server does not want it.
    params.identity_files = vec![encrypted, plain.clone()];
    let asked = Asked::default();
    let session = connect(params, &asked).await.expect("login");
    assert_eq!(session.auth_method(), &AuthMethod::IdentityFile(plain));
    assert_eq!(asked.passphrases.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn an_encrypted_key_the_server_accepts_asks_once_and_signs() {
    for (name, text) in [("id_ed25519", ENCRYPTED), ("id_rsa", ENCRYPTED_RSA)] {
        let dir = tempfile::tempdir().unwrap();
        let path = key_file(dir.path(), name, text);
        let door = Door {
            key: Some(public_of(text)),
            ..Door::default()
        };
        let port = serve_once(door, &[MethodKind::PublicKey]).await;
        let mut params = client(port);
        params.identity_files = vec![path.clone()];
        let asked = Asked::default();
        let session = connect(params, &asked).await.expect(name);
        assert_eq!(session.auth_method(), &AuthMethod::IdentityFile(path));
        assert_eq!(asked.passphrases.load(Ordering::SeqCst), 1, "{name}");
    }
}

#[tokio::test]
async fn a_pem_key_uses_its_pub_file_to_wait_for_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(dir.path(), "id_rsa", ENCRYPTED_RSA_PEM);
    key_file(dir.path(), "id_rsa.pub", ENCRYPTED_RSA_PEM_PUB);
    let part = read_public_part(&path).await.unwrap();
    assert_eq!(part.key, Some(public_of(ENCRYPTED_RSA_PEM_PUB)));
    // Refused: never asked.
    let port = serve_once(Door::default(), &[MethodKind::PublicKey]).await;
    let mut params = client(port);
    params.identity_files = vec![path];
    let asked = Asked::default();
    connect(params, &asked).await.unwrap_err();
    assert_eq!(asked.passphrases.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_saved_password_answers_a_keyboard_interactive_password_round() {
    let door = Door {
        password: Some("pw".into()),
        ..Door::default()
    };
    let port = serve_once(door, &[MethodKind::KeyboardInteractive]).await;
    let mut params = client(port);
    params.keyboard_interactive = true;
    let asked = Asked {
        saved: Mutex::new(Some("pw".into())),
        ..Asked::default()
    };
    let session = connect(params, &asked).await.expect("login");
    assert_eq!(session.auth_method(), &AuthMethod::KeyboardInteractive);
    assert_eq!(asked.kbd_rounds.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_refused_saved_password_is_followed_by_asking_the_user() {
    let door = Door {
        password: Some("right".into()),
        ..Door::default()
    };
    let port = serve_once(door, &[MethodKind::KeyboardInteractive]).await;
    let mut params = client(port);
    params.keyboard_interactive = true;
    let asked = Asked {
        saved: Mutex::new(Some("stale".into())),
        kbd_answer: Some("right".into()),
        ..Asked::default()
    };
    let session = connect(params, &asked).await.expect("login");
    assert_eq!(session.auth_method(), &AuthMethod::KeyboardInteractive);
    assert_eq!(asked.kbd_rounds.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_partial_success_runs_the_key_stage_again() {
    let door = Door {
        key: Some(public_of(PLAIN)),
        password: Some("pw".into()),
        kbd_first: true,
        ..Door::default()
    };
    // Only keyboard-interactive is offered at first.
    let port = serve_once(door.clone(), &[MethodKind::KeyboardInteractive]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = key_file(dir.path(), "id", PLAIN);
    let mut params = client(port);
    params.keyboard_interactive = true;
    params.identity_files = vec![path.clone()];
    let asked = Asked {
        kbd_answer: Some("pw".into()),
        ..Asked::default()
    };
    let session = connect(params, &asked).await.expect("login");
    assert_eq!(session.auth_method(), &AuthMethod::IdentityFile(path));
    assert_eq!(asked.kbd_rounds.load(Ordering::SeqCst), 1);
}

#[test]
fn identities_follow_openssh_order_and_are_offered_once() {
    use Candidate::{Agent, File};
    // Files a, b, c (c unreadable without a passphrase); the agent holds
    // x, b and a.
    let files = [Some("a"), Some("b"), None, Some("d")];
    let agent = ["x", "b", "a"];
    assert_eq!(
        order_identities(&files, &agent, false),
        vec![Agent(2), Agent(1), Agent(0), File(2), File(3)]
    );
    // IdentitiesOnly: the agent's other keys stay out.
    assert_eq!(
        order_identities(&files, &agent, true),
        vec![Agent(2), Agent(1), File(2), File(3)]
    );
    // The same key twice (a file and its copy) is offered once.
    assert_eq!(
        order_identities(&[Some("a"), Some("a")], &[] as &[&str], false),
        vec![File(0)]
    );
    assert_eq!(
        order_identities(&[Some("a"), Some("a")], &["a", "a"], false),
        vec![Agent(0)]
    );
}

#[test]
fn after_a_partial_success_the_remaining_methods_run_again() {
    let config = StageConfig {
        agent: false,
        identity_files: true,
        keyboard_interactive: true,
        password: true,
    };
    let kbd = [MethodKind::KeyboardInteractive];
    let pk = [MethodKind::PublicKey];
    let mut done = Vec::new();
    // The server first offers only keyboard-interactive.
    assert_eq!(
        next_stage(&config, &kbd, &done),
        Some(Stage::KeyboardInteractive)
    );
    done.push(Stage::KeyboardInteractive);
    // Refused without a partial success: nothing else to run.
    assert_eq!(next_stage(&config, &kbd, &done), None);
    // Partial success, publickey now required: done is cleared.
    done.clear();
    assert_eq!(next_stage(&config, &pk, &done), Some(Stage::PublicKey));
    done.push(Stage::PublicKey);
    assert_eq!(next_stage(&config, &pk, &done), None);
    // A disabled method never runs, whatever the server wants.
    let no_password = StageConfig {
        password: false,
        ..config
    };
    assert_eq!(next_stage(&no_password, &[MethodKind::Password], &[]), None);
}

#[test]
fn password_rounds_are_recognised() {
    let prompt = |text: &str, echo: bool| KbdPrompt {
        text: text.into(),
        echo,
    };
    assert!(asks_only_for_password(&[prompt("Password: ", false)]));
    assert!(asks_only_for_password(&[prompt(
        "(me@host) Password:",
        false
    )]));
    assert!(!asks_only_for_password(&[prompt("Password: ", true)]));
    assert!(!asks_only_for_password(&[prompt(
        "Verification code: ",
        false
    )]));
    assert!(!asks_only_for_password(&[prompt("New password: ", false)]));
    assert!(!asks_only_for_password(&[
        prompt("Password: ", false),
        prompt("OTP: ", false)
    ]));
    assert!(!asks_only_for_password(&[]));
}

#[test]
fn public_parts_are_read_without_the_passphrase() {
    let plain = public_part(PLAIN, None);
    assert!(plain.key.is_some() && !plain.public_only);
    // OpenSSH format keeps the public key in clear.
    let encrypted = public_part(ENCRYPTED, None);
    assert!(encrypted.key.is_some());
    // An encrypted PEM key needs its `.pub`.
    assert_eq!(public_part(ENCRYPTED_RSA_PEM, None).key, None);
    assert_eq!(
        public_part(ENCRYPTED_RSA_PEM, Some(ENCRYPTED_RSA_PEM_PUB)).key,
        Some(public_of(ENCRYPTED_RSA_PEM_PUB))
    );
    // An `IdentityFile` naming the `.pub` itself.
    let only = public_part(ENCRYPTED_RSA_PEM_PUB, None);
    assert!(only.public_only && only.key.is_some());
}
