//! The hook for sharing a log through a vault (sync lane).
//!
//! A log's key is derived from the device key, so another device cannot
//! derive it. To share a log, the key is wrapped under a vault key (which
//! members of that vault hold) and uploaded next to the log's id. Only the
//! wrapping is here; uploading, the vault choice and any UI are not built.
//!
//! TODO(sync lane): upload `(log id, wrapped key, the .esl file)` through the
//! server, and open a shared log with [`unwrap_log_key`] instead of the
//! device-key derivation. The reader already takes the key as a closure
//! ([`crate::KeyFn`]), so no format change is needed.

use crate::format::{Header, LogKey};
use easyssh_crypto::{decrypt_record, encrypt_record, Aad, CryptoError, VaultKey};
use zeroize::Zeroizing;

/// The key of one log, as the owning device derives it. The same salt and
/// device key always give the same key, so it can be wrapped after the fact.
pub type DeriveKey<'a> = &'a dyn Fn(&[u8]) -> Result<LogKey, String>;

fn aad<'a>(log_id: &'a str, vault_id: &'a str) -> Aad<'a> {
    Aad {
        record_id: log_id,
        vault_id,
        schema_version: 1,
        version_vector_digest: [0; 32],
    }
}

/// Wraps the key of the log with `header` under `vault_key`. The blob is
/// bound to the log id and the vault id.
pub fn wrap_log_key(
    header: &Header,
    vault_id: &str,
    vault_key: &VaultKey,
    derive: DeriveKey<'_>,
) -> Result<Vec<u8>, String> {
    let key = derive(&header.salt)?;
    encrypt_record(vault_key, &key[..], &aad(&header.id_hex(), vault_id))
        .map_err(|e: CryptoError| e.to_string())
}

/// Inverse of [`wrap_log_key`].
pub fn unwrap_log_key(
    header: &Header,
    vault_id: &str,
    vault_key: &VaultKey,
    wrapped: &[u8],
) -> Result<LogKey, String> {
    let plain = Zeroizing::new(
        decrypt_record(vault_key, wrapped, &aad(&header.id_hex(), vault_id))
            .map_err(|e| e.to_string())?,
    );
    let key: [u8; 32] = plain[..]
        .try_into()
        .map_err(|_| "the wrapped log key has the wrong length".to_owned())?;
    Ok(Zeroizing::new(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{decrypt, LogWriter};

    fn derive(salt: &[u8]) -> Result<LogKey, String> {
        let mut k = [9u8; 32];
        for (i, b) in salt.iter().enumerate() {
            k[i % 32] ^= *b;
        }
        Ok(Zeroizing::new(k))
    }

    #[test]
    fn a_wrapped_key_opens_the_log_on_another_device() {
        let mut w = LogWriter::new(Vec::new(), 1, &derive).unwrap();
        w.write(b"shared").unwrap();
        let header = w.header().clone();
        let bytes = w.finish(2).unwrap();
        let vault_key = VaultKey::from_bytes([4; 32]);
        let wrapped = wrap_log_key(&header, "vault-1", &vault_key, &derive).unwrap();
        let key = unwrap_log_key(&header, "vault-1", &vault_key, &wrapped).unwrap();
        let other_device = move |_: &[u8]| -> Result<LogKey, String> { Ok(key.clone()) };
        assert_eq!(decrypt(&bytes, &other_device).unwrap().data, b"shared");
        // Bound to the vault and the log.
        assert!(unwrap_log_key(&header, "vault-2", &vault_key, &wrapped).is_err());
        let other_log = Header::new(1);
        assert!(unwrap_log_key(&other_log, "vault-1", &vault_key, &wrapped).is_err());
    }
}
