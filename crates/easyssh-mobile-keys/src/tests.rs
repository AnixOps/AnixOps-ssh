use super::*;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use easyssh_crypto::{KdfParams, KeyStore, KeyStoreError};
use easyssh_vault::VaultError;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

const FAST: KdfParams = KdfParams::insecure_for_tests();

/// What the sensor does the next time it is asked.
#[derive(Clone, Debug)]
enum Sensor {
    Match,
    Cancel,
    Invalidate,
    Lockout,
    Broken(&'static str),
}

/// A phone: secure storage, a sensor, and a log of every call.
struct FakePhone {
    secure: Mutex<HashMap<(String, String), String>>,
    bound: Mutex<HashMap<String, String>>,
    sensor: Mutex<Sensor>,
    enrolled: Mutex<bool>,
    storage_down: Mutex<bool>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl FakePhone {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            secure: Mutex::default(),
            bound: Mutex::default(),
            sensor: Mutex::new(Sensor::Match),
            enrolled: Mutex::new(true),
            storage_down: Mutex::new(false),
            calls: Mutex::default(),
        })
    }

    fn sensor(&self, next: Sensor) {
        *self.sensor.lock().unwrap() = next;
    }

    fn no_fingerprints(&self) {
        *self.enrolled.lock().unwrap() = false;
    }

    fn prompts(&self) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(name, _)| name == command::BOUND_SEAL || name == command::BOUND_OPEN)
            .count()
    }

    fn log(&self) -> String {
        format!("{:?}", self.calls.lock().unwrap())
    }

    fn touch(&self) -> Result<(), BridgeError> {
        match self.sensor.lock().unwrap().clone() {
            Sensor::Match => Ok(()),
            Sensor::Cancel => Err(BridgeError::new(code::CANCELLED, "dismissed")),
            Sensor::Invalidate => Err(BridgeError::new(code::INVALIDATED, "key invalidated")),
            Sensor::Lockout => Err(BridgeError::new(code::UNAVAILABLE, "too many attempts")),
            Sensor::Broken(why) => Err(BridgeError::new("failed", why)),
        }
    }
}

fn text<'a>(args: &'a Value, field: &str) -> &'a str {
    args[field].as_str().unwrap()
}

impl NativeBridge for FakePhone {
    fn call(&self, name: &str, args: Value) -> Result<Value, BridgeError> {
        self.calls
            .lock()
            .unwrap()
            .push((name.to_owned(), args.clone()));
        let key = || {
            (
                text(&args, "service").to_owned(),
                text(&args, "account").to_owned(),
            )
        };
        if name.starts_with("secure") && *self.storage_down.lock().unwrap() {
            return Err(BridgeError::new(code::UNAVAILABLE, "no secure hardware"));
        }
        match name {
            command::SECURE_GET => {
                let value = self.secure.lock().unwrap().get(&key()).cloned();
                Ok(json!({ "value": value }))
            }
            command::SECURE_SET => {
                self.secure
                    .lock()
                    .unwrap()
                    .insert(key(), text(&args, "value").to_owned());
                Ok(json!({}))
            }
            command::SECURE_DELETE => {
                self.secure.lock().unwrap().remove(&key());
                Ok(json!({}))
            }
            command::BOUND_STATUS => Ok(json!({ "available": *self.enrolled.lock().unwrap() })),
            command::BOUND_CONTAINS => {
                let present = self
                    .bound
                    .lock()
                    .unwrap()
                    .contains_key(text(&args, "account"));
                Ok(json!({ "present": present }))
            }
            command::BOUND_SEAL => {
                self.touch()?;
                self.bound.lock().unwrap().insert(
                    text(&args, "account").to_owned(),
                    text(&args, "value").to_owned(),
                );
                Ok(json!({}))
            }
            command::BOUND_OPEN => {
                self.touch()?;
                let value = self
                    .bound
                    .lock()
                    .unwrap()
                    .get(text(&args, "account"))
                    .cloned();
                Ok(json!({ "value": value }))
            }
            command::BOUND_REMOVE => {
                self.bound.lock().unwrap().remove(text(&args, "account"));
                Ok(json!({}))
            }
            other => Err(BridgeError::new(
                "failed",
                format!("unknown command {other}"),
            )),
        }
    }
}

/// A vault on a phone that already has a passphrase and its biometric method
/// available but off.
fn phone_with_passphrase() -> (Arc<FakePhone>, Arc<Vault>) {
    let phone = FakePhone::new();
    let vault = mobile_vault_with(phone.clone(), FAST);
    vault.ensure_unlocked().unwrap();
    vault.set_passphrase("correct horse").unwrap();
    vault.lock();
    (phone, vault)
}

fn method(vault: &Vault) -> easyssh_vault::UnlockMethod {
    vault
        .unlock_methods()
        .into_iter()
        .find(|m| m.name == BIOMETRIC)
        .unwrap()
}

#[test]
fn the_device_key_is_created_once_in_the_secure_store_and_read_back_after_a_lock() {
    let phone = FakePhone::new();
    let vault = mobile_vault_with(phone.clone(), FAST);
    assert!(!vault.is_unlocked());
    vault.ensure_unlocked().unwrap();
    assert!(vault.is_unlocked());
    let stored = phone.secure.lock().unwrap().len();
    vault.lock();
    vault.ensure_unlocked().unwrap();
    // The same item again, not a second key.
    assert_eq!(phone.secure.lock().unwrap().len(), stored);
    let again = mobile_vault_with(phone.clone(), FAST);
    again.ensure_unlocked().unwrap();
    let blob = vault.cipher().encrypt(b"x", b"aad").unwrap();
    assert_eq!(again.cipher().decrypt(&blob, b"aad").unwrap(), b"x");
}

#[test]
fn a_passphrase_leaves_only_the_wrapped_key_in_the_secure_store() {
    let (phone, vault) = phone_with_passphrase();
    assert!(vault.status().passphrase_set);
    let accounts: Vec<String> = phone
        .secure
        .lock()
        .unwrap()
        .keys()
        .map(|(_, account)| account.clone())
        .collect();
    assert_eq!(
        accounts,
        [easyssh_crypto::keystore::WRAPPED_DEVICE_KEY_ACCOUNT]
    );
    // Without the passphrase and without a biometric copy the vault stays shut.
    assert!(matches!(vault.ensure_unlocked(), Err(VaultError::Locked)));
}

#[test]
fn biometrics_open_the_vault_without_the_passphrase_once_turned_on() {
    let (phone, vault) = phone_with_passphrase();
    assert!(method(&vault).available);
    assert!(!method(&vault).enabled);
    // Turning it on needs the passphrase once, and the sensor.
    assert!(matches!(
        vault.enable_unlock(BIOMETRIC, "wrong"),
        Err(VaultError::WrongPassphrase)
    ));
    assert_eq!(phone.prompts(), 0);
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    assert_eq!(phone.prompts(), 1);
    assert!(method(&vault).enabled);

    vault.lock();
    assert!(!vault.is_unlocked());
    vault.unlock_with(BIOMETRIC).unwrap();
    assert!(vault.is_unlocked());
    assert_eq!(phone.prompts(), 2);

    // And a normal use of a locked vault asks the sensor by itself.
    vault.lock();
    vault.ensure_unlocked().unwrap();
    assert!(vault.is_unlocked());
    assert_eq!(phone.prompts(), 3);
}

#[test]
fn a_dismissed_prompt_falls_back_to_the_passphrase() {
    let (phone, vault) = phone_with_passphrase();
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    vault.lock();
    phone.sensor(Sensor::Cancel);
    assert!(matches!(
        vault.unlock_with(BIOMETRIC),
        Err(VaultError::UnlockDeclined)
    ));
    assert!(!vault.is_unlocked());
    // Dismissing once does not turn the method off.
    assert!(method(&vault).enabled);
    vault.unlock("correct horse").unwrap();
    assert!(vault.is_unlocked());
}

#[test]
fn a_failed_enrolment_turns_nothing_on() {
    let (phone, vault) = phone_with_passphrase();
    phone.sensor(Sensor::Cancel);
    assert!(matches!(
        vault.enable_unlock(BIOMETRIC, "correct horse"),
        Err(VaultError::UnlockDeclined)
    ));
    assert!(!method(&vault).enabled);
    assert!(phone.bound.lock().unwrap().is_empty());
}

#[test]
fn a_new_fingerprint_invalidates_the_copy_but_not_the_vault() {
    let (phone, vault) = phone_with_passphrase();
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    vault.lock();
    phone.sensor(Sensor::Invalidate);
    // The automatic attempt gives up quietly.
    assert!(matches!(vault.ensure_unlocked(), Err(VaultError::Locked)));
    assert!(!method(&vault).enabled);
    assert!(phone.bound.lock().unwrap().is_empty());
    // The passphrase still opens it, and the method can be turned on again.
    vault.unlock("correct horse").unwrap();
    phone.sensor(Sensor::Match);
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    assert!(method(&vault).enabled);
}

#[test]
fn a_lockout_and_a_broken_sensor_are_told_apart() {
    let (phone, vault) = phone_with_passphrase();
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    vault.lock();
    phone.sensor(Sensor::Lockout);
    assert!(matches!(vault.ensure_unlocked(), Err(VaultError::Locked)));
    phone.sensor(Sensor::Broken("hardware error"));
    let error = vault.unlock_with(BIOMETRIC).unwrap_err();
    assert!(matches!(error, VaultError::UnlockFailed(why) if why == "hardware error"));
}

#[test]
fn without_an_enrolled_fingerprint_the_method_is_unavailable() {
    let (phone, vault) = phone_with_passphrase();
    phone.no_fingerprints();
    assert!(!method(&vault).available);
    assert!(!method(&vault).enabled);
    let error = vault.enable_unlock(BIOMETRIC, "correct horse").unwrap_err();
    assert_eq!(error.code(), "invalid_command");
    assert_eq!(phone.prompts(), 0);
}

#[test]
fn removing_the_passphrase_removes_the_biometric_copy() {
    let (phone, vault) = phone_with_passphrase();
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    vault.remove_passphrase("correct horse").unwrap();
    assert!(phone.bound.lock().unwrap().is_empty());
    assert!(!method(&vault).enabled);
}

#[test]
fn the_vault_locks_when_the_app_goes_to_the_background() {
    let (_phone, vault) = phone_with_passphrase();
    vault.unlock("correct horse").unwrap();
    // Default: immediately.
    assert_eq!(vault.background_lock_seconds(), 0);
    let left = SystemTime::now();
    assert!(vault.entered_background(left));
    assert!(!vault.is_unlocked());

    // With a grace period it survives a short trip and not a long one.
    vault.set_background_lock_seconds(60).unwrap();
    vault.unlock("correct horse").unwrap();
    assert!(!vault.entered_background(left));
    assert!(!vault.returned_to_foreground(left + Duration::from_secs(30)));
    assert!(vault.is_unlocked());
    vault.entered_background(left);
    assert!(vault.returned_to_foreground(left + Duration::from_secs(61)));
    assert!(!vault.is_unlocked());
}

#[test]
fn saved_secrets_work_on_a_phone_and_need_the_unlock_after_a_lock() {
    use easyssh_core::{Connection, ConnectionTarget};
    use easyssh_store::{SecretChange, SecretsUpdate};

    let (_phone, vault) = phone_with_passphrase();
    vault.unlock("correct horse").unwrap();
    let store = Store::open_in_memory("phone", vault.cipher()).unwrap();
    let mut host = Connection::alias("web", "x");
    host.target = ConnectionTarget::Endpoint {
        hostname: "example.test".into(),
        username: Some("alice".into()),
        port: 22,
    };
    store.save_host(&host, None).unwrap();
    let update = SecretsUpdate {
        password: SecretChange::Set(easyssh_crypto::SecretText::new("hunter2")),
        ..SecretsUpdate::default()
    };
    vault.set_host_secrets(&store, &host.id, &update).unwrap();
    vault.lock();
    // What is saved is visible (a flag), its value is not, until unlocked.
    assert!(vault.flags(&store, &host.id).unwrap().password);
    assert!(matches!(
        vault.host_secrets(&store, &host.id),
        Err(VaultError::Locked)
    ));
    vault.unlock("correct horse").unwrap();
    let secrets = vault.host_secrets(&store, &host.id).unwrap();
    assert_eq!(secrets.password.unwrap().expose(), "hunter2");
}

#[test]
fn the_first_run_choice_is_remembered() {
    let vault = mobile_vault_with(FakePhone::new(), FAST);
    let store = Store::open_in_memory("phone", vault.cipher()).unwrap();
    assert!(!setup_done(&store));
    mark_setup_done(&store).unwrap();
    assert!(setup_done(&store));
}

#[test]
fn the_status_json_has_the_shape_the_touch_ui_reads_and_no_secrets() {
    let (phone, vault) = phone_with_passphrase();
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    let status = status_json(&vault, true);
    assert_eq!(status["passphraseSet"], true);
    assert_eq!(status["unlocked"], true);
    assert_eq!(status["backgroundLockSeconds"], 0);
    assert_eq!(status["setupDone"], true);
    assert_eq!(status["keychainAvailable"], true);
    assert_eq!(status["unlockMethods"][0]["name"], BIOMETRIC);
    assert_eq!(status["unlockMethods"][0]["enabled"], true);
    assert_no_key_material(&phone, &status.to_string());
}

/// The device key and the passphrase must not turn up in anything the app
/// could print: status, error texts, `Debug` output.
fn assert_no_key_material(phone: &FakePhone, printed: &str) {
    let store = BridgeKeyStore::new(Arc::new(NoBridge));
    let _ = format!("{store:?}");
    for blob in phone.bound.lock().unwrap().values() {
        assert!(
            !printed.contains(blob.as_str()),
            "a stored copy was printed"
        );
    }
    assert!(!printed.contains("correct horse"));
}

struct NoBridge;
impl NativeBridge for NoBridge {
    fn call(&self, _: &str, _: Value) -> Result<Value, BridgeError> {
        Err(BridgeError::new(code::UNAVAILABLE, "none"))
    }
}

#[test]
fn nothing_secret_is_in_errors_or_debug_output() {
    let (phone, vault) = phone_with_passphrase();
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    let key = {
        let wrapped = BridgeKeyStore::new(phone.clone());
        let item = wrapped
            .get(
                easyssh_crypto::keystore::SERVICE,
                easyssh_crypto::keystore::WRAPPED_DEVICE_KEY_ACCOUNT,
            )
            .unwrap()
            .unwrap();
        STANDARD.encode(&*item)
    };
    let bound_copy = phone
        .bound
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();

    let mut printed = vec![format!("{vault:?}")];
    vault.lock();
    printed.push(vault.unlock("wrong").unwrap_err().to_string());
    phone.sensor(Sensor::Broken("hardware error"));
    printed.push(vault.unlock_with(BIOMETRIC).unwrap_err().to_string());
    printed.push(status_json(&vault, true).to_string());
    *phone.storage_down.lock().unwrap() = true;
    printed.push(vault.status().keychain_error.unwrap_or_default());
    printed.push(format!("{:?}", vault.status()));
    for line in printed {
        assert!(!line.contains(&key), "wrapped key printed: {line}");
        assert!(
            !line.contains(&bound_copy),
            "biometric copy printed: {line}"
        );
        assert!(
            !line.contains("correct horse"),
            "passphrase printed: {line}"
        );
    }
}

#[test]
fn the_bridge_calls_carry_a_secret_only_where_one_is_stored() {
    let (phone, vault) = phone_with_passphrase();
    vault.enable_unlock(BIOMETRIC, "correct horse").unwrap();
    vault.lock();
    vault.unlock_with(BIOMETRIC).unwrap();
    for (name, args) in phone.calls.lock().unwrap().iter() {
        let carries_value = args.get("value").is_some();
        let stores = name == command::SECURE_SET || name == command::BOUND_SEAL;
        assert_eq!(carries_value, stores, "{name} {args}");
        // The passphrase never crosses the bridge.
        assert!(!args.to_string().contains("correct horse"), "{name}");
    }
    assert!(!phone.log().contains("correct horse"));
}

#[test]
fn a_missing_secure_store_is_reported_as_unavailable_not_papered_over() {
    let phone = FakePhone::new();
    *phone.storage_down.lock().unwrap() = true;
    let vault = mobile_vault_with(phone, FAST);
    let status = vault.status();
    assert!(!status.keychain_available);
    assert!(status
        .keychain_error
        .unwrap()
        .contains("no secure hardware"));
    assert_eq!(
        vault.ensure_unlocked().unwrap_err().code(),
        "keychain_unavailable"
    );
}

#[test]
fn the_key_store_maps_answers_and_errors() {
    let phone = FakePhone::new();
    let store = BridgeKeyStore::new(phone.clone());
    assert!(store.get("svc", "a").unwrap().is_none());
    store.set("svc", "a", &[1, 2, 3, 255]).unwrap();
    assert_eq!(&**store.get("svc", "a").unwrap().unwrap(), &[1, 2, 3, 255]);
    assert!(store.get("svc", "b").unwrap().is_none());
    store.delete("svc", "a").unwrap();
    store.delete("svc", "a").unwrap();
    assert!(store.get("svc", "a").unwrap().is_none());

    phone
        .secure
        .lock()
        .unwrap()
        .insert(("svc".into(), "bad".into()), "***not base64***".into());
    assert!(matches!(
        store.get("svc", "bad"),
        Err(KeyStoreError::Backend(_))
    ));
    *phone.storage_down.lock().unwrap() = true;
    assert!(matches!(
        store.get("svc", "a"),
        Err(KeyStoreError::Unavailable(_))
    ));
}
