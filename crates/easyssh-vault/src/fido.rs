//! FIDO2 security keys as a way to unlock the vault.
//!
//! A key is enrolled by creating a credential with the `hmac-secret`
//! extension for this application and asking it once for the HMAC of a random
//! salt. That 32-byte answer only the key can produce, and it produces the
//! same one every time for the same credential and salt. The device key is
//! wrapped under `HKDF(answer)` and stored, with the credential id and the
//! salt, next to the other unlock methods (a keychain item, like the copy
//! Windows Hello and Touch ID keep). To unlock, the key is asked for the HMAC
//! again, which needs a touch (and the PIN when the key has one), and the
//! wrapped copy opens.
//!
//! Unlike the presence gates, the stored copy is useless without the key:
//! code running as the user can read the item but cannot unwrap it. The vault
//! passphrase stays the way in when no key is at hand, and removing the
//! passphrase removes every copy.
//!
//! The device is behind [`HmacAuthenticator`]. The real ones live in
//! `easyssh-unlock` (USB HID on Linux and macOS, the Windows WebAuthn API on
//! Windows); tests use a fake. Several keys can be enrolled (at most
//! [`MAX_KEYS`], so the item stays small enough for Windows Credential
//! Manager) and each can be removed on its own.
//!
//! The hmac-secret output depends on whether the key verified the user (PIN)
//! at the time, so a key whose PIN is set or removed after enrolment answers
//! differently; the copy then fails to open with a message that says so, and
//! the key is removed and added again.

use crate::{UnlockItem, UnlockPrompt, UnlockProvider, VaultError};
use easyssh_crypto::keystore::SERVICE;
use easyssh_crypto::{unwrap_with_secret, wrap_with_secret, DeviceKey, KeyStore};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// Name of the unlock method on the wire.
pub const METHOD: &str = "security_key";
/// Keychain account of the list of enrolled keys.
const ACCOUNT: &str = "device-key-security-keys/v1";
/// Most keys that can be enrolled.
pub const MAX_KEYS: usize = 4;
/// Longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 32;
/// Longest credential id the list accepts, in bytes.
pub const MAX_CREDENTIAL_ID: usize = 256;
/// How often the PIN is asked for when the key refuses it.
const PIN_ATTEMPTS: usize = 3;
const STORE_VERSION: u8 = 1;
const WRAP_INFO: &[u8] = b"easyssh/v1/vault-unlock/security-key";

/// Why the authenticator did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FidoError {
    /// No security key is connected.
    NoDevice,
    /// The platform or the key cannot do `hmac-secret`.
    Unsupported,
    /// The connected keys do not know this credential.
    NoCredential,
    /// The key wants its PIN.
    PinRequired,
    /// The PIN was wrong.
    PinInvalid { retries: Option<u32> },
    /// The PIN is blocked; the key has to be reset or unblocked.
    PinBlocked,
    /// The user did not touch the key in time, or declined.
    NotTouched,
    /// The user cancelled a prompt of ours.
    Cancelled,
    /// Anything else the device or the transport reported.
    Device(String),
}

impl std::fmt::Display for FidoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FidoError::NoDevice => f.write_str("no security key is connected"),
            FidoError::Unsupported => f.write_str(
                "this security key does not support hmac-secret, or this system cannot use it",
            ),
            FidoError::NoCredential => {
                f.write_str("the connected security key is not one added to the vault")
            }
            FidoError::PinRequired => f.write_str("the security key needs its PIN"),
            FidoError::PinInvalid { retries: Some(n) } => {
                write!(f, "wrong security key PIN ({n} tries left)")
            }
            FidoError::PinInvalid { retries: None } => f.write_str("wrong security key PIN"),
            FidoError::PinBlocked => f.write_str("the security key PIN is blocked"),
            FidoError::NotTouched => f.write_str("the security key was not touched in time"),
            FidoError::Cancelled => f.write_str("cancelled"),
            FidoError::Device(detail) => write!(f, "security key error: {detail}"),
        }
    }
}

/// An authenticator that can create an `hmac-secret` credential and answer
/// with the HMAC of a salt. Calls block while the user touches the key.
pub trait HmacAuthenticator: Send + Sync {
    /// This build and this system have a way to reach security keys (not
    /// whether one is plugged in).
    fn available(&self) -> bool;
    /// The backend shows its own touch and PIN dialog (Windows), so the
    /// vault asks for neither and passes no PIN.
    fn handles_user_verification(&self) -> bool {
        false
    }
    /// Creates a credential with the `hmac-secret` extension and returns its
    /// id. `Unsupported` when the key does not have the extension.
    fn make_credential(&self, pin: Option<&str>) -> Result<Vec<u8>, FidoError>;
    /// The HMAC of `salt` under the credential's secret.
    fn hmac_secret(
        &self,
        credential_id: &[u8],
        salt: &[u8; 32],
        pin: Option<&str>,
    ) -> Result<Zeroizing<[u8; 32]>, FidoError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: String,
    label: String,
    credential_id: Vec<u8>,
    salt: [u8; 32],
    wrapped: Vec<u8>,
}

impl Entry {
    fn aad(&self) -> Vec<u8> {
        let mut aad = WRAP_INFO.to_vec();
        aad.extend_from_slice(self.id.as_bytes());
        aad.extend_from_slice(&self.credential_id);
        aad
    }
}

fn put_u8_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.push(bytes.len() as u8);
    out.extend_from_slice(bytes);
}

fn put_u16_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn encode(entries: &[Entry]) -> Vec<u8> {
    let mut out = vec![STORE_VERSION, entries.len() as u8];
    for entry in entries {
        put_u8_bytes(&mut out, entry.id.as_bytes());
        put_u8_bytes(&mut out, entry.label.as_bytes());
        put_u16_bytes(&mut out, &entry.credential_id);
        out.extend_from_slice(&entry.salt);
        put_u16_bytes(&mut out, &entry.wrapped);
    }
    out
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u8_bytes(&mut self) -> Option<&'a [u8]> {
        let n = self.u8()? as usize;
        self.take(n)
    }
    fn u16_bytes(&mut self) -> Option<&'a [u8]> {
        let n = u16::from_be_bytes(self.take(2)?.try_into().ok()?) as usize;
        self.take(n)
    }
}

/// `None` for anything that is not a well-formed list of the current version.
fn decode(bytes: &[u8]) -> Option<Vec<Entry>> {
    let mut r = Reader(bytes);
    if r.u8()? != STORE_VERSION {
        return None;
    }
    let count = r.u8()? as usize;
    if count > MAX_KEYS {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let id = String::from_utf8(r.u8_bytes()?.to_vec()).ok()?;
        let label = String::from_utf8(r.u8_bytes()?.to_vec()).ok()?;
        let credential_id = r.u16_bytes()?.to_vec();
        if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID {
            return None;
        }
        let salt: [u8; 32] = r.take(32)?.try_into().ok()?;
        let wrapped = r.u16_bytes()?.to_vec();
        entries.push(Entry {
            id,
            label,
            credential_id,
            salt,
            wrapped,
        });
    }
    r.0.is_empty().then_some(entries)
}

fn clean_label(label: &str, fallback_number: usize) -> String {
    let trimmed: String = label
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_CHARS)
        .collect();
    if trimmed.is_empty() {
        format!("Security key {fallback_number}")
    } else {
        trimmed
    }
}

/// How a failed authenticator call looks to the caller of the vault.
fn vault_error(error: FidoError) -> VaultError {
    match error {
        FidoError::Cancelled | FidoError::NotTouched => VaultError::UnlockDeclined,
        other => VaultError::UnlockFailed(other.to_string()),
    }
}

/// The security keys of one vault, as an [`UnlockProvider`].
pub struct SecurityKeyProvider {
    authenticator: Arc<dyn HmacAuthenticator>,
    keystore: Arc<dyn KeyStore>,
    prompt: Mutex<Option<Arc<dyn UnlockPrompt>>>,
    /// Read-modify-write of the stored list.
    list: Mutex<()>,
}

impl SecurityKeyProvider {
    pub fn new(authenticator: Arc<dyn HmacAuthenticator>, keystore: Arc<dyn KeyStore>) -> Self {
        Self {
            authenticator,
            keystore,
            prompt: Mutex::new(None),
            list: Mutex::new(()),
        }
    }

    fn entries(&self) -> Vec<Entry> {
        match self.keystore.get(SERVICE, ACCOUNT) {
            Ok(Some(bytes)) => decode(&bytes).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    fn save(&self, entries: &[Entry]) -> Result<(), VaultError> {
        if entries.is_empty() {
            return Ok(self.keystore.delete(SERVICE, ACCOUNT)?);
        }
        Ok(self.keystore.set(SERVICE, ACCOUNT, &encode(entries))?)
    }

    fn prompt(&self) -> Option<Arc<dyn UnlockPrompt>> {
        self.prompt
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Runs one authenticator call with the touch notice up and, when the
    /// key asks, the PIN prompt.
    fn run<T>(
        &self,
        label: &str,
        call: impl Fn(Option<&str>) -> Result<T, FidoError>,
    ) -> Result<T, FidoError> {
        let prompt = self.prompt();
        let own_dialog = self.authenticator.handles_user_verification();
        let mut pin: Option<Zeroizing<String>> = None;
        let mut asked = 0;
        loop {
            let notice = prompt.as_ref().map(|p| p.touch(label));
            let result = call(pin.as_ref().map(|p| p.as_str()));
            drop(notice);
            let retries = match &result {
                Err(FidoError::PinInvalid { retries }) => *retries,
                Err(FidoError::PinRequired) => None,
                _ => return result,
            };
            if own_dialog || asked >= PIN_ATTEMPTS {
                return result;
            }
            let Some(prompt) = prompt.as_ref() else {
                // Nobody to ask (no daemon): say what is missing.
                return Err(FidoError::PinRequired);
            };
            asked += 1;
            match prompt.pin(label, retries) {
                Some(text) => pin = Some(Zeroizing::new(text)),
                None => return Err(FidoError::Cancelled),
            }
        }
    }
}

impl UnlockProvider for SecurityKeyProvider {
    fn name(&self) -> &'static str {
        METHOD
    }

    fn available(&self) -> bool {
        self.authenticator.available()
    }

    fn enrolled(&self) -> bool {
        !self.entries().is_empty()
    }

    fn enroll(&self, key: &DeviceKey) -> Result<(), VaultError> {
        self.add_item(key, "").map(|_| ())
    }

    fn unenroll(&self) -> Result<(), VaultError> {
        let _guard = self.list.lock().unwrap_or_else(|e| e.into_inner());
        Ok(self.keystore.delete(SERVICE, ACCOUNT)?)
    }

    fn items(&self) -> Vec<UnlockItem> {
        self.entries()
            .into_iter()
            .map(|e| UnlockItem {
                id: e.id,
                label: e.label,
            })
            .collect()
    }

    fn add_item(&self, key: &DeviceKey, label: &str) -> Result<UnlockItem, VaultError> {
        let _guard = self.list.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.entries();
        if entries.len() >= MAX_KEYS {
            return Err(VaultError::Input(format!(
                "at most {MAX_KEYS} security keys can be added; remove one first"
            )));
        }
        let label = clean_label(label, entries.len() + 1);
        let credential_id = self
            .run(&label, |pin| self.authenticator.make_credential(pin))
            .map_err(vault_error)?;
        if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID {
            return Err(VaultError::UnlockFailed(
                "the security key returned a credential this vault cannot store".into(),
            ));
        }
        let salt = *DeviceKey::generate()
            .map_err(|e| VaultError::Crypto(e.to_string()))?
            .expose();
        // The second touch proves the key really answers hmac-secret before
        // anything is stored.
        let secret = self
            .run(&label, |pin| {
                self.authenticator.hmac_secret(&credential_id, &salt, pin)
            })
            .map_err(vault_error)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let mut entry = Entry {
            id,
            label,
            credential_id,
            salt,
            wrapped: Vec::new(),
        };
        entry.wrapped = wrap_with_secret(&*secret, WRAP_INFO, &entry.aad(), key.expose())
            .map_err(|e| VaultError::Crypto(e.to_string()))?;
        let item = UnlockItem {
            id: entry.id.clone(),
            label: entry.label.clone(),
        };
        entries.push(entry);
        self.save(&entries)?;
        Ok(item)
    }

    fn remove_item(&self, id: &str) -> Result<(), VaultError> {
        let _guard = self.list.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = self.entries();
        entries.retain(|e| e.id != id);
        self.save(&entries)
    }

    fn try_unlock(&self) -> Result<Option<DeviceKey>, VaultError> {
        // Look first: nothing enrolled, no prompt.
        let entries = self.entries();
        if entries.is_empty() {
            return Ok(None);
        }
        let mut last = FidoError::NoCredential;
        for entry in &entries {
            let answer = self.run(&entry.label, |pin| {
                self.authenticator
                    .hmac_secret(&entry.credential_id, &entry.salt, pin)
            });
            match answer {
                Ok(secret) => {
                    return match unwrap_with_secret(
                        &*secret,
                        WRAP_INFO,
                        &entry.aad(),
                        &entry.wrapped,
                    ) {
                        Ok(plain) => Ok(DeviceKey::from_slice(&plain).ok()),
                        Err(_) => Err(VaultError::UnlockFailed(
                            "the security key answered differently than when it was added \
                             (was its PIN set or removed since?); remove it and add it again"
                                .into(),
                        )),
                    };
                }
                // Another enrolled key may be the one that is plugged in.
                Err(FidoError::NoCredential) => last = FidoError::NoCredential,
                Err(FidoError::Cancelled | FidoError::NotTouched) => return Ok(None),
                Err(other) => return Err(vault_error(other)),
            }
        }
        Err(vault_error(last))
    }

    fn set_prompt(&self, prompt: Arc<dyn UnlockPrompt>) {
        *self.prompt.lock().unwrap_or_else(|e| e.into_inner()) = Some(prompt);
    }
}

#[cfg(test)]
mod tests;
