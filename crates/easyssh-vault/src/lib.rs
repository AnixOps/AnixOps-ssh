//! The encrypted local vault for saved passwords and private keys.
//!
//! * [`Vault`]: where the device key comes from (OS keychain, optionally
//!   wrapped by a passphrase), lock and unlock, auto-lock.
//! * [`prompt`]: the strict rules for which OpenSSH prompts the vault may
//!   answer.
//! * [`SecretSource`] and [`PromptAnswers`]: how the SSH engines and the
//!   askpass bridge read saved secrets.
//! * [`KeyFile`]: a saved key as a `0600` temporary file for OpenSSH.
//!
//! The ciphertext lives in `easyssh-store` (`host_secrets`); the cipher is
//! `easyssh_crypto::DeviceKeyCipher`. See `docs/architecture-v2.md` section 6.

#![forbid(unsafe_code)]

pub mod gated;
pub mod input;
pub mod keyfile;
pub mod prompt;
pub mod source;
mod vault;

pub use gated::{GatedKeyProvider, PresenceGate};
pub use input::{secrets_update, InputError, MAX_SECRET_BYTES};
pub use keyfile::{key_dir, sweep, KeyFile};
pub use prompt::{match_prompt, Binding, SecretKind};
pub use source::{Answer, PromptAnswers, SecretSource, StoreAccess, VaultSecrets};
pub use vault::{
    UnlockMethod, UnlockProvider, Vault, VaultError, VaultStatus, AUTO_LOCK_SETTING,
    DEFAULT_AUTO_LOCK_MINUTES, MAX_AUTO_LOCK_MINUTES, PROVIDER_COOLDOWN,
};

#[cfg(test)]
mod tests;
