//! Saved host secrets: password, private key, key passphrase and outbound
//! proxy password, one row per
//! host in `host_secrets`, every value sealed by the store's [`FieldCipher`].
//!
//! Reading or writing a value needs the cipher to be unlocked; the plain
//! `*_set` flags do not (they only look at which columns are non-null), so
//! the UI can show "a password is saved" while the vault is locked. Plaintext
//! only ever lives in [`SecretText`], which is zeroized on drop.

use crate::error::{Result, StoreError};
use crate::meta::fmt_ts;
use crate::store::Store;
use chrono::Utc;
use easyssh_crypto::SecretText;
use rusqlite::{params, OptionalExtension};
use zeroize::Zeroizing;

const TABLE: &str = "host_secrets";

/// Which secrets a host has stored. Never contains a value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SecretFlags {
    pub password: bool,
    pub private_key: bool,
    pub passphrase: bool,
    /// The password of the host's outbound proxy.
    pub proxy_password: bool,
}

/// The encrypted columns of one `host_secrets` row.
type SecretBlobs = (
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
);

impl SecretFlags {
    pub fn any(&self) -> bool {
        self.password || self.private_key || self.passphrase || self.proxy_password
    }
}

/// The decrypted secrets of one host. `Debug` shows nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StoredSecrets {
    pub password: Option<SecretText>,
    pub private_key: Option<SecretText>,
    pub passphrase: Option<SecretText>,
    pub proxy_password: Option<SecretText>,
}

/// What to do with one stored secret when saving.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum SecretChange {
    /// Leave the stored value alone.
    #[default]
    Keep,
    Set(SecretText),
    Clear,
}

/// A partial update of a host's secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SecretsUpdate {
    pub password: SecretChange,
    pub private_key: SecretChange,
    pub passphrase: SecretChange,
    pub proxy_password: SecretChange,
}

impl SecretsUpdate {
    pub fn is_empty(&self) -> bool {
        self.password == SecretChange::Keep
            && self.private_key == SecretChange::Keep
            && self.passphrase == SecretChange::Keep
            && self.proxy_password == SecretChange::Keep
    }
}

impl Store {
    /// The vault id of a live or deleted host, or `NotFound`.
    fn host_vault(&self, host_id: &str) -> Result<String> {
        self.conn
            .query_row(
                "SELECT vault_id FROM hosts WHERE id = ?1",
                [host_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| StoreError::NotFound {
                entity: "host",
                id: host_id.to_owned(),
            })
    }

    /// Which secrets `host_id` has stored. Needs no key.
    pub fn host_secret_flags(&self, host_id: &str) -> Result<SecretFlags> {
        Ok(self
            .conn
            .query_row(
                "SELECT password_enc IS NOT NULL, private_key_enc IS NOT NULL,
                        passphrase_enc IS NOT NULL, proxy_password_enc IS NOT NULL
                 FROM host_secrets WHERE host_id = ?1",
                [host_id],
                |row| {
                    Ok(SecretFlags {
                        password: row.get(0)?,
                        private_key: row.get(1)?,
                        passphrase: row.get(2)?,
                        proxy_password: row.get(3)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Ids of hosts that have at least one stored secret.
    pub fn hosts_with_secrets(&self) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT host_id FROM host_secrets
             WHERE password_enc IS NOT NULL OR private_key_enc IS NOT NULL
                OR passphrase_enc IS NOT NULL OR proxy_password_enc IS NOT NULL
             ORDER BY host_id",
        )?;
        let ids = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        Ok(ids)
    }

    /// Decrypts the stored secrets of `host_id`. Fails with the cipher's
    /// error (for example `Locked`) when the vault cannot be opened; a host
    /// without secrets gives an empty value.
    pub fn host_secrets(&self, host_id: &str) -> Result<StoredSecrets> {
        let row = self
            .conn
            .query_row(
                "SELECT h.vault_id, s.password_enc, s.private_key_enc, s.passphrase_enc,
                        s.proxy_password_enc
                 FROM host_secrets s JOIN hosts h ON h.id = s.host_id
                 WHERE s.host_id = ?1 AND h.deleted = 0",
                [host_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, Option<Vec<u8>>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((vault_id, password, private_key, passphrase, proxy_password)) = row else {
            return Ok(StoredSecrets::default());
        };
        let open = |field: &str, blob: Option<Vec<u8>>| -> Result<Option<SecretText>> {
            blob.map(|blob| self.open_secret(&vault_id, host_id, field, &blob))
                .transpose()
        };
        Ok(StoredSecrets {
            password: open("password", password)?,
            private_key: open("private_key", private_key)?,
            passphrase: open("passphrase", passphrase)?,
            proxy_password: open("proxy_password", proxy_password)?,
        })
    }

    fn open_secret(
        &self,
        vault_id: &str,
        host_id: &str,
        field: &str,
        blob: &[u8],
    ) -> Result<SecretText> {
        let aad = Self::aad(TABLE, vault_id, host_id, field);
        let plain = Zeroizing::new(self.cipher.decrypt(blob, &aad)?);
        // `String::from_utf8` moves the buffer, so no stray copy is left.
        let text = String::from_utf8(plain.to_vec())
            .map_err(|_| StoreError::Invalid(format!("secret field {field} is not UTF-8")))?;
        Ok(SecretText::from(text))
    }

    /// Applies `update` to the secrets of `host_id` in one transaction. A row
    /// left with no secret is removed. Nothing is written when the cipher
    /// refuses (a locked vault).
    pub fn set_host_secrets(&self, host_id: &str, update: &SecretsUpdate) -> Result<()> {
        if update.is_empty() {
            return Ok(());
        }
        self.in_tx(|| {
            let vault_id = self.host_vault(host_id)?;
            let existing: SecretBlobs = self
                .conn
                .query_row(
                    "SELECT password_enc, private_key_enc, passphrase_enc, proxy_password_enc
                     FROM host_secrets WHERE host_id = ?1",
                    [host_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?
                .unwrap_or((None, None, None, None));
            let merge = |field: &str,
                         change: &SecretChange,
                         old: Option<Vec<u8>>|
             -> Result<Option<Vec<u8>>> {
                Ok(match change {
                    SecretChange::Keep => old,
                    SecretChange::Clear => None,
                    SecretChange::Set(value) if value.is_empty() => None,
                    SecretChange::Set(value) => Some(self.cipher.encrypt(
                        value.expose().as_bytes(),
                        &Self::aad(TABLE, &vault_id, host_id, field),
                    )?),
                })
            };
            let password = merge("password", &update.password, existing.0)?;
            let private_key = merge("private_key", &update.private_key, existing.1)?;
            let passphrase = merge("passphrase", &update.passphrase, existing.2)?;
            let proxy_password = merge("proxy_password", &update.proxy_password, existing.3)?;
            if password.is_none()
                && private_key.is_none()
                && passphrase.is_none()
                && proxy_password.is_none()
            {
                self.conn
                    .execute("DELETE FROM host_secrets WHERE host_id = ?1", [host_id])?;
            } else {
                self.conn.execute(
                    "INSERT INTO host_secrets
                        (host_id, updated_at, password_enc, private_key_enc, passphrase_enc,
                         proxy_password_enc)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(host_id) DO UPDATE SET
                        updated_at = excluded.updated_at,
                        password_enc = excluded.password_enc,
                        private_key_enc = excluded.private_key_enc,
                        passphrase_enc = excluded.passphrase_enc,
                        proxy_password_enc = excluded.proxy_password_enc",
                    params![
                        host_id,
                        fmt_ts(Utc::now()),
                        password,
                        private_key,
                        passphrase,
                        proxy_password
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Removes every stored secret of one host. Returns whether there was any.
    pub fn clear_host_secrets(&self, host_id: &str) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM host_secrets WHERE host_id = ?1", [host_id])?
            > 0)
    }

    /// Removes every stored secret of every host, including values the older
    /// `hosts.*_enc` columns and their encrypted change history may hold, and
    /// returns how many hosts had secrets. Works while the vault is locked:
    /// forgetting never needs the key.
    pub fn forget_all_secrets(&self) -> Result<usize> {
        self.in_tx(|| {
            let hosts = self.conn.execute("DELETE FROM host_secrets", [])?;
            self.conn.execute(
                "UPDATE hosts SET password_enc = NULL, private_key_enc = NULL,
                                  passphrase_enc = NULL
                 WHERE password_enc IS NOT NULL OR private_key_enc IS NOT NULL
                    OR passphrase_enc IS NOT NULL",
                [],
            )?;
            self.conn.execute(
                "DELETE FROM record_history
                 WHERE encrypted = 1 AND field IN ('password', 'private_key', 'passphrase')",
                [],
            )?;
            Ok(hosts)
        })
    }
}
