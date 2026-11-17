//! Field-level encryption for secrets.
//!
//! The store never writes a password, private key, passphrase or secret note
//! to disk unless a [`FieldCipher`] sealed it first. The real cipher comes
//! from the crypto lane; until then [`NotConfigured`] refuses every secret.

use std::fmt::Debug;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CipherError {
    #[error("no field cipher is configured; refusing to store or read a secret")]
    NotConfigured,
    #[error("field cipher failed: {0}")]
    Failed(String),
}

/// Encrypts one field value. `aad` binds the ciphertext to its table, vault,
/// record and field so it cannot be moved to another record.
pub trait FieldCipher: Send + Sync + Debug {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CipherError>;
    fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CipherError>;
}

/// Refuses to handle any secret. This is the default, so nothing sensitive can
/// be stored in plaintext by accident.
#[derive(Debug, Default, Clone, Copy)]
pub struct NotConfigured;

impl FieldCipher for NotConfigured {
    fn encrypt(&self, _plaintext: &[u8], _aad: &[u8]) -> Result<Vec<u8>, CipherError> {
        Err(CipherError::NotConfigured)
    }
    fn decrypt(&self, _ciphertext: &[u8], _aad: &[u8]) -> Result<Vec<u8>, CipherError> {
        Err(CipherError::NotConfigured)
    }
}

/// Not encryption at all: it only frames the value with its AAD so tests can
/// check binding. Compiled for this crate's own tests only.
#[cfg(test)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PlaintextForTests;

#[cfg(test)]
impl FieldCipher for PlaintextForTests {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CipherError> {
        let mut out = b"test-cipher:".to_vec();
        out.extend_from_slice(&(aad.len() as u32).to_be_bytes());
        out.extend_from_slice(aad);
        out.extend_from_slice(plaintext);
        Ok(out)
    }
    fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CipherError> {
        let short = || CipherError::Failed("short frame".into());
        let body = ciphertext
            .strip_prefix(b"test-cipher:")
            .ok_or_else(|| CipherError::Failed("bad frame".into()))?;
        let (len, rest) = body.split_at_checked(4).ok_or_else(short)?;
        let len = u32::from_be_bytes(len.try_into().unwrap()) as usize;
        let (stored, plaintext) = rest.split_at_checked(len).ok_or_else(short)?;
        if stored != aad {
            return Err(CipherError::Failed("associated data mismatch".into()));
        }
        Ok(plaintext.to_vec())
    }
}
