//! Checking what the user typed before it reaches the vault.

use crate::VaultError;
use easyssh_crypto::SecretText;
use easyssh_store::{SecretChange, SecretsUpdate};

/// Longest password, key passphrase or key text the vault accepts.
pub const MAX_SECRET_BYTES: usize = 64 * 1024;

/// A value that was refused, with the wire error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputError {
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<InputError> for VaultError {
    fn from(error: InputError) -> Self {
        VaultError::Input(error.message)
    }
}

fn invalid(code: &'static str, message: impl Into<String>) -> InputError {
    InputError {
        code,
        message: message.into(),
    }
}

/// Builds the update for `secrets.set`: `None` keeps a value, an empty
/// string clears it, anything else replaces it. Refuses passwords that are
/// not one line and keys that are not PEM or OpenSSH private keys (a pasted
/// public key, say). The message never repeats a value.
pub fn secrets_update(
    password: Option<String>,
    private_key: Option<String>,
    passphrase: Option<String>,
) -> Result<SecretsUpdate, InputError> {
    for (name, value) in [
        ("password", &password),
        ("passphrase", &passphrase),
        ("private key", &private_key),
    ] {
        if value.as_ref().is_some_and(|v| v.len() > MAX_SECRET_BYTES) {
            return Err(invalid(
                "invalid_command",
                format!("the {name} is too long"),
            ));
        }
    }
    // A prompt answer is one line, and a NUL cannot be passed to a program.
    for (name, value) in [("password", &password), ("passphrase", &passphrase)] {
        if value
            .as_ref()
            .is_some_and(|v| v.contains(['\n', '\r', '\0']))
        {
            return Err(invalid(
                "invalid_command",
                format!("the {name} must be a single line"),
            ));
        }
    }
    if let Some(key) = private_key.as_deref().filter(|k| !k.is_empty()) {
        if !(key.contains("-----BEGIN ") && key.contains("PRIVATE KEY-----")) {
            return Err(invalid(
                "invalid_key",
                "this does not look like a PEM or OpenSSH private key",
            ));
        }
        if key.contains('\0') {
            return Err(invalid("invalid_key", "the key contains a NUL byte"));
        }
    }
    let change = |value: Option<String>| match value {
        None => SecretChange::Keep,
        Some(value) if value.is_empty() => SecretChange::Clear,
        Some(value) => SecretChange::Set(SecretText::new(value)),
    };
    Ok(SecretsUpdate {
        password: change(password),
        private_key: change(private_key),
        passphrase: change(passphrase),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_set_and_clear() {
        let update = secrets_update(Some("pw".into()), Some(String::new()), None).unwrap();
        assert_eq!(update.password, SecretChange::Set(SecretText::new("pw")));
        assert_eq!(update.private_key, SecretChange::Clear);
        assert_eq!(update.passphrase, SecretChange::Keep);
    }

    #[test]
    fn refuses_bad_input_without_echoing_it() {
        let e = secrets_update(Some("a\nb-LEAK".into()), None, None).unwrap_err();
        assert_eq!(e.code, "invalid_command");
        assert!(!e.message.contains("LEAK"));
        let e = secrets_update(None, Some("ssh-ed25519 AAAA-LEAK".into()), None).unwrap_err();
        assert_eq!(e.code, "invalid_key");
        assert!(!e.message.contains("LEAK"));
        let long = "x".repeat(MAX_SECRET_BYTES + 1);
        assert!(secrets_update(Some(long), None, None).is_err());
        let key = "-----BEGIN OPENSSH PRIVATE KEY-----\nabc\n-----END OPENSSH PRIVATE KEY-----\n";
        assert!(secrets_update(None, Some(key.into()), None).is_ok());
        let rsa = "-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----\n";
        assert!(secrets_update(None, Some(rsa.into()), None).is_ok());
    }
}
