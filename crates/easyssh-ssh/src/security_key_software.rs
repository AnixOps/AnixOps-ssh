//! A software authenticator that makes real `sk-*` signatures, for tests and
//! the e2e suite. It behaves like a security key (a key handle names a
//! credential, a counter goes up with every signature, a PIN can be required)
//! but keeps the credentials in memory. The signatures verify with `ssh-key`
//! and with sshd, which checks them without any hardware.
//!
//! Only compiled for tests and with the `software-key` feature; nothing in
//! the application uses it.

use crate::security_key::{
    SecurityKey, SkAlgorithm, SkAssertion, SkError, SkKeyInfo, SkSignRequest, FLAG_USER_PRESENCE,
    FLAG_USER_VERIFICATION,
};
use async_trait::async_trait;
use rand::RngCore;
use russh::keys::ssh_key::private::{KeypairData, SkEcdsaSha2NistP256, SkEd25519};
use russh::keys::ssh_key::{LineEnding, PrivateKey, PublicKey};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

enum Secret {
    Ed25519(ed25519_dalek::SigningKey),
    P256(p256::ecdsa::SigningKey),
}

struct Credential {
    handle: Vec<u8>,
    application: String,
    secret: Secret,
}

/// A key file made by [`SoftwareSecurityKey`], in the forms tests need.
pub struct SoftwareKeyFile {
    /// The `sk-*` private key (handle, no secret).
    pub private: PrivateKey,
    /// The OpenSSH private key file text.
    pub private_text: String,
    /// The `authorized_keys` line.
    pub public_line: String,
}

/// An in-memory FIDO2 authenticator.
pub struct SoftwareSecurityKey {
    credentials: Mutex<Vec<Credential>>,
    counter: AtomicU32,
    pin: Option<String>,
    pin_retries: AtomicU32,
}

impl Default for SoftwareSecurityKey {
    fn default() -> Self {
        Self::new()
    }
}

fn put(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

impl SoftwareSecurityKey {
    pub fn new() -> Self {
        Self {
            credentials: Mutex::new(Vec::new()),
            counter: AtomicU32::new(0),
            pin: None,
            pin_retries: AtomicU32::new(8),
        }
    }

    /// The device has this PIN set.
    pub fn with_pin(mut self, pin: &str) -> Self {
        self.pin = Some(pin.to_owned());
        self
    }

    /// Makes a credential with random material, like `ssh-keygen -t
    /// ed25519-sk` does.
    pub fn create(&self, algorithm: SkAlgorithm, application: &str, flags: u8) -> SoftwareKeyFile {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        self.create_from_seed(algorithm, application, flags, seed)
    }

    /// Makes the credential that `seed` always gives, so a public key can be
    /// committed (the e2e containers authorize one).
    pub fn create_from_seed(
        &self,
        algorithm: SkAlgorithm,
        application: &str,
        flags: u8,
        seed: [u8; 32],
    ) -> SoftwareKeyFile {
        let handle = Sha256::digest([b"handle".as_slice(), &seed].concat()).to_vec();
        let (secret, public_blob) = match algorithm {
            SkAlgorithm::Ed25519 => {
                let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
                let mut blob = Vec::new();
                put(&mut blob, algorithm.name().as_bytes());
                put(&mut blob, signing.verifying_key().as_bytes());
                put(&mut blob, application.as_bytes());
                (Secret::Ed25519(signing), blob)
            }
            SkAlgorithm::EcdsaP256 => {
                let signing = p256::ecdsa::SigningKey::from_slice(&seed)
                    .expect("a 32 byte seed is a valid P-256 scalar");
                let point = signing.verifying_key().to_sec1_point(false);
                let mut blob = Vec::new();
                put(&mut blob, algorithm.name().as_bytes());
                put(&mut blob, b"nistp256");
                put(&mut blob, point.as_bytes());
                put(&mut blob, application.as_bytes());
                (Secret::P256(signing), blob)
            }
        };
        let public = PublicKey::from_bytes(&public_blob).expect("a well-formed sk public key");
        let keypair = match public.key_data() {
            russh::keys::ssh_key::public::KeyData::SkEd25519(key) => KeypairData::SkEd25519(
                SkEd25519::new(key.clone(), flags, handle.clone()).expect("short handle"),
            ),
            russh::keys::ssh_key::public::KeyData::SkEcdsaSha2NistP256(key) => {
                KeypairData::SkEcdsaSha2NistP256(
                    SkEcdsaSha2NistP256::new(key.clone(), flags, handle.clone())
                        .expect("short handle"),
                )
            }
            _ => unreachable!("an sk public key"),
        };
        let private = PrivateKey::new(keypair, "software security key").expect("a valid key");
        let private_text = private
            .to_openssh(LineEnding::LF)
            .expect("OpenSSH encoding")
            .to_string();
        let public_line = private
            .public_key()
            .to_openssh()
            .expect("OpenSSH public key encoding");
        self.credentials
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(Credential {
                handle,
                application: application.to_owned(),
                secret,
            });
        SoftwareKeyFile {
            private,
            private_text,
            public_line,
        }
    }

    /// How many signatures were made.
    pub fn signatures(&self) -> u32 {
        self.counter.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SecurityKey for SoftwareSecurityKey {
    async fn sign(
        &self,
        request: &SkSignRequest<'_>,
        pin: Option<&str>,
    ) -> Result<SkAssertion, SkError> {
        let info: &SkKeyInfo = request.info;
        let mut flags = 0u8;
        if info.flags & FLAG_USER_PRESENCE != 0 {
            flags |= FLAG_USER_PRESENCE;
        }
        // A device with a PIN asks for it when the key demands verification;
        // a PIN that is given is checked either way.
        match (&self.pin, pin) {
            (Some(_), None) if info.needs_pin() => return Err(SkError::PinRequired),
            (Some(expected), Some(given)) => {
                if expected != given {
                    let left = self
                        .pin_retries
                        .fetch_sub(1, Ordering::SeqCst)
                        .saturating_sub(1);
                    return Err(if left == 0 {
                        SkError::PinBlocked
                    } else {
                        SkError::PinInvalid {
                            retries: Some(left),
                        }
                    });
                }
                flags |= FLAG_USER_VERIFICATION;
            }
            (None, Some(_)) => {
                return Err(SkError::Device("this security key has no PIN".into()));
            }
            _ => {}
        }
        let credentials = self.credentials.lock().unwrap_or_else(|p| p.into_inner());
        let credential = credentials
            .iter()
            .find(|c| c.handle == info.key_handle && c.application == info.application)
            .ok_or(SkError::NoCredential)?;
        let counter = self.counter.fetch_add(1, Ordering::SeqCst) + 1;

        let mut signed = Vec::new();
        signed.extend_from_slice(&Sha256::digest(info.application.as_bytes()));
        signed.push(flags);
        signed.extend_from_slice(&counter.to_be_bytes());
        signed.extend_from_slice(&Sha256::digest(request.message));

        let signature = match &credential.secret {
            Secret::Ed25519(key) => {
                use ed25519_dalek::Signer;
                key.sign(&signed).to_bytes().to_vec()
            }
            Secret::P256(key) => {
                use p256::ecdsa::signature::Signer;
                let signature: p256::ecdsa::Signature = key.sign(&signed);
                signature.to_der().as_bytes().to_vec()
            }
        };
        Ok(SkAssertion {
            signature,
            flags,
            counter,
        })
    }
}
