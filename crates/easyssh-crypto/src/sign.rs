//! Device signing keys, device certificates and signed sync envelopes.
//!
//! See the crate README for the exact canonical byte encodings. In short:
//!
//! ```text
//! cert message     = "easyssh/v2/device-cert" || device_pub(32) || issued_at(i64 BE)
//!                    || expires_at(i64 BE) || lp(device_name)
//! envelope message = "easyssh/v2/envelope" || lp(record_id) || lp(vault_id) || lp(table)
//!                    || key_generation(u32 BE) || version_vector_canonical || deleted(1)
//!                    || lp(ciphertext) || signer(32)
//! ```
//!
//! Version 2 of both formats (before any release) added the certificate
//! expiry, and the table and vault key generation to the signed envelope.

use crate::keystore::{KeyStore, KeyStoreError, DEVICE_SIGNING_ACCOUNT, SERVICE};
use crate::{fill_random, put_lp, AccountSigningKey, CryptoError, Result, VersionVector};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

pub const CERT_CONTEXT: &[u8] = b"easyssh/v2/device-cert";
pub const ENVELOPE_CONTEXT: &[u8] = b"easyssh/v2/envelope";
pub const CERT_VERSION: u8 = 2;
/// How long a freshly issued device certificate is valid (one year).
pub const CERT_LIFETIME_SECS: i64 = 365 * 24 * 60 * 60;

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
    /// Unix seconds; the certificate is not valid after this instant.
    pub expires_at: i64,
    pub signature: [u8; 64],
}

impl DeviceCert {
    fn message(
        device_public: &[u8; 32],
        device_name: &str,
        issued_at: i64,
        expires_at: i64,
    ) -> Vec<u8> {
        let mut m = Vec::with_capacity(CERT_CONTEXT.len() + 32 + 16 + 4 + device_name.len());
        m.extend_from_slice(CERT_CONTEXT);
        m.extend_from_slice(device_public);
        m.extend_from_slice(&issued_at.to_be_bytes());
        m.extend_from_slice(&expires_at.to_be_bytes());
        put_lp(&mut m, device_name.as_bytes());
        m
    }

    /// Check the certificate against the account's signing public key.
    pub fn verify(&self, account_public: &VerifyingKey) -> Result<()> {
        let msg = Self::message(
            &self.device_public,
            &self.device_name,
            self.issued_at,
            self.expires_at,
        );
        account_public
            .verify_strict(&msg, &Signature::from_bytes(&self.signature))
            .map_err(|_| CryptoError::BadCertificate)
    }

    /// Is the certificate within its validity at `at` (unix seconds)?
    /// The check is `at <= expires_at`; `issued_at` is not enforced, so a
    /// device whose clock runs slow is not locked out.
    pub fn check_valid_at(&self, at: i64) -> Result<()> {
        if at > self.expires_at {
            Err(CryptoError::CertificateExpired)
        } else {
            Ok(())
        }
    }

    /// `0x02 || device_pub(32) || issued_at(i64 BE) || expires_at(i64 BE) || lp(device_name) || signature(64)`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![CERT_VERSION];
        out.extend_from_slice(&self.device_public);
        out.extend_from_slice(&self.issued_at.to_be_bytes());
        out.extend_from_slice(&self.expires_at.to_be_bytes());
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
        if rest.len() < 32 + 16 + 4 + 64 {
            return Err(CryptoError::Malformed("device certificate"));
        }
        let device_public: [u8; 32] = rest[..32].try_into().expect("32");
        let issued_at = i64::from_be_bytes(rest[32..40].try_into().expect("8"));
        let expires_at = i64::from_be_bytes(rest[40..48].try_into().expect("8"));
        let n = u32::from_be_bytes(rest[48..52].try_into().expect("4")) as usize;
        if rest.len() != 52 + n + 64 {
            return Err(CryptoError::Malformed("device certificate"));
        }
        let device_name = std::str::from_utf8(&rest[52..52 + n])
            .map_err(|_| CryptoError::Malformed("device certificate"))?
            .to_owned();
        let signature: [u8; 64] = rest[52 + n..].try_into().expect("64");
        Ok(Self {
            device_public,
            device_name,
            issued_at,
            expires_at,
            signature,
        })
    }
}

/// Certify `device_public` for the account that owns `account_signing_key`,
/// valid until `expires_at` (see [`CERT_LIFETIME_SECS`]).
pub fn certify_device(
    account_signing_key: &AccountSigningKey,
    device_public: &[u8; 32],
    device_name: &str,
    issued_at: i64,
    expires_at: i64,
) -> DeviceCert {
    let msg = DeviceCert::message(device_public, device_name, issued_at, expires_at);
    DeviceCert {
        device_public: *device_public,
        device_name: device_name.to_owned(),
        issued_at,
        expires_at,
        signature: account_signing_key.0.sign(&msg).to_bytes(),
    }
}

/// Everything in an envelope except the signer and signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeFields {
    pub record_id: String,
    pub vault_id: String,
    /// Entity table (`hosts`, `snippets`, ...); routing metadata the server
    /// may not change.
    pub table: String,
    /// Generation of the vault key the ciphertext is encrypted under. It
    /// starts at 1 and grows by one at every key rotation.
    pub key_generation: u32,
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
    pub table: String,
    pub key_generation: u32,
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
            &EnvelopeFields {
                record_id: self.record_id.clone(),
                vault_id: self.vault_id.clone(),
                table: self.table.clone(),
                key_generation: self.key_generation,
                version_vector: self.version_vector.clone(),
                deleted: self.deleted,
                ciphertext: self.ciphertext.clone(),
            },
            &self.signer,
        )
    }
}

fn signing_bytes(fields: &EnvelopeFields, signer: &[u8; 32]) -> Vec<u8> {
    let mut m = Vec::with_capacity(ENVELOPE_CONTEXT.len() + 128 + fields.ciphertext.len());
    m.extend_from_slice(ENVELOPE_CONTEXT);
    put_lp(&mut m, fields.record_id.as_bytes());
    put_lp(&mut m, fields.vault_id.as_bytes());
    put_lp(&mut m, fields.table.as_bytes());
    m.extend_from_slice(&fields.key_generation.to_be_bytes());
    m.extend_from_slice(&fields.version_vector.to_canonical_bytes());
    m.push(u8::from(fields.deleted));
    put_lp(&mut m, &fields.ciphertext);
    m.extend_from_slice(signer);
    m
}

pub fn sign_envelope(fields: EnvelopeFields, device_key: &DeviceSigningKey) -> Envelope {
    let signer = device_key.public();
    let msg = signing_bytes(&fields, &signer);
    Envelope {
        record_id: fields.record_id,
        vault_id: fields.vault_id,
        table: fields.table,
        key_generation: fields.key_generation,
        version_vector: fields.version_vector,
        deleted: fields.deleted,
        ciphertext: fields.ciphertext,
        signer,
        signature: device_key.0.sign(&msg).to_bytes(),
    }
}

/// Accept `envelope` only if `cert` is signed by `account_public`, is not
/// expired at `at` (unix seconds: the time the envelope reached the server,
/// or now when that is unknown), the cert belongs to the envelope's signer,
/// and the signature covers every field.
///
/// Rollback protection (comparing the version vector with ones already
/// seen) and device revocation are stateful and left to the caller.
pub fn verify_envelope(
    envelope: &Envelope,
    cert: &DeviceCert,
    account_public: &VerifyingKey,
    at: i64,
) -> Result<()> {
    cert.verify(account_public)?;
    cert.check_valid_at(at)?;
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

    const ISSUED: i64 = 1_700_000_000;

    struct Setup {
        account: AccountKeyPair,
        device: DeviceSigningKey,
        cert: DeviceCert,
        env: Envelope,
    }

    fn setup() -> Setup {
        let account = AccountKeyPair::from_seeds([1; 32], [11; 32]);
        let device = DeviceSigningKey::from_seed([2; 32]);
        let cert = certify_device(
            &account.signing,
            &device.public(),
            "laptop",
            ISSUED,
            ISSUED + CERT_LIFETIME_SECS,
        );
        let env = sign_envelope(
            EnvelopeFields {
                record_id: "rec".into(),
                vault_id: "vault".into(),
                table: "hosts".into(),
                key_generation: 1,
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
        verify_envelope(&s.env, &s.cert, &s.account.signing.public(), ISSUED).unwrap();
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
        e.table = "snippets".into();
        cases.push(e);
        let mut e = s.env.clone();
        e.key_generation = 2;
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
            assert!(
                verify_envelope(e, &s.cert, &pk, ISSUED).is_err(),
                "case {i}"
            );
        }
        // changed signer: different device, same cert
        let other = DeviceSigningKey::from_seed([3; 32]);
        let mut e = s.env.clone();
        e.signer = other.public();
        assert_eq!(
            verify_envelope(&e, &s.cert, &pk, ISSUED),
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
                table: "hosts".into(),
                key_generation: 1,
                version_vector: VersionVector::new(),
                deleted: false,
                ciphertext: vec![],
            },
            &rogue,
        );
        // using the legit device's cert for a rogue signer
        assert_eq!(
            verify_envelope(&env, &s.cert, &s.account.signing.public(), ISSUED),
            Err(CryptoError::SignerMismatch)
        );
        // rogue self-made cert signed by a different key
        let fake_acct = AccountSigningKey::from_seed([7; 32]);
        let fake_cert = certify_device(&fake_acct, &rogue.public(), "rogue", 1, ISSUED * 2);
        assert_eq!(
            verify_envelope(&env, &fake_cert, &s.account.signing.public(), ISSUED),
            Err(CryptoError::BadCertificate)
        );
    }

    #[test]
    fn cert_from_another_account_fails() {
        let s = setup();
        let other = AccountKeyPair::from_seeds([5; 32], [15; 32]);
        let cert = certify_device(
            &other.signing,
            &s.device.public(),
            "laptop",
            ISSUED,
            ISSUED + CERT_LIFETIME_SECS,
        );
        assert_eq!(
            verify_envelope(&s.env, &cert, &s.account.signing.public(), ISSUED),
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
        c.expires_at += 1;
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
        t[0] = 3;
        assert_eq!(
            DeviceCert::from_bytes(&t),
            Err(CryptoError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn tombstone_signs_and_verifies() {
        let s = setup();
        let env = sign_envelope(
            EnvelopeFields {
                record_id: "rec".into(),
                vault_id: "vault".into(),
                table: "hosts".into(),
                key_generation: 1,
                version_vector: VersionVector::from_entries([("dev1", 4)]),
                deleted: true,
                ciphertext: vec![],
            },
            &s.device,
        );
        verify_envelope(&env, &s.cert, &s.account.signing.public(), ISSUED).unwrap();
        let mut undeleted = env.clone();
        undeleted.deleted = false;
        assert!(verify_envelope(&undeleted, &s.cert, &s.account.signing.public(), ISSUED).is_err());
    }

    #[test]
    fn expired_certificates_are_rejected_but_the_last_second_is_valid() {
        let s = setup();
        let pk = s.account.signing.public();
        let last = s.cert.expires_at;
        verify_envelope(&s.env, &s.cert, &pk, last).unwrap();
        assert_eq!(
            verify_envelope(&s.env, &s.cert, &pk, last + 1),
            Err(CryptoError::CertificateExpired)
        );
        // An envelope received long before expiry stays valid when judged
        // at that time.
        verify_envelope(&s.env, &s.cert, &pk, ISSUED + 10).unwrap();
        // A cert that expired before it was issued never validates.
        let dead = certify_device(&s.account.signing, &s.device.public(), "d", 10, 5);
        assert_eq!(dead.check_valid_at(6), Err(CryptoError::CertificateExpired));
    }

    #[test]
    fn signed_routing_fields_are_the_whole_envelope() {
        // table and key_generation change the signing bytes.
        let s = setup();
        let mut e = s.env.clone();
        e.table.push('x');
        assert_ne!(e.signing_bytes(), s.env.signing_bytes());
        let mut e = s.env.clone();
        e.key_generation += 1;
        assert_ne!(e.signing_bytes(), s.env.signing_bytes());
        // lp() keeps record_id/vault_id/table boundaries unambiguous.
        let a = EnvelopeFields {
            record_id: "ab".into(),
            vault_id: "c".into(),
            table: "d".into(),
            key_generation: 1,
            version_vector: VersionVector::new(),
            deleted: false,
            ciphertext: vec![],
        };
        let mut b = a.clone();
        b.record_id = "a".into();
        b.vault_id = "bc".into();
        assert_ne!(
            sign_envelope(a, &s.device).signing_bytes(),
            sign_envelope(b, &s.device).signing_bytes()
        );
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
