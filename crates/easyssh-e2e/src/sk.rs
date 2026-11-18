//! Software `sk-*` keys for the e2e suite.
//!
//! sshd checks `sk-*` signatures without any hardware, so a key made by the
//! software authenticator logs in like a real one. The container authorizes
//! two fixed public keys (`e2e/id_e2e_sk.pub`, copied into the image); the
//! matching key files are rebuilt from the seeds below, so no private key
//! file is ever committed. The seeds are public and protect nothing.

use easyssh_ssh::security_key_software::{SoftwareKeyFile, SoftwareSecurityKey};
use easyssh_ssh::{SkAlgorithm, FLAG_USER_PRESENCE};
use std::path::{Path, PathBuf};

const ED25519_SEED: [u8; 32] = *b"easyssh-e2e-sk-ed25519-seed-0001";
const ECDSA_SEED: [u8; 32] = *b"easyssh-e2e-sk-ecdsa-p256-seed-1";

/// The authorized `sk-*` public keys, one per line.
pub fn authorized_file() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e/id_e2e_sk.pub")
}

/// A fresh software authenticator that holds both e2e credentials, and the
/// key files for them (Ed25519 first, then ECDSA).
pub fn e2e_security_keys() -> (SoftwareSecurityKey, SoftwareKeyFile, SoftwareKeyFile) {
    let device = SoftwareSecurityKey::new();
    let ed25519 = device.create_from_seed(
        SkAlgorithm::Ed25519,
        "ssh:",
        FLAG_USER_PRESENCE,
        ED25519_SEED,
    );
    let ecdsa = device.create_from_seed(
        SkAlgorithm::EcdsaP256,
        "ssh:",
        FLAG_USER_PRESENCE,
        ECDSA_SEED,
    );
    (device, ed25519, ecdsa)
}

/// Like [`e2e_security_keys`] for the ECDSA credential only, on a device that
/// signs the way the Windows WebAuthn API does (`webauthn-sk-ecdsa-...`).
pub fn e2e_webauthn_ecdsa_key() -> (SoftwareSecurityKey, SoftwareKeyFile) {
    let device = SoftwareSecurityKey::new().with_webauthn();
    let ecdsa = device.create_from_seed(
        SkAlgorithm::EcdsaP256,
        "ssh:",
        FLAG_USER_PRESENCE,
        ECDSA_SEED,
    );
    (device, ecdsa)
}
