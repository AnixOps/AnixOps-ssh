//! Whole-record encryption with XChaCha20-Poly1305.
//!
//! Blob: `0x01 || nonce(24) || ciphertext || tag(16)`.
//!
//! The associated data binds the ciphertext to its record id, vault id,
//! schema version and version-vector digest, so a server cannot move
//! ciphertext between records or vaults or replay it under another version.

use crate::{fill_random, put_lp, CryptoError, Result, VaultKey};
use chacha20poly1305::{aead::Aead, aead::Payload, KeyInit, XChaCha20Poly1305, XNonce};

pub const RECORD_VERSION: u8 = 1;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
/// Smallest valid blob: version + nonce + tag (empty plaintext).
pub const MIN_BLOB_LEN: usize = 1 + NONCE_LEN + TAG_LEN;

const AAD_CONTEXT: &[u8] = b"easyssh/v1/record";

/// Associated data of a record ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Aad<'a> {
    pub record_id: &'a str,
    pub vault_id: &'a str,
    pub schema_version: u32,
    /// [`crate::VersionVector::digest`] of the record's version vector.
    pub version_vector_digest: [u8; 32],
}

impl Aad<'_> {
    /// `"easyssh/v1/record" || lp(record_id) || lp(vault_id) || schema(u32 BE) || digest(32)`,
    /// where `lp(x)` is a u32 BE length followed by the bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(AAD_CONTEXT.len() + 64 + self.record_id.len());
        out.extend_from_slice(AAD_CONTEXT);
        put_lp(&mut out, self.record_id.as_bytes());
        put_lp(&mut out, self.vault_id.as_bytes());
        out.extend_from_slice(&self.schema_version.to_be_bytes());
        out.extend_from_slice(&self.version_vector_digest);
        out
    }
}

/// Seal `plaintext` as `version || nonce || ct+tag` with an explicit nonce.
pub(crate) fn seal_blob(
    version: u8,
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(&(*key).into());
    let ct = cipher
        .encrypt(
            &XNonce::from(*nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Authentication)?;
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    out.push(version);
    out.extend_from_slice(nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

/// Inverse of [`seal_blob`]; checks the version byte and length first.
pub(crate) fn open_blob(version: u8, key: &[u8; 32], aad: &[u8], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < MIN_BLOB_LEN {
        return Err(CryptoError::Malformed("blob too short"));
    }
    if blob[0] != version {
        return Err(CryptoError::UnsupportedVersion(blob[0]));
    }
    let nonce: [u8; NONCE_LEN] = blob[1..1 + NONCE_LEN].try_into().expect("24 bytes");
    let cipher = XChaCha20Poly1305::new(&(*key).into());
    cipher
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &blob[1 + NONCE_LEN..],
                aad,
            },
        )
        .map_err(|_| CryptoError::Authentication)
}

/// Encrypt one record with a fresh random nonce.
pub fn encrypt_record(key: &VaultKey, plaintext: &[u8], aad: &Aad<'_>) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    encrypt_record_with_nonce(key, &nonce, plaintext, aad)
}

/// Like [`encrypt_record`] with a caller-chosen nonce. **Only for test
/// vectors**: reusing a nonce under one key breaks confidentiality.
#[doc(hidden)]
pub fn encrypt_record_with_nonce(
    key: &VaultKey,
    nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &Aad<'_>,
) -> Result<Vec<u8>> {
    seal_blob(
        RECORD_VERSION,
        key.expose(),
        nonce,
        &aad.to_bytes(),
        plaintext,
    )
}

/// Decrypt a record blob. Fails on a wrong key, any AAD mismatch, any
/// modification, truncation or an unknown version byte.
pub fn decrypt_record(key: &VaultKey, blob: &[u8], aad: &Aad<'_>) -> Result<Vec<u8>> {
    open_blob(RECORD_VERSION, key.expose(), &aad.to_bytes(), blob)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aad() -> Aad<'static> {
        Aad {
            record_id: "rec-1",
            vault_id: "vault-1",
            schema_version: 1,
            version_vector_digest: [9; 32],
        }
    }

    fn key() -> VaultKey {
        VaultKey::from_bytes([3; 32])
    }

    #[test]
    fn round_trip_and_layout() {
        let blob = encrypt_record(&key(), b"hello", &aad()).unwrap();
        assert_eq!(blob[0], 1);
        assert_eq!(blob.len(), 1 + 24 + 5 + 16);
        assert_eq!(decrypt_record(&key(), &blob, &aad()).unwrap(), b"hello");
        let empty = encrypt_record(&key(), b"", &aad()).unwrap();
        assert_eq!(empty.len(), MIN_BLOB_LEN);
        assert!(decrypt_record(&key(), &empty, &aad()).unwrap().is_empty());
    }

    #[test]
    fn nonces_are_fresh() {
        let a = encrypt_record(&key(), b"x", &aad()).unwrap();
        let b = encrypt_record(&key(), b"x", &aad()).unwrap();
        assert_ne!(a[1..25], b[1..25]);
    }

    #[test]
    fn every_bit_flip_fails() {
        let blob = encrypt_record(&key(), b"top secret", &aad()).unwrap();
        for i in 0..blob.len() {
            for bit in 0..8 {
                let mut t = blob.clone();
                t[i] ^= 1 << bit;
                assert!(
                    decrypt_record(&key(), &t, &aad()).is_err(),
                    "byte {i} bit {bit} accepted"
                );
            }
        }
    }

    #[test]
    fn wrong_version_byte() {
        let mut blob = encrypt_record(&key(), b"x", &aad()).unwrap();
        blob[0] = 2;
        assert_eq!(
            decrypt_record(&key(), &blob, &aad()),
            Err(CryptoError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn truncation_fails() {
        let blob = encrypt_record(&key(), b"some data", &aad()).unwrap();
        for len in 0..blob.len() {
            assert!(decrypt_record(&key(), &blob[..len], &aad()).is_err());
        }
        let mut extended = blob.clone();
        extended.push(0);
        assert!(decrypt_record(&key(), &extended, &aad()).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let blob = encrypt_record(&key(), b"x", &aad()).unwrap();
        let other = VaultKey::from_bytes([4; 32]);
        assert!(decrypt_record(&other, &blob, &aad()).is_err());
    }

    #[test]
    fn every_aad_field_is_bound() {
        let blob = encrypt_record(&key(), b"x", &aad()).unwrap();
        let cases = [
            Aad {
                record_id: "rec-2",
                ..aad()
            },
            Aad {
                vault_id: "vault-2",
                ..aad()
            },
            Aad {
                schema_version: 2,
                ..aad()
            },
            Aad {
                version_vector_digest: [8; 32],
                ..aad()
            },
        ];
        for c in cases {
            assert_eq!(
                decrypt_record(&key(), &blob, &c),
                Err(CryptoError::Authentication)
            );
        }
    }

    #[test]
    fn aad_encoding_is_unambiguous() {
        // Moving bytes between record_id and vault_id must change the AAD.
        let a = Aad {
            record_id: "ab",
            vault_id: "c",
            ..aad()
        };
        let b = Aad {
            record_id: "a",
            vault_id: "bc",
            ..aad()
        };
        assert_ne!(a.to_bytes(), b.to_bytes());
    }
}
