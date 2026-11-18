use crate::*;
use easyssh_core::{Connection, ConnectionTarget};
use easyssh_crypto::{protect, DeviceKey, KdfParams, KeyStore, MemoryKeyStore, SecretText};
use easyssh_store::{SecretChange, SecretsUpdate, Store};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

const FAST: KdfParams = KdfParams::insecure_for_tests();

fn vault_over(keystore: Arc<MemoryKeyStore>) -> Arc<Vault> {
    Arc::new(Vault::with_kdf(keystore, FAST))
}

fn vault() -> Arc<Vault> {
    vault_over(Arc::new(MemoryKeyStore::new()))
}

fn store_for(vault: &Vault) -> Store {
    Store::open_in_memory("dev", vault.cipher()).unwrap()
}

fn endpoint(store: &Store, user: &str, hostname: &str) -> Connection {
    let mut connection = Connection::alias("web", "x");
    connection.target = ConnectionTarget::Endpoint {
        hostname: hostname.into(),
        username: Some(user.into()),
        port: 22,
    };
    store.save_host(&connection, None).unwrap();
    connection
}

fn set(value: &str) -> SecretChange {
    SecretChange::Set(SecretText::new(value))
}

fn password(value: &str) -> SecretsUpdate {
    SecretsUpdate {
        password: set(value),
        ..SecretsUpdate::default()
    }
}

#[test]
fn without_a_passphrase_the_first_use_unlocks_from_the_keychain() {
    let keystore = Arc::new(MemoryKeyStore::new());
    let vault = vault_over(keystore.clone());
    let store = store_for(&vault);
    let host = endpoint(&store, "alice", "example.test");
    assert!(!vault.is_unlocked());
    assert!(!vault.status().passphrase_set);
    vault
        .set_host_secrets(&store, &host.id, &password("pw"))
        .unwrap();
    assert!(vault.is_unlocked());
    // After a lock, the next use reads the keychain again, no prompt.
    vault.lock();
    assert!(!vault.is_unlocked());
    let secrets = vault.host_secrets(&store, &host.id).unwrap();
    assert_eq!(secrets.password.unwrap().expose(), "pw");
    // A second vault over the same keychain has the same key.
    let again = vault_over(keystore);
    again.ensure_unlocked().unwrap();
    let blob = vault.cipher().encrypt(b"x", b"aad").unwrap();
    assert_eq!(again.cipher().decrypt(&blob, b"aad").unwrap(), b"x");
}

#[test]
fn lock_now_and_unlock_with_a_passphrase() {
    let vault = vault();
    let store = store_for(&vault);
    let host = endpoint(&store, "alice", "example.test");
    vault
        .set_host_secrets(&store, &host.id, &password("pw"))
        .unwrap();
    vault.set_passphrase("correct horse").unwrap();
    assert!(vault.status().passphrase_set);
    assert!(vault.is_unlocked());
    vault.lock();
    // Flags work while locked; values do not.
    assert!(vault.flags(&store, &host.id).unwrap().password);
    assert!(matches!(
        vault.host_secrets(&store, &host.id),
        Err(VaultError::Locked)
    ));
    assert!(matches!(
        vault.set_host_secrets(&store, &host.id, &password("new")),
        Err(VaultError::Locked)
    ));
    assert!(matches!(
        vault.unlock("wrong"),
        Err(VaultError::WrongPassphrase)
    ));
    assert!(!vault.is_unlocked());
    vault.unlock("correct horse").unwrap();
    let secrets = vault.host_secrets(&store, &host.id).unwrap();
    assert_eq!(secrets.password.unwrap().expose(), "pw");
}

#[test]
fn keychain_alone_is_not_enough_once_a_passphrase_is_set() {
    let keystore = Arc::new(MemoryKeyStore::new());
    let first = vault_over(keystore.clone());
    first.ensure_unlocked().unwrap();
    first.set_passphrase("pw").unwrap();
    // A new process: same keychain, nothing in memory.
    let second = vault_over(keystore.clone());
    assert!(matches!(second.ensure_unlocked(), Err(VaultError::Locked)));
    assert!(matches!(
        protect::open_device_key(&*keystore, None),
        Err(protect::ProtectError::NeedsPassphrase)
    ));
}

#[test]
fn change_and_remove_the_passphrase() {
    let vault = vault();
    vault.set_passphrase("one").unwrap();
    assert!(matches!(
        vault.set_passphrase("again"),
        Err(VaultError::Passphrase(_))
    ));
    assert!(matches!(
        vault.change_passphrase("bad", "two"),
        Err(VaultError::WrongPassphrase)
    ));
    vault.change_passphrase("one", "two").unwrap();
    vault.lock();
    assert!(vault.unlock("one").is_err());
    vault.unlock("two").unwrap();
    assert!(matches!(
        vault.remove_passphrase("nope"),
        Err(VaultError::WrongPassphrase)
    ));
    vault.remove_passphrase("two").unwrap();
    assert!(!vault.status().passphrase_set);
    vault.lock();
    vault.ensure_unlocked().unwrap();
}

#[test]
fn auto_lock_after_idle_minutes() {
    let vault = vault();
    assert_eq!(vault.auto_lock_minutes(), DEFAULT_AUTO_LOCK_MINUTES);
    vault.ensure_unlocked().unwrap();
    let now = Instant::now();
    assert!(!vault.lock_if_idle(now + Duration::from_secs(14 * 60)));
    assert!(vault.is_unlocked());
    assert!(vault.lock_if_idle(now + Duration::from_secs(15 * 60 + 1)));
    assert!(!vault.is_unlocked());
    // Locked already: nothing to do.
    assert!(!vault.lock_if_idle(now + Duration::from_secs(3600)));
}

#[test]
fn auto_lock_can_change_or_be_turned_off_and_is_saved() {
    let vault = vault();
    let store = store_for(&vault);
    vault.save_auto_lock(&store, 2).unwrap();
    assert_eq!(vault.auto_lock_minutes(), 2);
    let other = self::vault();
    other.load_settings(&store);
    assert_eq!(other.auto_lock_minutes(), 2);
    vault.set_auto_lock_minutes(0).unwrap();
    vault.ensure_unlocked().unwrap();
    assert!(!vault.lock_if_idle(Instant::now() + Duration::from_secs(10 * 24 * 3600)));
    assert!(vault
        .set_auto_lock_minutes(MAX_AUTO_LOCK_MINUTES + 1)
        .is_err());
}

#[test]
fn activity_restarts_the_idle_timer() {
    let vault = vault();
    vault.set_auto_lock_minutes(1).unwrap();
    vault.ensure_unlocked().unwrap();
    let start = Instant::now();
    // Using the vault again just before the limit pushes it out.
    std::thread::sleep(Duration::from_millis(30));
    vault.ensure_unlocked().unwrap();
    assert!(!vault.lock_if_idle(start + Duration::from_secs(59)));
}

#[test]
fn the_vault_locks_the_moment_the_app_leaves_the_screen_by_default() {
    let vault = vault();
    vault.ensure_unlocked().unwrap();
    assert_eq!(vault.background_lock_seconds(), 0);
    let now = SystemTime::now();
    assert!(vault.entered_background(now));
    assert!(!vault.is_unlocked());
    // Coming back finds it locked already; nothing more to do.
    assert!(!vault.returned_to_foreground(now + Duration::from_secs(1)));
}

#[test]
fn a_background_lock_time_gives_a_grace_period() {
    let vault = vault();
    vault.set_background_lock_seconds(300).unwrap();
    vault.ensure_unlocked().unwrap();
    let left = SystemTime::now();
    assert!(!vault.entered_background(left));
    assert!(vault.is_unlocked());
    // Back after four minutes: still open.
    assert!(!vault.returned_to_foreground(left + Duration::from_secs(240)));
    assert!(vault.is_unlocked());
    // Away for exactly the limit: locked.
    assert!(!vault.entered_background(left));
    assert!(vault.returned_to_foreground(left + Duration::from_secs(300)));
    assert!(!vault.is_unlocked());
}

#[test]
fn a_clock_that_went_backwards_locks_instead_of_extending_the_grace_period() {
    let vault = vault();
    vault.set_background_lock_seconds(300).unwrap();
    vault.ensure_unlocked().unwrap();
    let left = SystemTime::now();
    vault.entered_background(left);
    assert!(vault.returned_to_foreground(left - Duration::from_secs(3600)));
    assert!(!vault.is_unlocked());
}

#[test]
fn returning_without_having_left_changes_nothing() {
    let vault = vault();
    vault.ensure_unlocked().unwrap();
    assert!(!vault.returned_to_foreground(SystemTime::now()));
    assert!(vault.is_unlocked());
}

#[test]
fn the_background_lock_is_limited_and_saved() {
    let first = vault();
    assert!(first
        .set_background_lock_seconds(MAX_BACKGROUND_LOCK_SECONDS + 1)
        .is_err());
    let store = store_for(&first);
    first.save_background_lock(&store, 60).unwrap();
    let again = vault();
    assert_eq!(again.background_lock_seconds(), 0);
    again.load_settings(&store);
    assert_eq!(again.background_lock_seconds(), 60);
}

type Release = Mutex<Option<std::sync::mpsc::Receiver<()>>>;

#[test]
fn leaving_the_screen_during_a_system_prompt_does_not_cancel_the_unlock() {
    struct Slow(DeviceKey, std::sync::mpsc::Sender<()>, Release);
    impl UnlockProvider for Slow {
        fn name(&self) -> &'static str {
            "slow"
        }
        fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError> {
            // The prompt is up: tell the test, then wait for it to go away.
            self.1.send(()).unwrap();
            let release = self.2.lock().unwrap().take().unwrap();
            let _ = release.recv();
            Ok(Some(DeviceKey::from_slice(self.0.expose()).unwrap()))
        }
    }
    let keystore = Arc::new(MemoryKeyStore::new());
    let first = vault_over(keystore.clone());
    first.ensure_unlocked().unwrap();
    let key = protect::open_device_key(&*keystore, None).unwrap();
    first.set_passphrase("pw").unwrap();
    let vault = vault_over(keystore);
    let (asked_tx, asked_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    vault.add_unlock_provider(Arc::new(Slow(key, asked_tx, Mutex::new(Some(release_rx)))));
    let background = vault.clone();
    let worker = std::thread::spawn(move || background.ensure_unlocked());
    asked_rx.recv().unwrap();
    // The OS reports the app inactive while the prompt shows.
    assert!(!vault.entered_background(SystemTime::now()));
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
    assert!(vault.is_unlocked());
}

struct Provider(DeviceKey);
impl UnlockProvider for Provider {
    fn name(&self) -> &'static str {
        "test"
    }
    fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError> {
        Ok(Some(DeviceKey::from_slice(self.0.expose()).unwrap()))
    }
}

#[test]
fn an_unlock_provider_can_open_a_passphrase_vault() {
    let keystore = Arc::new(MemoryKeyStore::new());
    let first = vault_over(keystore.clone());
    first.ensure_unlocked().unwrap();
    let key = protect::open_device_key(&*keystore, None).unwrap();
    first.set_passphrase("pw").unwrap();
    let second = vault_over(keystore);
    assert!(matches!(second.ensure_unlocked(), Err(VaultError::Locked)));
    second.add_unlock_provider(Arc::new(Provider(key)));
    second.ensure_unlocked().unwrap();
    assert!(second.is_unlocked());
}

#[test]
fn unavailable_keychain_is_reported_not_papered_over() {
    struct Broken;
    impl KeyStore for Broken {
        fn get(
            &self,
            _: &str,
            _: &str,
        ) -> Result<Option<zeroize::Zeroizing<Vec<u8>>>, easyssh_crypto::KeyStoreError> {
            Err(easyssh_crypto::KeyStoreError::Unavailable(
                "no service".into(),
            ))
        }
        fn set(&self, _: &str, _: &str, _: &[u8]) -> Result<(), easyssh_crypto::KeyStoreError> {
            Err(easyssh_crypto::KeyStoreError::Unavailable(
                "no service".into(),
            ))
        }
        fn delete(&self, _: &str, _: &str) -> Result<(), easyssh_crypto::KeyStoreError> {
            Ok(())
        }
    }
    let vault = Vault::with_kdf(Arc::new(Broken), FAST);
    let status = vault.status();
    assert!(!status.keychain_available);
    assert!(status.keychain_error.unwrap().contains("no service"));
    let error = vault.ensure_unlocked().unwrap_err();
    assert_eq!(error.code(), "keychain_unavailable");
    assert!(!vault.is_unlocked());
}

struct Fixture {
    secrets: VaultSecrets,
    store: Arc<Mutex<Store>>,
    host: Connection,
}

fn fixture() -> Fixture {
    let vault = vault();
    let store = Arc::new(Mutex::new(store_for(&vault)));
    let host = endpoint(&store.lock().unwrap(), "alice", "example.test");
    let access: Arc<dyn StoreAccess> = store.clone();
    Fixture {
        secrets: VaultSecrets::new(vault, access),
        store,
        host,
    }
}

#[test]
fn secret_source_returns_saved_values_and_locked_when_locked() {
    let f = fixture();
    let nothing = f.secrets.host_secrets(&f.host.id).unwrap();
    assert!(nothing.password.is_none() && nothing.private_key.is_none());
    {
        let store = f.store.lock().unwrap();
        f.secrets
            .vault()
            .set_host_secrets(
                &store,
                &f.host.id,
                &SecretsUpdate {
                    password: set("pw"),
                    private_key: set("KEY"),
                    passphrase: set("phrase"),
                },
            )
            .unwrap();
    }
    let source: &dyn SecretSource = &f.secrets;
    let got = source.host_secrets(&f.host.id).unwrap();
    assert_eq!(got.private_key.unwrap().expose(), "KEY");
    f.secrets.vault().set_passphrase("pp").unwrap();
    f.secrets.vault().lock();
    assert!(matches!(
        source.host_secrets(&f.host.id),
        Err(VaultError::Locked)
    ));
}

#[test]
fn prompt_answers_and_remembering() {
    let f = fixture();
    let binding = Binding::for_connection(&f.host, None);
    assert!(matches!(
        f.secrets.answer(&binding, SecretKind::Password),
        Answer::None
    ));
    f.secrets
        .remember(&binding, SecretKind::Password, SecretText::new("pw"))
        .unwrap();
    f.secrets
        .remember(&binding, SecretKind::KeyPassphrase, SecretText::new("ph"))
        .unwrap();
    match f.secrets.answer(&binding, SecretKind::Password) {
        Answer::Secret(s) => assert_eq!(s.expose(), "pw"),
        other => panic!("{other:?}"),
    }
    match f.secrets.answer(&binding, SecretKind::KeyPassphrase) {
        Answer::Secret(s) => assert_eq!(s.expose(), "ph"),
        other => panic!("{other:?}"),
    }
    // Another host never gets this host's secrets.
    let other = {
        let store = f.store.lock().unwrap();
        endpoint(&store, "alice", "other.test")
    };
    let other_binding = Binding::for_connection(&other, None);
    assert!(matches!(
        f.secrets.answer(&other_binding, SecretKind::Password),
        Answer::None
    ));
    // Locked vault: say so instead of answering.
    f.secrets.vault().set_passphrase("pp").unwrap();
    f.secrets.vault().lock();
    assert!(matches!(
        f.secrets.answer(&binding, SecretKind::Password),
        Answer::Locked
    ));
    assert!(f
        .secrets
        .remember(&binding, SecretKind::Password, SecretText::new("x"))
        .is_err());
}

#[test]
fn forget_all_removes_everything_even_while_locked() {
    let f = fixture();
    let binding = Binding::for_connection(&f.host, None);
    f.secrets
        .remember(&binding, SecretKind::Password, SecretText::new("pw"))
        .unwrap();
    f.secrets.vault().set_passphrase("pp").unwrap();
    f.secrets.vault().lock();
    let store = f.store.lock().unwrap();
    assert_eq!(f.secrets.vault().forget_all(&store).unwrap(), 1);
    assert!(!f.secrets.vault().flags(&store, &f.host.id).unwrap().any());
}

#[test]
fn derived_keys_need_the_vault_and_are_stable_per_salt() {
    let vault = vault();
    vault.set_passphrase("pp").unwrap();
    vault.lock();
    assert!(matches!(
        vault.derive_key(b"info", b"salt"),
        Err(VaultError::Locked)
    ));
    vault.unlock("pp").unwrap();
    let a = vault.derive_key(b"info", b"salt").unwrap();
    assert_eq!(*a, *vault.derive_key(b"info", b"salt").unwrap());
    assert_ne!(*a, *vault.derive_key(b"info", b"other").unwrap());
    // The same device key after a lock and unlock gives the same key.
    vault.lock();
    vault.unlock("pp").unwrap();
    assert_eq!(*a, *vault.derive_key(b"info", b"salt").unwrap());
}

#[test]
fn single_values_are_sealed_to_their_slot() {
    let vault = vault();
    let blob = vault.seal_value(b"ai/anthropic", b"sk-test-123").unwrap();
    assert!(!blob.windows(5).any(|w| w == b"sk-te"));
    assert_eq!(
        &vault.open_value(b"ai/anthropic", &blob).unwrap()[..],
        b"sk-test-123"
    );
    assert!(vault.open_value(b"ai/openai", &blob).is_err());
}

// ---- biometric and OS unlock methods (with fake gates) -------------------

mod unlock_methods {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A gate the test controls.
    struct FakeGate {
        available: AtomicBool,
        approve: AtomicBool,
        asked: AtomicUsize,
    }

    impl FakeGate {
        fn new(approve: bool) -> Arc<Self> {
            Arc::new(Self {
                available: AtomicBool::new(true),
                approve: AtomicBool::new(approve),
                asked: AtomicUsize::new(0),
            })
        }
        fn asked(&self) -> usize {
            self.asked.load(Ordering::SeqCst)
        }
    }

    impl PresenceGate for FakeGate {
        fn available(&self) -> bool {
            self.available.load(Ordering::SeqCst)
        }
        fn verify(&self, _reason: &str) -> Result<bool, VaultError> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            Ok(self.approve.load(Ordering::SeqCst))
        }
    }

    fn provider(
        name: &'static str,
        gate: &Arc<FakeGate>,
        keystore: &Arc<MemoryKeyStore>,
    ) -> Arc<GatedKeyProvider> {
        Arc::new(GatedKeyProvider::new(
            name,
            if name == "first" {
                "k-first"
            } else {
                "k-second"
            },
            gate.clone(),
            keystore.clone(),
        ))
    }

    /// A vault with a passphrase, locked, plus a fresh vault over the same
    /// key chain to unlock.
    fn locked_vault(keystore: &Arc<MemoryKeyStore>) -> Arc<Vault> {
        let setup = vault_over(keystore.clone());
        setup.ensure_unlocked().unwrap();
        setup.set_passphrase("pw").unwrap();
        vault_over(keystore.clone())
    }

    #[test]
    fn enabling_needs_the_passphrase_and_then_unlocks_without_it() {
        let keystore = Arc::new(MemoryKeyStore::new());
        let vault = locked_vault(&keystore);
        let gate = FakeGate::new(true);
        vault.add_unlock_provider(provider("first", &gate, &keystore));
        assert_eq!(
            vault.unlock_methods(),
            vec![UnlockMethod {
                name: "first",
                available: true,
                enabled: false,
                items: Vec::new(),
            }]
        );
        // Not enabled yet: the passphrase is the only way.
        assert!(matches!(vault.ensure_unlocked(), Err(VaultError::Locked)));
        assert_eq!(gate.asked(), 0);

        assert!(matches!(
            vault.enable_unlock("first", "wrong"),
            Err(VaultError::WrongPassphrase)
        ));
        vault.enable_unlock("first", "pw").unwrap();
        assert!(vault.status().unlock_methods[0].enabled);
        assert!(vault.is_unlocked());

        vault.lock();
        vault.ensure_unlocked().unwrap();
        assert!(vault.is_unlocked());
        assert_eq!(gate.asked(), 2); // once to enable, once to unlock
    }

    #[test]
    fn a_declined_prompt_falls_back_to_the_passphrase_and_is_not_repeated() {
        let keystore = Arc::new(MemoryKeyStore::new());
        let vault = locked_vault(&keystore);
        let gate = FakeGate::new(true);
        vault.add_unlock_provider(provider("first", &gate, &keystore));
        vault.enable_unlock("first", "pw").unwrap();
        vault.lock();

        gate.approve.store(false, Ordering::SeqCst);
        let before = gate.asked();
        assert!(matches!(vault.ensure_unlocked(), Err(VaultError::Locked)));
        // Background uses within the cooldown do not raise the prompt again.
        assert!(matches!(vault.ensure_unlocked(), Err(VaultError::Locked)));
        assert_eq!(gate.asked(), before + 1);

        // The person presses the button: asked again, now approved.
        gate.approve.store(true, Ordering::SeqCst);
        vault.unlock_with("first").unwrap();
        assert!(vault.is_unlocked());

        // And the passphrase always works.
        vault.lock();
        gate.approve.store(false, Ordering::SeqCst);
        assert!(matches!(
            vault.unlock_with("first"),
            Err(VaultError::UnlockDeclined)
        ));
        vault.unlock("pw").unwrap();
        assert!(vault.is_unlocked());
    }

    #[test]
    fn providers_are_tried_in_order_and_unavailable_ones_are_skipped() {
        let keystore = Arc::new(MemoryKeyStore::new());
        let vault = locked_vault(&keystore);
        let first = FakeGate::new(true);
        let second = FakeGate::new(true);
        vault.add_unlock_provider(provider("first", &first, &keystore));
        vault.add_unlock_provider(provider("second", &second, &keystore));
        vault.enable_unlock("first", "pw").unwrap();
        vault.enable_unlock("second", "pw").unwrap();
        vault.lock();

        first.available.store(false, Ordering::SeqCst);
        let (a, b) = (first.asked(), second.asked());
        vault.ensure_unlocked().unwrap();
        assert_eq!((first.asked(), second.asked()), (a, b + 1));
        assert!(
            !vault.status().unlock_methods[0].available,
            "an unavailable method is reported as such"
        );
        assert!(matches!(
            vault.enable_unlock("first", "pw"),
            Err(VaultError::Input(_))
        ));

        // The first one declines: the second still gets its turn.
        vault.lock();
        first.available.store(true, Ordering::SeqCst);
        first.approve.store(false, Ordering::SeqCst);
        vault.unlock_with("second").unwrap();
        vault.lock();
        vault.add_unlock_provider(provider("first", &first, &keystore));
        vault.ensure_unlocked().unwrap();
    }

    #[test]
    fn a_refused_presence_check_does_not_turn_the_method_on() {
        let keystore = Arc::new(MemoryKeyStore::new());
        let vault = locked_vault(&keystore);
        let gate = FakeGate::new(false);
        vault.add_unlock_provider(provider("first", &gate, &keystore));
        assert!(matches!(
            vault.enable_unlock("first", "pw"),
            Err(VaultError::UnlockDeclined)
        ));
        assert!(!vault.status().unlock_methods[0].enabled);
        assert!(matches!(
            vault.enable_unlock("nope", "pw"),
            Err(VaultError::Input(_))
        ));
    }

    #[test]
    fn disabling_and_removing_the_passphrase_delete_the_copy() {
        let keystore = Arc::new(MemoryKeyStore::new());
        let vault = locked_vault(&keystore);
        let gate = FakeGate::new(true);
        vault.add_unlock_provider(provider("first", &gate, &keystore));
        vault.enable_unlock("first", "pw").unwrap();
        vault.disable_unlock("first").unwrap();
        assert!(!vault.status().unlock_methods[0].enabled);
        vault.lock();
        assert!(matches!(vault.ensure_unlocked(), Err(VaultError::Locked)));

        vault.enable_unlock("first", "pw").unwrap();
        vault.remove_passphrase("pw").unwrap();
        assert!(!vault.status().unlock_methods[0].enabled);
        assert!(keystore
            .get(easyssh_crypto::keystore::SERVICE, "k-first")
            .unwrap()
            .is_none());
    }

    #[test]
    fn a_vault_without_a_passphrase_has_nothing_to_enable() {
        let keystore = Arc::new(MemoryKeyStore::new());
        let vault = vault_over(keystore.clone());
        let gate = FakeGate::new(true);
        vault.add_unlock_provider(provider("first", &gate, &keystore));
        assert!(matches!(
            vault.enable_unlock("first", "pw"),
            Err(VaultError::Passphrase(_))
        ));
        assert_eq!(gate.asked(), 0);
    }
}
