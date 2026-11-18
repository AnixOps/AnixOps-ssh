//! Wrapping a small secret under a 32-byte secret that comes from outside,
//! such as the `hmac-secret` output of a FIDO2 security key.
//!
//! Blob: `0x01 || nonce(24) || ciphertext || tag(16)` (XChaCha20-Poly1305).
//! The key is `HKDF-SHA256(secret, salt = none, info)`, so the raw output of
//! the authenticator is never used as a key directly and a different `info`
//! gives an unrelated key. `aad` binds the blob to the slot it is stored in.

use crate::record::{open_blob, seal_blob, NONCE_LEN};
use crate::{fill_random, CryptoError, Result};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

const WRAP_VERSION: u8 = 1;

fn wrapping_key(secret: &[u8], info: &[u8]) -> Result<Zeroizing<[u8; 32]>> {
    let mut key = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(None, secret)
        .expand(info, &mut *key)
        .map_err(|_| CryptoError::Kdf)?;
    Ok(key)
}

/// Encrypts `plaintext` under a key derived from `secret`.
pub fn wrap_with_secret(
    secret: &[u8],
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let key = wrapping_key(secret, info)?;
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    seal_blob(WRAP_VERSION, &key, &nonce, aad, plaintext)
}

/// Inverse of [`wrap_with_secret`]. Fails with
/// [`CryptoError::Authentication`] for a wrong secret, `info` or `aad`.
pub fn unwrap_with_secret(
    secret: &[u8],
    info: &[u8],
    aad: &[u8],
    blob: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let key = wrapping_key(secret, info)?;
    open_blob(WRAP_VERSION, &key, aad, blob).map(Zeroizing::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let blob = wrap_with_secret(&[7; 32], b"info", b"slot", b"device key").unwrap();
        assert_eq!(
            &*unwrap_with_secret(&[7; 32], b"info", b"slot", &blob).unwrap(),
            b"device key"
        );
    }

    #[test]
    fn a_wrong_secret_info_or_slot_fails() {
        let blob = wrap_with_secret(&[7; 32], b"info", b"slot", b"device key").unwrap();
        for (secret, info, aad) in [
            ([8u8; 32], &b"info"[..], &b"slot"[..]),
            ([7; 32], b"other", b"slot"),
            ([7; 32], b"info", b"other"),
        ] {
            assert_eq!(
                unwrap_with_secret(&secret, info, aad, &blob).unwrap_err(),
                CryptoError::Authentication
            );
        }
    }

    #[test]
    fn blobs_are_not_repeated() {
        let a = wrap_with_secret(&[7; 32], b"i", b"a", b"x").unwrap();
        let b = wrap_with_secret(&[7; 32], b"i", b"a", b"x").unwrap();
        assert_ne!(a, b);
    }
}
