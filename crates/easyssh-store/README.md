# easyssh-store

The local SQLite store: hosts, groups, snippets, tunnels, known hosts, settings
and the encrypted `host_secrets`, with numbered migrations (`migrations/`) and
a one-time import of the v1 JSON file (`open_app_store`).

## Sync

`sync.rs` adapts the store to `easyssh-sync`:

- `StoreSync` implements `SyncStore`. Records are mapped to field maps with a
  stamp per field and a tombstone from the `deleted` pseudo field; remote
  records that fail validation, or would duplicate a local one, are skipped,
  not applied. `host_secrets` is never mapped.
- Everything a merge overwrites, locally or from a remote, is written to
  `record_history`; `Store::restore_host_field` puts a value back as a normal
  edit.
- Migration 0004 adds `sync_account` (the sealed account keys and token),
  `sync_records` (the server vector last seen per record), `sync_cursors`,
  `sync_high_water` and `sync_pins` (member keys, trust on first use), and the
  `role`, `version_vector` and `field_stamps` columns of `vaults`.
- `move_host_to_vault` copies a host to a new id in the target vault and
  removes the original, because the vault is part of a record's identity.
- `personal_vault_server_id` derives the personal vault's id on the server
  from the account id.

`src/sync_tests.rs` covers the adapter: all entity kinds, concurrent edits to
different fields, a same-field conflict with history and restore, tombstones,
persistence of cursor and high-water marks, team vault names and roles, and
that secrets are not synced.
