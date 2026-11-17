//! Passphrase key derivation: Argon2id, then HKDF-SHA256 into independent
//! subkeys.
//!
//! ```text
//! passphrase --Argon2id(params, salt)--> master (32 B)
//!   master --HKDF "easyssh/v1/account-key"--> AccountKey
//!   master --HKDF "easyssh/v1/auth"---------> AuthSecret
//! ```

use crate::{fill_random, AccountKey, AuthSecret, CryptoError, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroizing;

pub const ACCOUNT_KEY_INFO: &[u8] = b"easyssh/v1/account-key";
pub const AUTH_INFO: &[u8] = b"easyssh/v1/auth";

/// Current KDF parameter-set version (Argon2id v1.3, 32-byte output).
pub const KDF_VERSION: u8 = 1;
pub const SALT_LEN: usize = 16;
/// Serialized [`KdfHeader`] length.
pub const HEADER_LEN: usize = 1 + 4 + 4 + 4 + SALT_LEN;

// Upper bounds protect clients from a hostile server sending absurd parameters.
const MAX_M_KIB: u32 = 1024 * 1024; // 1 GiB
const MAX_T: u32 = 16;
const MAX_P: u32 = 16;

/// Explicit Argon2id parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    /// Memory in KiB.
    pub m_cost_kib: u32,
    /// Iterations.
    pub t_cost: u32,
    /// Lanes.
    pub p_cost: u32,
}

impl KdfParams {
    /// Desktop default: 64 MiB, 3 passes, 1 lane.
    pub const DESKTOP_V1: KdfParams = KdfParams {
        m_cost_kib: 64 * 1024,
        t_cost: 3,
        p_cost: 1,
    };

    /// Lowest parameters this crate will accept when parsing (m >= 8 KiB * p, t >= 1).
    /// Only meant for tests; real accounts must use [`KdfParams::DESKTOP_V1`] or stronger.
    pub const fn insecure_for_tests() -> Self {
        KdfParams {
            m_cost_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.p_cost == 0 || self.p_cost > MAX_P {
            return Err(CryptoError::InvalidKdfParams("lanes out of range"));
        }
        if self.t_cost == 0 || self.t_cost > MAX_T {
            return Err(CryptoError::InvalidKdfParams("iterations out of range"));
        }
        if self.m_cost_kib < 8 * self.p_cost || self.m_cost_kib > MAX_M_KIB {
            return Err(CryptoError::InvalidKdfParams("memory out of range"));
        }
        Ok(())
    }
}

/// Parameters and salt, stored (and later synced) next to the account.
/// Public data.
///
/// Wire format (29 bytes): `version(1)=0x01 || m_kib(u32 BE) || t(u32 BE) || p(u32 BE) || salt(16)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KdfHeader {
    pub params: KdfParams,
    pub salt: [u8; SALT_LEN],
}

impl KdfHeader {
    /// New header with a random salt.
    pub fn generate(params: KdfParams) -> Result<Self> {
        params.validate()?;
        let mut salt = [0u8; SALT_LEN];
        fill_random(&mut salt)?;
        Ok(Self { params, salt })
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[0] = KDF_VERSION;
        out[1..5].copy_from_slice(&self.params.m_cost_kib.to_be_bytes());
        out[5..9].copy_from_slice(&self.params.t_cost.to_be_bytes());
        out[9..13].copy_from_slice(&self.params.p_cost.to_be_bytes());
        out[13..].copy_from_slice(&self.salt);
        out
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != HEADER_LEN {
            return Err(CryptoError::Malformed("kdf header length"));
        }
        if bytes[0] != KDF_VERSION {
            return Err(CryptoError::UnsupportedVersion(bytes[0]));
        }
        let u = |i: usize| u32::from_be_bytes(bytes[i..i + 4].try_into().expect("4 bytes"));
        let params = KdfParams {
            m_cost_kib: u(1),
            t_cost: u(5),
            p_cost: u(9),
        };
        params.validate()?;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&bytes[13..]);
        Ok(Self { params, salt })
    }
}

/// The two independent secrets derived from one passphrase.
pub struct AccountSecrets {
    /// Root of the account key pairs. Never leaves the device.
    pub account_key: AccountKey,
    /// Presented to the server at sign-in (phase 5). Never equals or reveals `account_key`.
    pub auth_secret: AuthSecret,
}

/// Run Argon2id over `passphrase` and split the result with HKDF.
pub fn derive_account_secrets(passphrase: &[u8], header: &KdfHeader) -> Result<AccountSecrets> {
    header.params.validate()?;
    let p = &header.params;
    let params = Params::new(p.m_cost_kib, p.t_cost, p.p_cost, Some(32))
        .map_err(|_| CryptoError::InvalidKdfParams("rejected by argon2"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut master = Zeroizing::new([0u8; 32]);
    argon
        .hash_password_into(passphrase, &header.salt, &mut *master)
        .map_err(|_| CryptoError::Kdf)?;

    let hk = Hkdf::<Sha256>::new(None, &*master);
    let mut account = [0u8; 32];
    let mut auth = [0u8; 32];
    hk.expand(ACCOUNT_KEY_INFO, &mut account)
        .map_err(|_| CryptoError::Kdf)?;
    hk.expand(AUTH_INFO, &mut auth)
        .map_err(|_| CryptoError::Kdf)?;
    Ok(AccountSecrets {
        account_key: AccountKey::from_bytes(account),
        auth_secret: AuthSecret::from_bytes(auth),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> KdfHeader {
        KdfHeader {
            params: KdfParams::insecure_for_tests(),
            salt: [7; 16],
        }
    }

    #[test]
    fn header_round_trip() {
        let h = KdfHeader::generate(KdfParams::DESKTOP_V1).unwrap();
        assert_eq!(KdfHeader::from_bytes(&h.to_bytes()).unwrap(), h);
        assert_eq!(h.to_bytes()[0], 1);
    }

    #[test]
    fn header_rejects_bad_input() {
        let good = header().to_bytes();
        assert!(KdfHeader::from_bytes(&good[..HEADER_LEN - 1]).is_err());
        let mut v = good;
        v[0] = 2;
        assert_eq!(
            KdfHeader::from_bytes(&v),
            Err(CryptoError::UnsupportedVersion(2))
        );
        let mut v = good;
        v[1..5].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(KdfHeader::from_bytes(&v).is_err());
        let mut v = good;
        v[5..9].copy_from_slice(&0u32.to_be_bytes());
        assert!(KdfHeader::from_bytes(&v).is_err());
        let mut v = good;
        v[9..13].copy_from_slice(&0u32.to_be_bytes());
        assert!(KdfHeader::from_bytes(&v).is_err());
    }

    #[test]
    fn deterministic_and_separated() {
        let a = derive_account_secrets(b"correct horse", &header()).unwrap();
        let b = derive_account_secrets(b"correct horse", &header()).unwrap();
        assert_eq!(a.account_key, b.account_key);
        assert_eq!(a.auth_secret, b.auth_secret);
        assert_ne!(a.account_key.expose(), a.auth_secret.expose());
    }

    #[test]
    fn passphrase_and_salt_matter() {
        let a = derive_account_secrets(b"correct horse", &header()).unwrap();
        let c = derive_account_secrets(b"correct horsf", &header()).unwrap();
        assert_ne!(a.account_key, c.account_key);
        let mut h = header();
        h.salt[0] ^= 1;
        let d = derive_account_secrets(b"correct horse", &h).unwrap();
        assert_ne!(a.account_key, d.account_key);
        let mut h = header();
        h.params.t_cost = 2;
        let e = derive_account_secrets(b"correct horse", &h).unwrap();
        assert_ne!(a.account_key, e.account_key);
    }

    #[test]
    fn invalid_params_rejected_before_work() {
        let h = KdfHeader {
            params: KdfParams {
                m_cost_kib: u32::MAX,
                t_cost: 1,
                p_cost: 1,
            },
            salt: [0; 16],
        };
        assert!(derive_account_secrets(b"x", &h).is_err());
    }
}
