//! Field-level encryption for secrets.
//!
//! The store never writes a password, private key, passphrase or secret note
//! to disk unless a [`FieldCipher`] sealed it first. There is one definition of
//! that trait, in `easyssh-crypto`; this module re-exports it. The default is
//! [`NotConfigured`], which refuses every secret. The app passes a
//! [`DeviceKeyCipher`] (see the `easyssh-vault` crate).

pub use easyssh_crypto::{CryptoError, DeviceKeyCipher, FieldCipher, NotConfigured};

/// Not encryption at all: it only frames the value with its AAD so tests can
/// check binding. Compiled for this crate's own tests only.
#[cfg(test)]
#[derive(Debug, Default, Clone, Copy)]
pub struct PlaintextForTests;

#[cfg(test)]
impl FieldCipher for PlaintextForTests {
    fn encrypt(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let mut out = b"test-cipher:".to_vec();
        out.extend_from_slice(&(aad.len() as u32).to_be_bytes());
        out.extend_from_slice(aad);
        out.extend_from_slice(plaintext);
        Ok(out)
    }
    fn decrypt(&self, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let bad = || CryptoError::Authentication;
        let body = ciphertext.strip_prefix(b"test-cipher:").ok_or_else(bad)?;
        let (len, rest) = body.split_at_checked(4).ok_or_else(bad)?;
        let len = u32::from_be_bytes(len.try_into().unwrap()) as usize;
        let (stored, plaintext) = rest.split_at_checked(len).ok_or_else(bad)?;
        if stored != aad {
            return Err(CryptoError::Authentication);
        }
        Ok(plaintext.to_vec())
    }
}
