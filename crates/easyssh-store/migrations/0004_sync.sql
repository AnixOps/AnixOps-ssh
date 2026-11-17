-- Sync with easyssh-server (architecture-v2 section 8).
--
-- Everything here is local bookkeeping. Plaintext records never leave the
-- device unencrypted; the account keys and the session token live in
-- sync_account.secrets_enc, sealed with the device-key FieldCipher.

-- The signed-in account, at most one row.
CREATE TABLE sync_account (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    server_url  TEXT NOT NULL,
    username    TEXT NOT NULL,
    secrets_enc BLOB NOT NULL,
    created_at  TEXT NOT NULL
);

-- The version vector of the copy the server is known to hold, per record
-- (the compare-and-set token). vault_id is the server's vault id. A record
-- without a row has never been synced.
CREATE TABLE sync_records (
    vault_id       TEXT NOT NULL,
    record_id      TEXT NOT NULL,
    server_vector  TEXT NOT NULL,
    PRIMARY KEY (vault_id, record_id)
);

-- Highest server sequence pulled, per vault.
CREATE TABLE sync_cursors (
    vault_id TEXT PRIMARY KEY,
    cursor   INTEGER NOT NULL
);

-- Rollback protection: the join of every version vector accepted or pushed.
CREATE TABLE sync_high_water (
    vault_id  TEXT NOT NULL,
    record_id TEXT NOT NULL,
    vector    TEXT NOT NULL,
    PRIMARY KEY (vault_id, record_id)
);

-- Account fingerprints a person confirmed (or that were seen first). A
-- member whose keys later differ is not trusted.
CREATE TABLE sync_pins (
    account_id  TEXT PRIMARY KEY,
    username    TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    confirmed   INTEGER NOT NULL DEFAULT 0 CHECK (confirmed IN (0, 1)),
    pinned_at   TEXT NOT NULL
);

-- Team vaults carry their own sync metadata (the name is a record inside
-- the vault) and the role of the signed-in account.
ALTER TABLE vaults ADD COLUMN role TEXT;
ALTER TABLE vaults ADD COLUMN version_vector TEXT NOT NULL DEFAULT '{}';
ALTER TABLE vaults ADD COLUMN field_stamps TEXT NOT NULL DEFAULT '{}';
