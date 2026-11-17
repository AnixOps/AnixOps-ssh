-- EasySSH v2 local store, schema 1.
--
-- Every syncable table carries: id (UUID text), vault_id, updated_at,
-- version_vector (JSON device -> counter), field_stamps (JSON field ->
-- {lamport, device}) and deleted (tombstone). Timestamps are RFC 3339 UTC.
-- Columns ending in _enc hold ciphertext produced by the FieldCipher; no
-- secret is ever stored in a plain column.

CREATE TABLE vaults (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('personal', 'team')),
    created_at TEXT NOT NULL
);

-- The built-in personal vault every record belongs to until vaults ship.
INSERT INTO vaults (id, name, kind, created_at)
VALUES ('00000000-0000-0000-0000-000000000001', 'Personal', 'personal',
        strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE groups (
    id             TEXT PRIMARY KEY,
    vault_id       TEXT NOT NULL REFERENCES vaults(id),
    updated_at     TEXT NOT NULL,
    version_vector TEXT NOT NULL DEFAULT '{}',
    field_stamps   TEXT NOT NULL DEFAULT '{}',
    deleted        INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    name           TEXT NOT NULL,
    color          TEXT,
    parent_id      TEXT REFERENCES groups(id) ON DELETE SET NULL
);

CREATE TABLE hosts (
    id                   TEXT PRIMARY KEY,
    vault_id             TEXT NOT NULL REFERENCES vaults(id),
    updated_at           TEXT NOT NULL,
    version_vector       TEXT NOT NULL DEFAULT '{}',
    field_stamps         TEXT NOT NULL DEFAULT '{}',
    deleted              INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    created_at           TEXT NOT NULL,
    name                 TEXT NOT NULL,
    target_kind          TEXT NOT NULL CHECK (target_kind IN ('alias', 'endpoint')),
    alias                TEXT,
    hostname             TEXT,
    username             TEXT,
    port                 INTEGER CHECK (port IS NULL OR (port BETWEEN 1 AND 65535)),
    group_id             TEXT REFERENCES groups(id) ON DELETE SET NULL,
    favorite             INTEGER NOT NULL DEFAULT 0,
    color                TEXT,
    notes                TEXT NOT NULL DEFAULT '',
    notes_secret         INTEGER NOT NULL DEFAULT 0 CHECK (notes_secret IN (0, 1)),
    notes_enc            BLOB,
    proxy_jump           TEXT,
    local_forwards       TEXT NOT NULL DEFAULT '[]',
    remote_forwards      TEXT NOT NULL DEFAULT '[]',
    dynamic_forwards     TEXT NOT NULL DEFAULT '[]',
    remote_command       TEXT,
    local_directories    TEXT NOT NULL DEFAULT '[]',
    remote_directories   TEXT NOT NULL DEFAULT '[]',
    term_font_size       REAL NOT NULL DEFAULT 14.0,
    term_scrollback      INTEGER NOT NULL DEFAULT 10000,
    term_startup_command TEXT,
    password_enc         BLOB,
    private_key_enc      BLOB,
    passphrase_enc       BLOB
);
CREATE INDEX hosts_group ON hosts(group_id);
CREATE INDEX hosts_live ON hosts(vault_id, deleted);

CREATE TABLE tags (
    id             TEXT PRIMARY KEY,
    vault_id       TEXT NOT NULL REFERENCES vaults(id),
    updated_at     TEXT NOT NULL,
    version_vector TEXT NOT NULL DEFAULT '{}',
    field_stamps   TEXT NOT NULL DEFAULT '{}',
    deleted        INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    name           TEXT NOT NULL,
    UNIQUE (vault_id, name)
);

CREATE TABLE host_tags (
    host_id TEXT NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    tag_id  TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    PRIMARY KEY (host_id, tag_id)
);

CREATE TABLE snippets (
    id             TEXT PRIMARY KEY,
    vault_id       TEXT NOT NULL REFERENCES vaults(id),
    updated_at     TEXT NOT NULL,
    version_vector TEXT NOT NULL DEFAULT '{}',
    field_stamps   TEXT NOT NULL DEFAULT '{}',
    deleted        INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    name           TEXT NOT NULL,
    content        TEXT NOT NULL
);

-- host_id is deliberately not a foreign key: v1 tunnels may name a host that
-- no longer exists and must still migrate without loss.
CREATE TABLE tunnels (
    id             TEXT PRIMARY KEY,
    vault_id       TEXT NOT NULL REFERENCES vaults(id),
    updated_at     TEXT NOT NULL,
    version_vector TEXT NOT NULL DEFAULT '{}',
    field_stamps   TEXT NOT NULL DEFAULT '{}',
    deleted        INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    name           TEXT NOT NULL,
    host_id        TEXT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('local', 'remote', 'dynamic')),
    bind_address   TEXT,
    bind_port      INTEGER NOT NULL,
    target_host    TEXT,
    target_port    INTEGER
);
CREATE INDEX tunnels_host ON tunnels(host_id);

-- host is the first field of a known_hosts line, verbatim: a plain name, a
-- "[name]:port" form, a comma list, or a hashed "|1|salt|hash" entry.
CREATE TABLE known_hosts (
    id             TEXT PRIMARY KEY,
    vault_id       TEXT NOT NULL REFERENCES vaults(id),
    updated_at     TEXT NOT NULL,
    version_vector TEXT NOT NULL DEFAULT '{}',
    field_stamps   TEXT NOT NULL DEFAULT '{}',
    deleted        INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    host           TEXT NOT NULL,
    port           INTEGER NOT NULL DEFAULT 22,
    hashed         INTEGER NOT NULL DEFAULT 0 CHECK (hashed IN (0, 1)),
    marker         TEXT,
    key_type       TEXT NOT NULL,
    key_blob       BLOB NOT NULL,
    fingerprint    TEXT NOT NULL,
    comment        TEXT,
    added_at       TEXT NOT NULL,
    UNIQUE (vault_id, host, key_type, key_blob)
);
CREATE INDEX known_hosts_host ON known_hosts(host);

-- One chain per target host; hops are ordered host ids (first hop first).
CREATE TABLE jump_chains (
    id             TEXT PRIMARY KEY,
    vault_id       TEXT NOT NULL REFERENCES vaults(id),
    updated_at     TEXT NOT NULL,
    version_vector TEXT NOT NULL DEFAULT '{}',
    field_stamps   TEXT NOT NULL DEFAULT '{}',
    deleted        INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1)),
    host_id        TEXT NOT NULL UNIQUE REFERENCES hosts(id)
);

CREATE TABLE jump_chain_hops (
    chain_id    TEXT NOT NULL REFERENCES jump_chains(id) ON DELETE CASCADE,
    position    INTEGER NOT NULL,
    hop_host_id TEXT NOT NULL REFERENCES hosts(id),
    PRIMARY KEY (chain_id, position)
);

-- Per-key settings. The sync columns let a future lane merge them per key.
CREATE TABLE settings (
    key            TEXT PRIMARY KEY,
    value          TEXT NOT NULL,
    updated_at     TEXT NOT NULL,
    version_vector TEXT NOT NULL DEFAULT '{}',
    field_stamps   TEXT NOT NULL DEFAULT '{}',
    deleted        INTEGER NOT NULL DEFAULT 0 CHECK (deleted IN (0, 1))
);

-- Overwritten values, kept so a merge or edit can be undone. Values of secret
-- fields are stored encrypted (encrypted = 1).
CREATE TABLE record_history (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    record_table TEXT NOT NULL,
    record_id    TEXT NOT NULL,
    field        TEXT NOT NULL,
    old_value    BLOB,
    encrypted    INTEGER NOT NULL DEFAULT 0 CHECK (encrypted IN (0, 1)),
    changed_at   TEXT NOT NULL,
    device       TEXT NOT NULL
);
CREATE INDEX record_history_record ON record_history(record_id, seq);
