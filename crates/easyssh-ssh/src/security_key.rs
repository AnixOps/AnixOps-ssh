//! FIDO2 security keys (`sk-ssh-ed25519@openssh.com` and
//! `sk-ecdsa-sha2-nistp256@openssh.com`).
//!
//! An OpenSSH `sk-*` private key file holds no secret: it holds the public
//! key, the *application* (the relying party id, usually `ssh:`), a flags
//! byte and the *key handle* the authenticator gave out at creation. The
//! private key never leaves the device; to log in, the authenticator is asked
//! to sign the userauth challenge for that handle.
//!
//! The signature on the wire (PROTOCOL.u2f) is the algorithm name, the raw
//! signature, the flags byte and the signature counter the authenticator
//! reported. What the authenticator really signs is
//! `SHA256(application) || flags || counter || SHA256(message)`, which is the
//! CTAP2 authenticator data followed by the client data hash, so a stock
//! `get_assertion` with `clientDataHash = SHA256(message)` produces it.
//!
//! The device is behind the [`SecurityKey`] trait so tests (and the e2e
//! suite) use a software authenticator. The real one, [`HidSecurityKey`],
//! talks CTAP2 over USB HID.

use crate::error::{Error, Result};
use crate::interaction::Interaction;
use async_trait::async_trait;
use russh::keys::ssh_key::private::KeypairData;
use russh::keys::ssh_key::{Algorithm, PrivateKey, Signature};
use russh::keys::HashAlg;
use russh::{SendError, Signer};
use std::sync::Arc;
use zeroize::Zeroizing;

/// The signature asserts that the user was present (touched the key).
pub const FLAG_USER_PRESENCE: u8 = 0x01;
/// The signature asserts user verification (PIN or biometrics).
pub const FLAG_USER_VERIFICATION: u8 = 0x04;

/// How often the PIN is asked for when the device refuses it.
const PIN_ATTEMPTS: usize = 3;

/// The key types of `sk-*` keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkAlgorithm {
    Ed25519,
    EcdsaP256,
}

impl SkAlgorithm {
    pub fn name(self) -> &'static str {
        match self {
            SkAlgorithm::Ed25519 => "sk-ssh-ed25519@openssh.com",
            SkAlgorithm::EcdsaP256 => "sk-ecdsa-sha2-nistp256@openssh.com",
        }
    }
}

/// Everything an `sk-*` key file holds, besides the public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkKeyInfo {
    pub algorithm: SkAlgorithm,
    /// The relying party id the credential was made for, e.g. `ssh:`.
    pub application: String,
    /// The credential id the authenticator gave out.
    pub key_handle: Vec<u8>,
    /// Flags from the key file (user presence required, verify required,
    /// resident).
    pub flags: u8,
}

impl SkKeyInfo {
    /// `None` when `key` is not an `sk-*` key.
    pub fn of(key: &PrivateKey) -> Option<Self> {
        match key.key_data() {
            KeypairData::SkEd25519(sk) => Some(Self {
                algorithm: SkAlgorithm::Ed25519,
                application: sk.public().application().to_owned(),
                key_handle: sk.key_handle().to_vec(),
                flags: sk.flags(),
            }),
            KeypairData::SkEcdsaSha2NistP256(sk) => Some(Self {
                algorithm: SkAlgorithm::EcdsaP256,
                application: sk.public().application().to_owned(),
                key_handle: sk.key_handle().to_vec(),
                flags: sk.flags(),
            }),
            _ => None,
        }
    }

    /// The device must be touched for this key.
    pub fn needs_touch(&self) -> bool {
        self.flags & FLAG_USER_PRESENCE != 0
    }

    /// The key was made with `verify-required`: a PIN is needed.
    pub fn needs_pin(&self) -> bool {
        self.flags & FLAG_USER_VERIFICATION != 0
    }
}

/// What the authenticator is asked to sign.
#[derive(Debug, Clone, Copy)]
pub struct SkSignRequest<'a> {
    pub info: &'a SkKeyInfo,
    /// The data to sign. The authenticator hashes it with SHA-256 to get the
    /// client data hash.
    pub message: &'a [u8],
}

/// What the authenticator answered, in CTAP2 form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkAssertion {
    /// Ed25519: the 64 raw bytes. ECDSA: the ASN.1 DER signature.
    pub signature: Vec<u8>,
    /// The flags byte of the authenticator data.
    pub flags: u8,
    /// The signature counter of the authenticator data.
    pub counter: u32,
}

/// Why an authenticator did not sign.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkError {
    /// No security key is connected.
    NoDevice,
    /// Connected keys do not know this key handle (another key, or a key
    /// that was reset).
    NoCredential,
    /// The device wants its PIN.
    PinRequired,
    /// The PIN was wrong; `retries` attempts are left when the device says.
    PinInvalid { retries: Option<u32> },
    /// The PIN is blocked; the device has to be reset or unblocked.
    PinBlocked,
    /// The user did not touch the key in time, or declined.
    NotTouched,
    /// Anything else the device or the transport reported.
    Device(String),
}

impl std::fmt::Display for SkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkError::NoDevice => f.write_str("no security key is connected"),
            SkError::NoCredential => {
                f.write_str("the connected security key does not hold this key")
            }
            SkError::PinRequired => f.write_str("the security key needs its PIN"),
            SkError::PinInvalid { retries: Some(n) } => {
                write!(f, "wrong security key PIN ({n} tries left)")
            }
            SkError::PinInvalid { retries: None } => f.write_str("wrong security key PIN"),
            SkError::PinBlocked => f.write_str("the security key PIN is blocked"),
            SkError::NotTouched => f.write_str("the security key was not touched in time"),
            SkError::Device(detail) => write!(f, "security key error: {detail}"),
        }
    }
}

impl std::error::Error for SkError {}

/// An authenticator that can sign for `sk-*` keys.
#[async_trait]
pub trait SecurityKey: Send + Sync {
    /// Signs `request.message` for the credential in `request.info`. The call
    /// may wait for the user to touch the device. `pin` is only given when
    /// the device asked for it (`SkError::PinRequired`) or the key demands
    /// user verification.
    async fn sign(
        &self,
        request: &SkSignRequest<'_>,
        pin: Option<&str>,
    ) -> std::result::Result<SkAssertion, SkError>;
}

/// The authenticator backend of this build: real devices over USB HID where
/// the platform has one, `None` otherwise.
pub fn default_security_key() -> Option<Arc<dyn SecurityKey>> {
    #[cfg(all(
        feature = "hardware-key",
        any(target_os = "linux", target_os = "macos")
    ))]
    {
        Some(Arc::new(HidSecurityKey))
    }
    #[cfg(not(all(
        feature = "hardware-key",
        any(target_os = "linux", target_os = "macos")
    )))]
    {
        None
    }
}

/// Whether [`default_security_key`] returns a backend. When it does not,
/// hosts with `sk-*` keys run on OpenSSH.
pub const fn hardware_backend_available() -> bool {
    cfg!(all(
        feature = "hardware-key",
        any(target_os = "linux", target_os = "macos")
    ))
}

#[cfg(all(
    feature = "hardware-key",
    any(target_os = "linux", target_os = "macos")
))]
pub use crate::security_key_hid::HidSecurityKey;

/// Without a hardware backend there is no `HidSecurityKey`; this keeps the
/// public name resolvable for documentation only.
#[cfg(not(all(
    feature = "hardware-key",
    any(target_os = "linux", target_os = "macos")
)))]
#[derive(Debug, Clone, Copy, Default)]
pub struct HidSecurityKey;

#[cfg(not(all(
    feature = "hardware-key",
    any(target_os = "linux", target_os = "macos")
)))]
#[async_trait]
impl SecurityKey for HidSecurityKey {
    async fn sign(
        &self,
        _: &SkSignRequest<'_>,
        _: Option<&str>,
    ) -> std::result::Result<SkAssertion, SkError> {
        Err(SkError::NoDevice)
    }
}

/// Reads the DER `SEQUENCE { INTEGER r, INTEGER s }` of a CTAP2 ES256
/// signature and returns it as the two SSH `mpint`s the sk-ecdsa signature
/// holds.
fn ecdsa_der_to_ssh(der: &[u8]) -> Option<Vec<u8>> {
    fn length(bytes: &[u8]) -> Option<(usize, &[u8])> {
        let (&first, rest) = bytes.split_first()?;
        if first < 0x80 {
            return Some((first as usize, rest));
        }
        let count = (first & 0x7f) as usize;
        if count == 0 || count > 2 || rest.len() < count {
            return None;
        }
        let value = rest[..count]
            .iter()
            .fold(0usize, |acc, byte| (acc << 8) | *byte as usize);
        Some((value, &rest[count..]))
    }
    fn integer(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
        let (&tag, rest) = bytes.split_first()?;
        if tag != 0x02 {
            return None;
        }
        let (len, rest) = length(rest)?;
        if len == 0 || rest.len() < len {
            return None;
        }
        let (value, tail) = rest.split_at(len);
        // A DER integer is minimal and positive, which is exactly an SSH mpint.
        if value[0] & 0x80 != 0 || (value.len() > 1 && value[0] == 0 && value[1] & 0x80 == 0) {
            return None;
        }
        Some((value, tail))
    }
    let (&tag, rest) = der.split_first()?;
    if tag != 0x30 {
        return None;
    }
    let (len, rest) = length(rest)?;
    if rest.len() != len {
        return None;
    }
    let (r, rest) = integer(rest)?;
    let (s, rest) = integer(rest)?;
    if !rest.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(r.len() + s.len() + 8);
    for part in [r, s] {
        out.extend_from_slice(&(part.len() as u32).to_be_bytes());
        out.extend_from_slice(part);
    }
    Some(out)
}

/// The SSH signature of an `sk-*` key (algorithm, signature, flags, counter),
/// in the form that follows the length in a userauth request.
pub fn encode_signature(info: &SkKeyInfo, assertion: &SkAssertion) -> Result<Vec<u8>> {
    let (algorithm, mut data) = match info.algorithm {
        SkAlgorithm::Ed25519 => {
            if assertion.signature.len() != 64 {
                return Err(Error::Protocol(
                    "the security key returned a malformed Ed25519 signature".into(),
                ));
            }
            (Algorithm::SkEd25519, assertion.signature.clone())
        }
        SkAlgorithm::EcdsaP256 => {
            let data = ecdsa_der_to_ssh(&assertion.signature).ok_or_else(|| {
                Error::Protocol("the security key returned a malformed ECDSA signature".into())
            })?;
            (Algorithm::SkEcdsaSha2NistP256, data)
        }
    };
    // ssh-key keeps flags and counter at the end of the signature data.
    data.push(assertion.flags);
    data.extend_from_slice(&assertion.counter.to_be_bytes());
    let signature = Signature::new(algorithm, data)
        .map_err(|e| Error::Protocol(format!("malformed security key signature: {e}")))?;
    russh::keys::ssh_encoding::Encode::encode_vec(&signature)
        .map_err(|e| Error::Protocol(format!("cannot encode the security key signature: {e}")))
}

/// Signs `message` with the key `key` (an `sk-*` key) on `backend`,
/// prompting through `interaction` for the touch and the PIN. Returns the
/// encoded SSH signature.
pub async fn sign_with_security_key(
    backend: &dyn SecurityKey,
    interaction: &dyn Interaction,
    label: &str,
    info: &SkKeyInfo,
    message: &[u8],
) -> Result<Vec<u8>> {
    let request = SkSignRequest { info, message };
    let mut pin: Option<Zeroizing<String>> = None;
    let mut retries: Option<u32> = None;
    if info.needs_pin() {
        pin = Some(ask_pin(interaction, label, None).await?);
    }
    for attempt in 0..=PIN_ATTEMPTS {
        if info.needs_touch() {
            interaction.security_key_touch(label).await;
        }
        let result = backend
            .sign(&request, pin.as_ref().map(|p| p.as_str()))
            .await;
        if info.needs_touch() {
            interaction.security_key_touch_done(label).await;
        }
        let last = attempt == PIN_ATTEMPTS;
        match result {
            Ok(assertion) => return encode_signature(info, &assertion),
            Err(SkError::PinRequired) if !last => {
                pin = Some(ask_pin(interaction, label, None).await?);
            }
            Err(SkError::PinInvalid { retries: left }) if !last => {
                retries = left;
                pin = Some(ask_pin(interaction, label, retries).await?);
            }
            Err(other) => return Err(Error::Protocol(other.to_string())),
        }
    }
    Err(Error::Protocol(SkError::PinInvalid { retries }.to_string()))
}

async fn ask_pin(
    interaction: &dyn Interaction,
    label: &str,
    retries: Option<u32>,
) -> Result<Zeroizing<String>> {
    match interaction.security_key_pin(label, retries).await {
        Some(pin) if !pin.is_empty() => Ok(Zeroizing::new(pin)),
        _ => Err(Error::Cancelled(format!("no security key PIN for {label}"))),
    }
}

/// Why a signature for the userauth request could not be made.
#[derive(Debug)]
pub(crate) enum SkSignError {
    /// The connection is gone.
    Send(SendError),
    /// The key could not sign (cancelled prompt, wrong PIN, no device).
    Failed(Error),
}

impl From<SendError> for SkSignError {
    fn from(error: SendError) -> Self {
        SkSignError::Send(error)
    }
}

/// Plugs a security key into russh's public key authentication: russh asks
/// for the signature once the server accepted the key.
pub(crate) struct SkSigner<'a> {
    pub backend: Arc<dyn SecurityKey>,
    pub interaction: &'a dyn Interaction,
    pub label: &'a str,
    pub info: SkKeyInfo,
}

impl Signer for SkSigner<'_> {
    type Error = SkSignError;

    #[allow(clippy::manual_async_fn)]
    fn auth_sign(
        &mut self,
        _key: &russh::keys::agent::AgentIdentity,
        _hash_alg: Option<HashAlg>,
        mut to_sign: Vec<u8>,
    ) -> impl std::future::Future<Output = std::result::Result<Vec<u8>, Self::Error>> + Send {
        async move {
            let signature = sign_with_security_key(
                self.backend.as_ref(),
                self.interaction,
                self.label,
                &self.info,
                &to_sign,
            )
            .await
            .map_err(SkSignError::Failed)?;
            to_sign.extend_from_slice(&(signature.len() as u32).to_be_bytes());
            to_sign.extend_from_slice(&signature);
            Ok(to_sign)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_signatures_become_ssh_mpints() {
        // r = 0x01, s = 0x80 (needs a leading zero in DER and in an mpint).
        let der = [0x30, 0x07, 0x02, 0x01, 0x01, 0x02, 0x02, 0x00, 0x80];
        assert_eq!(
            ecdsa_der_to_ssh(&der).unwrap(),
            [0, 0, 0, 1, 1, 0, 0, 0, 2, 0, 0x80]
        );
    }

    #[test]
    fn malformed_der_is_refused() {
        assert!(ecdsa_der_to_ssh(&[]).is_none());
        assert!(ecdsa_der_to_ssh(&[0x30, 0x00]).is_none());
        // Wrong total length.
        assert!(ecdsa_der_to_ssh(&[0x30, 0x08, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01]).is_none());
        // Negative integer.
        assert!(ecdsa_der_to_ssh(&[0x30, 0x06, 0x02, 0x01, 0x81, 0x02, 0x01, 0x01]).is_none());
        // Trailing bytes.
        assert!(
            ecdsa_der_to_ssh(&[0x30, 0x07, 0x02, 0x01, 0x01, 0x02, 0x01, 0x01, 0x00]).is_none()
        );
    }

    #[test]
    fn flags_say_what_the_user_must_do() {
        let info = |flags| SkKeyInfo {
            algorithm: SkAlgorithm::Ed25519,
            application: "ssh:".into(),
            key_handle: vec![1],
            flags,
        };
        assert!(info(0x01).needs_touch() && !info(0x01).needs_pin());
        assert!(info(0x05).needs_touch() && info(0x05).needs_pin());
        assert!(!info(0x00).needs_touch());
    }
}
