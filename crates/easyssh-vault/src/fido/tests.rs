use super::*;
use crate::Vault;
use easyssh_crypto::KdfParams;
const FAST: KdfParams = KdfParams::insecure_for_tests();
use easyssh_crypto::MemoryKeyStore;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A software authenticator: every credential has a secret, the HMAC is
/// SHA-256-free (secret xor salt) because only determinism matters here.
struct Fake {
    present: Mutex<bool>,
    supports: bool,
    pin: Mutex<Option<String>>,
    next_error: Mutex<Option<FidoError>>,
    credentials: Mutex<Vec<(Vec<u8>, [u8; 32])>>,
    calls: AtomicUsize,
    own_dialog: bool,
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            present: Mutex::new(true),
            supports: true,
            pin: Mutex::new(None),
            next_error: Mutex::new(None),
            credentials: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            own_dialog: false,
        })
    }

    fn unplug(&self) {
        *self.present.lock().unwrap() = false;
    }

    fn fail_next(&self, error: FidoError) {
        *self.next_error.lock().unwrap() = Some(error);
    }

    fn check(&self, pin: Option<&str>) -> Result<(), FidoError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if !*self.present.lock().unwrap() {
            return Err(FidoError::NoDevice);
        }
        if let Some(error) = self.next_error.lock().unwrap().take() {
            return Err(error);
        }
        if let Some(expected) = &*self.pin.lock().unwrap() {
            match pin {
                None => return Err(FidoError::PinRequired),
                Some(given) if given != expected => {
                    return Err(FidoError::PinInvalid { retries: Some(5) })
                }
                Some(_) => {}
            }
        }
        Ok(())
    }
}

impl HmacAuthenticator for Fake {
    fn available(&self) -> bool {
        true
    }

    fn handles_user_verification(&self) -> bool {
        self.own_dialog
    }

    fn make_credential(&self, pin: Option<&str>) -> Result<Vec<u8>, FidoError> {
        self.check(pin)?;
        if !self.supports {
            return Err(FidoError::Unsupported);
        }
        let mut credentials = self.credentials.lock().unwrap();
        let n = credentials.len() as u8;
        let id = vec![n + 1; 40];
        credentials.push((id.clone(), [n + 10; 32]));
        Ok(id)
    }

    fn hmac_secret(
        &self,
        credential_id: &[u8],
        salt: &[u8; 32],
        pin: Option<&str>,
    ) -> Result<Zeroizing<[u8; 32]>, FidoError> {
        self.check(pin)?;
        let credentials = self.credentials.lock().unwrap();
        let (_, secret) = credentials
            .iter()
            .find(|(id, _)| id == credential_id)
            .ok_or(FidoError::NoCredential)?;
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = secret[i] ^ salt[i];
        }
        Ok(Zeroizing::new(out))
    }
}

#[derive(Default)]
struct Prompts {
    live: Arc<AtomicUsize>,
    shown: AtomicUsize,
    pins: Mutex<VecDeque<Option<String>>>,
    pin_asks: Mutex<Vec<Option<u32>>>,
}

struct Notice(Arc<AtomicUsize>);

impl Drop for Notice {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl UnlockPrompt for Prompts {
    fn touch(&self, _label: &str) -> Box<dyn Send> {
        self.live.fetch_add(1, Ordering::SeqCst);
        self.shown.fetch_add(1, Ordering::SeqCst);
        Box::new(Notice(self.live.clone()))
    }

    fn pin(&self, _label: &str, retries: Option<u32>) -> Option<String> {
        self.pin_asks.lock().unwrap().push(retries);
        self.pins.lock().unwrap().pop_front().flatten()
    }
}

fn setup() -> (
    Arc<Fake>,
    Arc<MemoryKeyStore>,
    Arc<Prompts>,
    SecurityKeyProvider,
) {
    let fake = Fake::new();
    let keystore = Arc::new(MemoryKeyStore::new());
    let prompts = Arc::new(Prompts::default());
    let provider = SecurityKeyProvider::new(fake.clone(), keystore.clone());
    provider.set_prompt(prompts.clone());
    (fake, keystore, prompts, provider)
}

#[test]
fn enrolling_wraps_the_key_and_unlocking_returns_it() {
    let (_, _, prompts, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    assert!(provider.available());
    assert!(!provider.enrolled());
    let item = provider.add_item(&key, "Desk key").unwrap();
    assert_eq!(item.label, "Desk key");
    assert!(provider.enrolled());
    assert_eq!(provider.try_unlock().unwrap().unwrap(), key);
    // Enrolling touches twice (create, then the proof), unlocking once; all
    // notices are closed afterwards.
    assert_eq!(prompts.shown.load(Ordering::SeqCst), 3);
    assert_eq!(prompts.live.load(Ordering::SeqCst), 0);
}

#[test]
fn the_stored_copy_holds_neither_the_key_nor_the_hmac_secret() {
    let (_, keystore, _, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    provider.add_item(&key, "k").unwrap();
    let stored = keystore.get(SERVICE, ACCOUNT).unwrap().unwrap();
    assert!(!stored
        .windows(32)
        .any(|window| window == key.expose().as_slice()));
}

#[test]
fn nothing_enrolled_means_no_prompt_and_no_device_call() {
    let (fake, _, prompts, provider) = setup();
    assert!(provider.try_unlock().unwrap().is_none());
    assert_eq!(fake.calls.load(Ordering::SeqCst), 0);
    assert_eq!(prompts.shown.load(Ordering::SeqCst), 0);
}

#[test]
fn several_keys_each_open_the_vault_and_each_can_be_removed() {
    let (fake, _, _, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    let first = provider.add_item(&key, "one").unwrap();
    let second = provider.add_item(&key, "two").unwrap();
    assert_eq!(provider.items().len(), 2);
    assert_ne!(first.id, second.id);

    // Only the second device is plugged in: the first entry is skipped.
    fake.credentials.lock().unwrap().remove(0);
    assert_eq!(provider.try_unlock().unwrap().unwrap(), key);

    provider.remove_item(&second.id).unwrap();
    assert_eq!(provider.items(), vec![first.clone()]);
    // The remaining entry's credential is gone from the device.
    assert!(matches!(
        provider.try_unlock(),
        Err(VaultError::UnlockFailed(_))
    ));
    provider.remove_item(&first.id).unwrap();
    assert!(!provider.enrolled());
    // Removing again is harmless.
    provider.remove_item(&first.id).unwrap();
}

#[test]
fn at_most_four_keys_and_labels_are_cleaned() {
    let (_, keystore, _, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    let item = provider
        .add_item(&key, &format!("  {}\n", "x".repeat(80)))
        .unwrap();
    assert_eq!(item.label.chars().count(), MAX_LABEL_CHARS);
    assert_eq!(
        provider.add_item(&key, " ").unwrap().label,
        "Security key 2"
    );
    for _ in 2..MAX_KEYS {
        provider.add_item(&key, "more").unwrap();
    }
    assert!(matches!(
        provider.add_item(&key, "one too many"),
        Err(VaultError::Input(_))
    ));
    // The whole list stays under the Windows Credential Manager limit.
    assert!(keystore.get(SERVICE, ACCOUNT).unwrap().unwrap().len() < 2560);
}

#[test]
fn a_key_without_hmac_secret_is_refused_and_nothing_is_stored() {
    let fake = Arc::new(Fake {
        supports: false,
        ..Arc::try_unwrap(Fake::new()).ok().unwrap()
    });
    let keystore = Arc::new(MemoryKeyStore::new());
    let provider = SecurityKeyProvider::new(fake, keystore.clone());
    let error = provider
        .add_item(&DeviceKey::generate().unwrap(), "k")
        .unwrap_err();
    assert!(matches!(error, VaultError::UnlockFailed(why) if why.contains("hmac-secret")));
    assert!(!provider.enrolled());
    assert!(keystore.get(SERVICE, ACCOUNT).unwrap().is_none());
}

#[test]
fn a_cancelled_touch_turns_nothing_on_and_falls_back_when_unlocking() {
    let (fake, _, _, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    fake.fail_next(FidoError::NotTouched);
    assert!(matches!(
        provider.add_item(&key, "k"),
        Err(VaultError::UnlockDeclined)
    ));
    assert!(!provider.enrolled());

    provider.add_item(&key, "k").unwrap();
    fake.fail_next(FidoError::NotTouched);
    assert!(provider.try_unlock().unwrap().is_none());
    // Still enrolled: one missed touch does not turn the method off.
    assert!(provider.enrolled());
}

#[test]
fn an_unplugged_key_is_reported_so_the_unlock_button_can_say_so() {
    let (fake, _, _, provider) = setup();
    provider
        .add_item(&DeviceKey::generate().unwrap(), "k")
        .unwrap();
    fake.unplug();
    let error = provider.try_unlock().unwrap_err();
    assert!(matches!(error, VaultError::UnlockFailed(why) if why.contains("no security key")));
}

#[test]
fn the_pin_is_asked_for_when_the_key_wants_it() {
    let (fake, _, prompts, provider) = setup();
    *fake.pin.lock().unwrap() = Some("1234".into());
    let key = DeviceKey::generate().unwrap();
    // Enrolling: a wrong PIN first, then the right one, then the same PIN is
    // asked again for the proof (it is never kept between calls).
    prompts.pins.lock().unwrap().extend([
        Some("0000".into()),
        Some("1234".into()),
        Some("1234".into()),
    ]);
    provider.add_item(&key, "k").unwrap();
    assert_eq!(*prompts.pin_asks.lock().unwrap(), [None, Some(5), None]);

    prompts.pins.lock().unwrap().push_back(Some("1234".into()));
    assert_eq!(provider.try_unlock().unwrap().unwrap(), key);
}

#[test]
fn a_cancelled_pin_prompt_falls_back_and_too_many_wrong_pins_stop() {
    let (fake, _, prompts, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    provider.add_item(&key, "k").unwrap();
    *fake.pin.lock().unwrap() = Some("1234".into());

    prompts.pins.lock().unwrap().push_back(None);
    assert!(provider.try_unlock().unwrap().is_none());

    prompts
        .pins
        .lock()
        .unwrap()
        .extend((0..5).map(|_| Some("9999".to_string())));
    let error = provider.try_unlock().unwrap_err();
    assert!(matches!(error, VaultError::UnlockFailed(why) if why.contains("wrong")));
}

#[test]
fn without_anyone_to_ask_a_pin_key_says_what_is_missing() {
    let fake = Fake::new();
    *fake.pin.lock().unwrap() = Some("1234".into());
    let provider = SecurityKeyProvider::new(fake, Arc::new(MemoryKeyStore::new()));
    let error = provider
        .add_item(&DeviceKey::generate().unwrap(), "k")
        .unwrap_err();
    assert!(matches!(error, VaultError::UnlockFailed(why) if why.contains("PIN")));
}

#[test]
fn a_backend_with_its_own_dialog_is_never_asked_for_a_pin_by_the_vault() {
    let fake = Arc::new(Fake {
        own_dialog: true,
        ..Arc::try_unwrap(Fake::new()).ok().unwrap()
    });
    *fake.pin.lock().unwrap() = Some("1234".into());
    let prompts = Arc::new(Prompts::default());
    let provider = SecurityKeyProvider::new(fake, Arc::new(MemoryKeyStore::new()));
    provider.set_prompt(prompts.clone());
    assert!(provider
        .add_item(&DeviceKey::generate().unwrap(), "k")
        .is_err());
    assert!(prompts.pin_asks.lock().unwrap().is_empty());
}

#[test]
fn a_key_that_answers_differently_is_reported_not_trusted() {
    let (fake, _, _, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    provider.add_item(&key, "k").unwrap();
    // The device was reset and the credential re-created under the same id.
    fake.credentials.lock().unwrap()[0].1 = [99; 32];
    let error = provider.try_unlock().unwrap_err();
    assert!(matches!(error, VaultError::UnlockFailed(why) if why.contains("add it again")));
}

#[test]
fn a_tampered_or_damaged_list_opens_nothing() {
    let (_, keystore, _, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    provider.add_item(&key, "k").unwrap();
    let mut stored = keystore.get(SERVICE, ACCOUNT).unwrap().unwrap().to_vec();

    // A flipped byte in the wrapped copy fails authentication.
    let last = stored.len() - 1;
    stored[last] ^= 1;
    keystore.set(SERVICE, ACCOUNT, &stored).unwrap();
    assert!(provider.try_unlock().is_err());

    // Garbage and truncation read as "nothing enrolled".
    keystore.set(SERVICE, ACCOUNT, b"junk").unwrap();
    assert!(!provider.enrolled());
    keystore
        .set(SERVICE, ACCOUNT, &stored[..stored.len() - 5])
        .unwrap();
    assert!(!provider.enrolled());
    assert!(provider.try_unlock().unwrap().is_none());
}

#[test]
fn the_wrapped_copy_is_bound_to_its_slot() {
    let (_, keystore, _, provider) = setup();
    let key = DeviceKey::generate().unwrap();
    provider.add_item(&key, "one").unwrap();
    provider.add_item(&key, "two").unwrap();
    let mut entries = decode(&keystore.get(SERVICE, ACCOUNT).unwrap().unwrap()).unwrap();
    // Swap the wrapped copies: each is only valid under its own id.
    let (a, b) = (entries[0].wrapped.clone(), entries[1].wrapped.clone());
    entries[0].wrapped = b;
    entries[1].wrapped = a;
    keystore.set(SERVICE, ACCOUNT, &encode(&entries)).unwrap();
    assert!(provider.try_unlock().is_err());
}

#[test]
fn the_list_round_trips() {
    let entries = vec![
        Entry {
            id: "abc".into(),
            label: "Büro 键".into(),
            credential_id: vec![1; 70],
            salt: [3; 32],
            wrapped: vec![9; 73],
        },
        Entry {
            id: "def".into(),
            label: String::new(),
            credential_id: vec![2; 1],
            salt: [4; 32],
            wrapped: vec![],
        },
    ];
    assert_eq!(decode(&encode(&entries)).unwrap(), entries);
    assert_eq!(decode(&encode(&[])).unwrap(), Vec::new());
    assert!(decode(&[]).is_none());
    assert!(decode(&[2, 0]).is_none());
}

#[test]
fn removing_the_passphrase_removes_the_copies_and_the_method_works_in_a_vault() {
    let keystore = Arc::new(MemoryKeyStore::new());
    let fake = Fake::new();
    let make = |keystore: &Arc<MemoryKeyStore>| {
        let vault = Vault::with_kdf(keystore.clone(), FAST);
        vault.add_unlock_provider(Arc::new(SecurityKeyProvider::new(
            fake.clone(),
            keystore.clone(),
        )));
        vault
    };
    let setup = make(&keystore);
    setup.ensure_unlocked().unwrap();
    setup.set_passphrase("pw").unwrap();

    let vault = make(&keystore);
    assert!(matches!(
        vault.add_unlock_item(METHOD, "wrong", "k"),
        Err(VaultError::WrongPassphrase)
    ));
    let item = vault.add_unlock_item(METHOD, "pw", "Desk").unwrap();
    let methods = vault.unlock_methods();
    assert!(methods[0].enabled);
    assert_eq!(methods[0].items, vec![item]);

    // A fresh process opens the vault by touch alone.
    let again = make(&keystore);
    assert!(!again.is_unlocked());
    again.unlock_with(METHOD).unwrap();
    assert!(again.is_unlocked());

    again.remove_passphrase("pw").unwrap();
    assert!(!again.unlock_methods()[0].enabled);
}

#[test]
fn a_method_that_holds_one_entry_refuses_items() {
    struct Single;
    impl UnlockProvider for Single {
        fn name(&self) -> &'static str {
            "single"
        }
        fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError> {
            Ok(None)
        }
    }
    let key = DeviceKey::generate().unwrap();
    assert!(Single.items().is_empty());
    assert!(Single.add_item(&key, "x").is_err());
    assert!(Single.remove_item("x").is_err());
}
