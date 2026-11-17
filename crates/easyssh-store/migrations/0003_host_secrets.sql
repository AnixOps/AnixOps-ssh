-- Saved host secrets: a password, a private key and a key passphrase per host,
-- kept apart from `hosts` so that listing hosts never needs the vault key.
--
-- Every *_enc column holds a FieldCipher blob (device key, XChaCha20-Poly1305)
-- whose associated data names the table, vault, host id and field, so a blob
-- cannot be moved to another host or column. This table is local to the
-- device: it carries no sync metadata and is never put on the wire.
CREATE TABLE host_secrets (
    host_id         TEXT PRIMARY KEY REFERENCES hosts(id) ON DELETE CASCADE,
    updated_at      TEXT NOT NULL,
    password_enc    BLOB,
    private_key_enc BLOB,
    passphrase_enc  BLOB
);
