//! Device signing keys, device certificates and signed sync envelopes.
//!
//! See the crate README for the exact canonical byte encodings. In short:
//!
//! ```text
//! cert message     = "easyssh/v1/device-cert" || device_pub(32) || issued_at(i64 BE) || lp(device_name)
//! envelope message = "easyssh/v1/envelope" || lp(record_id) || lp(vault_id)
//!                    || version_vector_canonical || deleted(1) || lp(ciphertext) || signer(32)
//! ```

use crate::keystore::{KeyStore, KeyStoreError, DEVICE_SIGNING_ACCOUNT, SERVICE};
use crate::{fill_random, put_lp, AccountSigningKey, CryptoError, Result, VersionVector};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

pub const CERT_CONTEXT: &[u8] = b"easyssh/v1/device-cert";
pub const ENVELOPE_CONTEXT: &[u8] = b"easyssh/v1/envelope";
pub const CERT_VERSION: u8 = 1;

/// Ed25519 key a device signs sync envelopes with.
pub struct DeviceSigningKey(SigningKey);

impl DeviceSigningKey {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }

    pub fn generate() -> Result<Self> {
        let mut seed = zeroize::Zeroizing::new([0u8; 32]);
        fill_random(&mut *seed)?;
        Ok(Self::from_seed(*seed))
    }

    /// Public key bytes; this is also the `signer` identity in envelopes.
    pub fn public(&self) -> [u8; 32] {
        self.0.verifying_key().to_bytes()
    }

    /// Load the seed from `store` or create and save a new key.
    pub fn load_or_create(store: &dyn KeyStore) -> std::result::Result<Self, KeyStoreError> {
        if let Some(seed) = store.get(SERVICE, DEVICE_SIGNING_ACCOUNT)? {
            let arr: [u8; 32] = seed
                .as_slice()
                .try_into()
                .map_err(|_| CryptoError::Malformed("stored signing seed length"))?;
            return Ok(Self::from_seed(arr));
        }
        let key = Self::generate()?;
        store.set(SERVICE, DEVICE_SIGNING_ACCOUNT, key.0.as_bytes())?;
        Ok(key)
    }
}

impl core::fmt::Debug for DeviceSigningKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("DeviceSigningKey([REDACTED])")
    }
}

/// Statement by the account key that a device key may sign for the account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCert {
    pub device_public: [u8; 32],
    pub device_name: String,
    /// Unix seconds.
    pub issued_at: i64,
    pub signature: [u8; 64],
}

impl DeviceCert {
    fn message(device_public: &[u8; 32], device_name: &str, issued_at: i64) -> Vec<u8> {
        let mut m = Vec::with_capacity(CERT_CONTEXT.len() + 32 + 8 + 4 + device_name.len());
        m.extend_from_slice(CERT_CONTEXT);
        m.extend_from_slice(device_public);
        m.extend_from_slice(&issued_at.to_be_bytes());
        put_lp(&mut m, device_name.as_bytes());
        m
    }

    /// Check the certificate against the account's signing public key.
    pub fn verify(&self, account_public: &VerifyingKey) -> Result<()> {
        let msg = Self::message(&self.device_public, &self.device_name, self.issued_at);
        account_public
            .verify_strict(&msg, &Signature::from_bytes(&self.signature))
            .map_err(|_| CryptoError::BadCertificate)
    }

    /// `0x01 || device_pub(32) || issued_at(i64 BE) || lp(device_name) || signature(64)`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![CERT_VERSION];
        out.extend_from_slice(&self.device_public);
        out.extend_from_slice(&self.issued_at.to_be_bytes());
        put_lp(&mut out, self.device_name.as_bytes());
        out.extend_from_slice(&self.signature);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let bad = CryptoError::Malformed("device certificate");
        if bytes.first() != Some(&CERT_VERSION) {
            return Err(bytes
                .first()
                .map_or(bad, |v| CryptoError::UnsupportedVersion(*v)));
        }
        let rest = &bytes[1..];
        if rest.len() < 32 + 8 + 4 + 64 {
            return Err(CryptoError::Malformed("device certificate"));
        }
        let device_public: [u8; 32] = rest[..32].try_into().expect("32");
        let issued_at = i64::from_be_bytes(rest[32..40].try_into().expect("8"));
        let n = u32::from_be_bytes(rest[40..44].try_into().expect("4")) as usize;
        if rest.len() != 44 + n + 64 {
            return Err(CryptoError::Malformed("device certificate"));
        }
        let device_name = std::str::from_utf8(&rest[44..44 + n])
            .map_err(|_| CryptoError::Malformed("device certificate"))?
            .to_owned();
        let signature: [u8; 64] = rest[44 + n..].try_into().expect("64");
        Ok(Self {
            device_public,
            device_name,
            issued_at,
            signature,
        })
    }
}

/// Certify `device_public` for the account that owns `account_signing_key`.
pub fn certify_device(
    account_signing_key: &AccountSigningKey,
    device_public: &[u8; 32],
    device_name: &str,
    issued_at: i64,
) -> DeviceCert {
    let msg = DeviceCert::message(device_public, device_name, issued_at);
    DeviceCert {
        device_public: *device_public,
        device_name: device_name.to_owned(),
        issued_at,
        signature: account_signing_key.0.sign(&msg).to_bytes(),
    }
}

/// Everything in an envelope except the signer and signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeFields {
    pub record_id: String,
    pub vault_id: String,
    pub version_vector: VersionVector,
    /// `true` makes this a tombstone (the ciphertext may then be empty).
    pub deleted: bool,
    pub ciphertext: Vec<u8>,
}

/// Plain routing metadata + ciphertext, signed by a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub record_id: String,
    pub vault_id: String,
    pub version_vector: VersionVector,
    pub deleted: bool,
    pub ciphertext: Vec<u8>,
    /// Public key of the signing device.
    pub signer: [u8; 32],
    pub signature: [u8; 64],
}

impl Envelope {
    /// The exact bytes that are signed.
    pub fn signing_bytes(&self) -> Vec<u8> {
        signing_bytes(
            &self.record_id,
            &self.vault_id,
            &self.version_vector,
            self.deleted,
            &self.ciphertext,
            &self.signer,
        )
    }
}

fn signing_bytes(
    record_id: &str,
    vault_id: &str,
    vv: &VersionVector,
    deleted: bool,
    ciphertext: &[u8],
    signer: &[u8; 32],
) -> Vec<u8> {
    let mut m = Vec::with_capacity(ENVELOPE_CONTEXT.len() + 128 + ciphertext.len());
    m.extend_from_slice(ENVELOPE_CONTEXT);
    put_lp(&mut m, record_id.as_bytes());
    put_lp(&mut m, vault_id.as_bytes());
    m.extend_from_slice(&vv.to_canonical_bytes());
    m.push(u8::from(deleted));
    put_lp(&mut m, ciphertext);
    m.extend_from_slice(signer);
    m
}

pub fn sign_envelope(fields: EnvelopeFields, device_key: &DeviceSigningKey) -> Envelope {
    let signer = device_key.public();
    let msg = signing_bytes(
        &fields.record_id,
        &fields.vault_id,
        &fields.version_vector,
        fields.deleted,
        &fields.ciphertext,
        &signer,
    );
    Envelope {
        record_id: fields.record_id,
        vault_id: fields.vault_id,
        version_vector: fields.version_vector,
        deleted: fields.deleted,
        ciphertext: fields.ciphertext,
        signer,
        signature: device_key.0.sign(&msg).to_bytes(),
    }
}

/// Accept `envelope` only if `cert` is signed by `account_public`, the cert
/// belongs to the envelope's signer, and the signature covers every field.
///
/// Rollback protection (comparing the version vector with ones already
/// seen) is stateful and left to the caller.
pub fn verify_envelope(
    envelope: &Envelope,
    cert: &DeviceCert,
    account_public: &VerifyingKey,
) -> Result<()> {
    cert.verify(account_public)?;
    if cert.device_public != envelope.signer {
        return Err(CryptoError::SignerMismatch);
    }
    let device =
        VerifyingKey::from_bytes(&envelope.signer).map_err(|_| CryptoError::BadSignature)?;
    device
        .verify_strict(
            &envelope.signing_bytes(),
            &Signature::from_bytes(&envelope.signature),
        )
        .map_err(|_| CryptoError::BadSignature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountKeyPair, MemoryKeyStore};

    struct Setup {
        account: AccountKeyPair,
        device: DeviceSigningKey,
        cert: DeviceCert,
        env: Envelope,
    }

    fn setup() -> Setup {
        let account = AccountKeyPair::from_seeds([1; 32], [11; 32]);
        let device = DeviceSigningKey::from_seed([2; 32]);
        let cert = certify_device(&account.signing, &device.public(), "laptop", 1_700_000_000);
        let env = sign_envelope(
            EnvelopeFields {
                record_id: "rec".into(),
                vault_id: "vault".into(),
                version_vector: VersionVector::from_entries([("dev1", 3), ("dev2", 1)]),
                deleted: false,
                ciphertext: vec![1, 2, 3, 4],
            },
            &device,
        );
        Setup {
            account,
            device,
            cert,
            env,
        }
    }

    #[test]
    fn valid_envelope_verifies() {
        let s = setup();
        verify_envelope(&s.env, &s.cert, &s.account.signing.public()).unwrap();
        assert_eq!(s.env.signer, s.device.public());
    }

    #[test]
    fn any_field_change_fails() {
        let s = setup();
        let pk = s.account.signing.public();
        let mut cases: Vec<Envelope> = Vec::new();
        let mut e = s.env.clone();
        e.record_id.push('x');
        cases.push(e);
        let mut e = s.env.clone();
        e.vault_id = "other".into();
        cases.push(e);
        let mut e = s.env.clone();
        e.version_vector.increment("dev1");
        cases.push(e);
        let mut e = s.env.clone();
        e.version_vector = VersionVector::new();
        cases.push(e);
        let mut e = s.env.clone();
        e.deleted = true;
        cases.push(e);
        let mut e = s.env.clone();
        e.ciphertext[0] ^= 1;
        cases.push(e);
        let mut e = s.env.clone();
        e.ciphertext.push(0);
        cases.push(e);
        let mut e = s.env.clone();
        e.signature[0] ^= 1;
        cases.push(e);
        for (i, e) in cases.iter().enumerate() {
            assert!(verify_envelope(e, &s.cert, &pk).is_err(), "case {i}");
        }
        // changed signer: different device, same cert
        let other = DeviceSigningKey::from_seed([3; 32]);
        let mut e = s.env.clone();
        e.signer = other.public();
        assert_eq!(
            verify_envelope(&e, &s.cert, &pk),
            Err(CryptoError::SignerMismatch)
        );
    }

    #[test]
    fn uncertified_device_fails() {
        let s = setup();
        let rogue = DeviceSigningKey::from_seed([9; 32]);
        let env = sign_envelope(
            EnvelopeFields {
                record_id: "rec".into(),
                vault_id: "vault".into(),
                version_vector: VersionVector::new(),
                deleted: false,
                ciphertext: vec![],
            },
            &rogue,
        );
        // using the legit device's cert for a rogue signer
        assert_eq!(
            verify_envelope(&env, &s.cert, &s.account.signing.public()),
            Err(CryptoError::SignerMismatch)
        );
        // rogue self-made cert signed by a different key
        let fake_acct = AccountSigningKey::from_seed([7; 32]);
        let fake_cert = certify_device(&fake_acct, &rogue.public(), "rogue", 1);
        assert_eq!(
            verify_envelope(&env, &fake_cert, &s.account.signing.public()),
            Err(CryptoError::BadCertificate)
        );
    }

    #[test]
    fn cert_from_another_account_fails() {
        let s = setup();
        let other = AccountKeyPair::from_seeds([5; 32], [15; 32]);
        let cert = certify_device(&other.signing, &s.device.public(), "laptop", 1_700_000_000);
        assert_eq!(
            verify_envelope(&s.env, &cert, &s.account.signing.public()),
            Err(CryptoError::BadCertificate)
        );
    }

    #[test]
    fn cert_fields_are_signed() {
        let s = setup();
        let pk = s.account.signing.public();
        let mut c = s.cert.clone();
        c.device_name = "desktop".into();
        assert!(c.verify(&pk).is_err());
        let mut c = s.cert.clone();
        c.issued_at += 1;
        assert!(c.verify(&pk).is_err());
        let mut c = s.cert.clone();
        c.device_public[0] ^= 1;
        assert!(c.verify(&pk).is_err());
    }

    #[test]
    fn cert_bytes_round_trip_and_reject_garbage() {
        let s = setup();
        let b = s.cert.to_bytes();
        assert_eq!(DeviceCert::from_bytes(&b).unwrap(), s.cert);
        for len in 0..b.len() {
            assert!(DeviceCert::from_bytes(&b[..len]).is_err());
        }
        let mut t = b.clone();
        t.push(0);
        assert!(DeviceCert::from_bytes(&t).is_err());
        let mut t = b;
        t[0] = 2;
        assert_eq!(
            DeviceCert::from_bytes(&t),
            Err(CryptoError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn tombstone_signs_and_verifies() {
        let s = setup();
        let env = sign_envelope(
            EnvelopeFields {
                record_id: "rec".into(),
                vault_id: "vault".into(),
                version_vector: VersionVector::from_entries([("dev1", 4)]),
                deleted: true,
                ciphertext: vec![],
            },
            &s.device,
        );
        verify_envelope(&env, &s.cert, &s.account.signing.public()).unwrap();
        let mut undeleted = env.clone();
        undeleted.deleted = false;
        assert!(verify_envelope(&undeleted, &s.cert, &s.account.signing.public()).is_err());
    }

    #[test]
    fn device_signing_key_persists_in_store() {
        let store = MemoryKeyStore::new();
        let a = DeviceSigningKey::load_or_create(&store).unwrap();
        let b = DeviceSigningKey::load_or_create(&store).unwrap();
        assert_eq!(a.public(), b.public());
        assert!(!format!("{a:?}").contains(&format!("{:?}", a.public())));
    }
}
