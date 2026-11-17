use crate::*;
use easyssh_core::{Connection, ConnectionTarget};
use easyssh_crypto::{protect, DeviceKey, KdfParams, KeyStore, MemoryKeyStore, SecretText};
use easyssh_store::{SecretChange, SecretsUpdate, Store};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
