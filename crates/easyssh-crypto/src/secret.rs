//! Fixed-size secret key types: zeroized on drop, constant-time equality,
//! and a `Debug` that never prints key material.

use crate::{random_array, CryptoError, Result};
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

macro_rules! secret32 {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Zeroize, ZeroizeOnDrop)]
        pub struct $name([u8; 32]);

        impl $name {
            pub const LEN: usize = 32;

            pub fn from_bytes(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub fn from_slice(bytes: &[u8]) -> Result<Self> {
                let arr: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| CryptoError::Malformed("key must be 32 bytes"))?;
                Ok(Self(arr))
            }

            /// Fresh random key from the OS CSPRNG.
            pub fn generate() -> Result<Self> {
                Ok(Self(random_array()?))
            }

            /// Borrow the raw bytes. Callers must not copy them into
            /// long-lived, non-zeroized storage.
            pub fn expose(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl PartialEq for $name {
            fn eq(&self, other: &Self) -> bool {
                self.0.ct_eq(&other.0).into()
            }
        }
        impl Eq for $name {}

        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, concat!(stringify!($name), "([REDACTED])"))
            }
        }
    };
}

secret32! {
    /// Random per-device key; protects local database fields. Lives in the OS keychain.
    DeviceKey
}
secret32! {
    /// Random key protecting every record of one vault.
    VaultKey
}
secret32! {
    /// Passphrase-derived root of the account's key pairs. Never stored or sent.
    AccountKey
}
secret32! {
    /// Passphrase-derived secret for server sign-in. Independent of [`AccountKey`].
    AuthSecret
}

/// A variable-length secret (a password, a private key, a passphrase):
/// zeroized on drop, never printed by `Debug`, never serialized.
#[derive(Clone, Default, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct SecretText(String);

impl SecretText {
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// Borrow the text. Do not copy it into long-lived, non-zeroized storage.
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<String> for SecretText {
    fn from(text: String) -> Self {
        Self(text)
    }
}

impl core::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SecretText([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_text_is_redacted() {
        let s = SecretText::new("hunter2");
        assert_eq!(format!("{s:?}"), "SecretText([REDACTED])");
        assert_eq!(s.expose(), "hunter2");
    }

    #[test]
    fn debug_is_redacted() {
        let k = DeviceKey::from_bytes([0xAB; 32]);
        let s = format!("{k:?}");
        assert_eq!(s, "DeviceKey([REDACTED])");
        assert!(!s.to_lowercase().contains("ab"));
    }

    #[test]
    fn equality() {
        assert_eq!(VaultKey::from_bytes([1; 32]), VaultKey::from_bytes([1; 32]));
        assert_ne!(VaultKey::from_bytes([1; 32]), VaultKey::from_bytes([2; 32]));
    }

    #[test]
    fn from_slice_checks_length() {
        assert!(VaultKey::from_slice(&[0; 31]).is_err());
        assert!(VaultKey::from_slice(&[0; 32]).is_ok());
    }

    #[test]
    fn generate_differs() {
        assert_ne!(
            DeviceKey::generate().unwrap(),
            DeviceKey::generate().unwrap()
        );
    }
}
