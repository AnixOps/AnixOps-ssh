//! Account key pair and sealed vault keys.
//!
//! The account's X25519 (sealing) and Ed25519 (certifying) keys are both
//! **derived** from the passphrase-derived [`AccountKey`] with HKDF, not
//! generated. Why: the account must be recoverable on a new device from the
//! passphrase alone, with no server-held private-key blob to protect and
//! no extra state to sync. The cost is that rotating either key means
//! rotating the passphrase-derived root (new salt), which is acceptable
//! for a passphrase change.
//!
//! Sealed vault key (ECIES-style, built from standard parts):
//!
//! ```text
//! 0x01 || eph_pub(32) || nonce(24) || XChaCha20-Poly1305(vault_key) (32 + tag 16)   = 105 bytes
//!   shared = X25519(eph_secret, recipient_pub)          (rejected if all-zero)
//!   key    = HKDF-SHA256(salt = eph_pub || recipient_pub, ikm = shared,
//!                        info = "easyssh/v1/sealed-vault-key")
//!   aad    = "easyssh/v1/sealed-vault-key" || eph_pub || recipient_pub
//! ```

use crate::record::{open_blob, seal_blob, NONCE_LEN};
use crate::{fill_random, AccountKey, CryptoError, Result, VaultKey};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

pub const ACCOUNT_X25519_INFO: &[u8] = b"easyssh/v1/account-x25519";
pub const ACCOUNT_ED25519_INFO: &[u8] = b"easyssh/v1/account-ed25519";
pub const SEALED_INFO: &[u8] = b"easyssh/v1/sealed-vault-key";
pub const SEALED_VERSION: u8 = 1;
pub const SEALED_LEN: usize = 1 + 32 + NONCE_LEN + 32 + 16;

/// X25519 public key a vault key can be sealed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealPublicKey(pub [u8; 32]);

/// Private X25519 key of the account. Zeroized on drop.
pub struct AccountSealingKey(StaticSecret);

impl AccountSealingKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(StaticSecret::from(bytes))
    }
    pub fn public(&self) -> SealPublicKey {
        SealPublicKey(PublicKey::from(&self.0).to_bytes())
    }
}

impl core::fmt::Debug for AccountSealingKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AccountSealingKey([REDACTED])")
    }
}

/// Private Ed25519 key of the account; only used to certify devices.
pub struct AccountSigningKey(pub(crate) SigningKey);

impl AccountSigningKey {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self(SigningKey::from_bytes(&seed))
    }
    pub fn public(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
}

impl core::fmt::Debug for AccountSigningKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("AccountSigningKey([REDACTED])")
    }
}

/// Both account keys, derived from one [`AccountKey`].
#[derive(Debug)]
pub struct AccountKeyPair {
    pub sealing: AccountSealingKey,
    pub signing: AccountSigningKey,
}

impl AccountKeyPair {
    pub fn from_account_key(account_key: &AccountKey) -> Result<Self> {
        let hk = Hkdf::<Sha256>::new(None, account_key.expose());
        let mut x = Zeroizing::new([0u8; 32]);
        let mut e = Zeroizing::new([0u8; 32]);
        hk.expand(ACCOUNT_X25519_INFO, &mut *x)
            .map_err(|_| CryptoError::Kdf)?;
        hk.expand(ACCOUNT_ED25519_INFO, &mut *e)
            .map_err(|_| CryptoError::Kdf)?;
        Ok(Self {
            sealing: AccountSealingKey::from_bytes(*x),
            signing: AccountSigningKey::from_seed(*e),
        })
    }
}

fn seal_key_and_aad(
    shared: &[u8; 32],
    eph_pub: &[u8; 32],
    recipient: &[u8; 32],
) -> Result<(Zeroizing<[u8; 32]>, Vec<u8>)> {
    let mut salt = [0u8; 64];
    salt[..32].copy_from_slice(eph_pub);
    salt[32..].copy_from_slice(recipient);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared);
    let mut key = Zeroizing::new([0u8; 32]);
    hk.expand(SEALED_INFO, &mut *key)
        .map_err(|_| CryptoError::Kdf)?;
    let mut aad = Vec::with_capacity(SEALED_INFO.len() + 64);
    aad.extend_from_slice(SEALED_INFO);
    aad.extend_from_slice(&salt);
    Ok((key, aad))
}

/// Seal `vault_key` so only the holder of `member_public`'s private key can open it.
pub fn seal_vault_key(vault_key: &VaultKey, member_public: &SealPublicKey) -> Result<Vec<u8>> {
    let mut eph = Zeroizing::new([0u8; 32]);
    fill_random(&mut *eph)?;
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    seal_vault_key_with(vault_key, member_public, &eph, &nonce)
}

/// Deterministic variant for test vectors. Never reuse `eph_secret` or `nonce`.
#[doc(hidden)]
pub fn seal_vault_key_with(
    vault_key: &VaultKey,
    member_public: &SealPublicKey,
    eph_secret: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
) -> Result<Vec<u8>> {
    let eph = StaticSecret::from(*eph_secret);
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let shared = eph.diffie_hellman(&PublicKey::from(member_public.0));
    if !shared.was_contributory() {
        return Err(CryptoError::Malformed(
            "recipient public key is a weak point",
        ));
    }
    let (key, aad) = seal_key_and_aad(shared.as_bytes(), &eph_pub, &member_public.0)?;
    let inner = seal_blob(SEALED_VERSION, &key, nonce, &aad, vault_key.expose())?;
    let mut out = Vec::with_capacity(SEALED_LEN);
    out.push(SEALED_VERSION);
    out.extend_from_slice(&eph_pub);
    out.extend_from_slice(&inner[1..]); // nonce || ct+tag
    Ok(out)
}

/// Open a vault key sealed to `account`.
pub fn open_vault_key(sealed: &[u8], account: &AccountSealingKey) -> Result<VaultKey> {
    if sealed.len() != SEALED_LEN {
        return Err(CryptoError::Malformed("sealed key length"));
    }
    if sealed[0] != SEALED_VERSION {
        return Err(CryptoError::UnsupportedVersion(sealed[0]));
    }
    let eph_pub: [u8; 32] = sealed[1..33].try_into().expect("32");
    let shared = account.0.diffie_hellman(&PublicKey::from(eph_pub));
    if !shared.was_contributory() {
        return Err(CryptoError::SealOpen);
    }
    let recipient = account.public().0;
    let (key, aad) = seal_key_and_aad(shared.as_bytes(), &eph_pub, &recipient)?;
    let mut inner = Vec::with_capacity(sealed.len() - 32);
    inner.push(SEALED_VERSION);
    inner.extend_from_slice(&sealed[33..]);
    let pt = Zeroizing::new(
        open_blob(SEALED_VERSION, &key, &aad, &inner).map_err(|_| CryptoError::SealOpen)?,
    );
    VaultKey::from_slice(&pt).map_err(|_| CryptoError::SealOpen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(n: u8) -> AccountKeyPair {
        AccountKeyPair::from_account_key(&AccountKey::from_bytes([n; 32])).unwrap()
    }

    #[test]
    fn derivation_is_deterministic_and_separated() {
        let a = pair(1);
        let b = pair(1);
        assert_eq!(a.sealing.public(), b.sealing.public());
        assert_eq!(a.signing.public(), b.signing.public());
        assert_ne!(a.sealing.public(), pair(2).sealing.public());
        assert_ne!(a.sealing.public().0, a.signing.public().to_bytes());
    }

    #[test]
    fn seal_open_round_trip() {
        let acct = pair(1);
        let vk = VaultKey::generate().unwrap();
        let sealed = seal_vault_key(&vk, &acct.sealing.public()).unwrap();
        assert_eq!(sealed.len(), SEALED_LEN);
        assert_eq!(open_vault_key(&sealed, &acct.sealing).unwrap(), vk);
        // sealing twice yields different blobs
        assert_ne!(sealed, seal_vault_key(&vk, &acct.sealing.public()).unwrap());
    }

    #[test]
    fn wrong_recipient_fails() {
        let vk = VaultKey::generate().unwrap();
        let sealed = seal_vault_key(&vk, &pair(1).sealing.public()).unwrap();
        assert_eq!(
            open_vault_key(&sealed, &pair(2).sealing),
            Err(CryptoError::SealOpen)
        );
    }

    #[test]
    fn tampering_and_truncation_fail() {
        let acct = pair(1);
        let vk = VaultKey::generate().unwrap();
        let sealed = seal_vault_key(&vk, &acct.sealing.public()).unwrap();
        for i in 0..sealed.len() {
            let mut t = sealed.clone();
            t[i] ^= 0x01;
            assert!(open_vault_key(&t, &acct.sealing).is_err(), "byte {i}");
        }
        for len in 0..sealed.len() {
            assert!(open_vault_key(&sealed[..len], &acct.sealing).is_err());
        }
    }

    #[test]
    fn weak_recipient_rejected() {
        let vk = VaultKey::generate().unwrap();
        assert!(seal_vault_key(&vk, &SealPublicKey([0; 32])).is_err());
    }
}
