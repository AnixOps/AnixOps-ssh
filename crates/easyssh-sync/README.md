# easyssh-sync

Sync client for EasySSH v2 (architecture-v2.md §8). MIT. It talks to
`easyssh-server` (or any server implementing the protocol below) and does
everything that needs plaintext: the server only relays signed ciphertext.

| Module | Role |
|---|---|
| `wire` | JSON protocol types (`Envelope`, push/pull/history, accounts, devices, vaults, errors) |
| `record` | `RecordVersion`: decrypted fields, per-field `Stamp {lamport, device}`, tombstone |
| `merge` | `merge(local, remote) -> Merged` |
| `verify` | `Verifier`: signature, certificate and rollback checks; `HighWater` |
| `codec` | `seal_record` / `open_record` (encrypt + sign, verify-then-decrypt) |
| `engine` | `SyncEngine` push/pull loop over a `Transport`; `SyncStore` adapter trait |
| `transport`, `http` | `Transport` trait, in-memory `MemoryServer`, `HttpClient` (reqwest + rustls) |
| `auth`, `account` | OPAQUE client side, register/login, device enrollment, vault sharing |

## Wire protocol

Every body is JSON; binary values are standard base64. Errors are
`{"code": "<snake_case>", "message": "..."}` with codes `bad_request`,
`unauthorized`, `forbidden`, `not_found`, `conflict`, `too_large`,
`rate_limited`, `device_not_registered`, `device_revoked`,
`invalid_signature`, `registration_closed`, `internal`.

### Envelope

```json
{
  "record_id": "…", "vault_id": "…", "table": "hosts",
  "version_vector": {"<device id>": 3, "<device id>": 1},
  "deleted": false,
  "ciphertext": "<b64 record blob>",
  "signer": "<b64 device Ed25519 public key>",
  "signature": "<b64>",
  "cert": "<b64 DeviceCert signed by the account key>"
}
```

`signature` is `easyssh-crypto`'s envelope signature (it covers record id,
vault id, version vector, deleted flag, ciphertext and signer; see that
crate's README). `table` is not covered by that signature, so it is repeated
inside the ciphertext and `open_record` rejects a mismatch; the delete flag
is likewise cross-checked against the decrypted tombstone. Device ids are the
lowercase hex of the device public key (`device_id_for`).

### Plaintext inside the ciphertext

XChaCha20-Poly1305 under the vault key, AAD =
`record id, vault id, schema 1, SHA-256(version vector)` (crypto README).

```json
{"v":1, "table":"hosts",
 "fields": {"name":"db-prod","port":22},
 "stamps": {"name":{"lamport":4,"device":"…"},"port":{"lamport":1,"device":"…"}},
 "tombstone": null}
```

Every field has a stamp. Setting a field to `null` removes it (the stamp stays).
`tombstone` is the stamp of the newest delete, if any.

### Endpoints (server `/v1`)

| | Request | Response |
|---|---|---|
| `GET /auth/kdf?username=` | | `{kdf_header}` |
| `POST /auth/register/start` | `{username, request}` | `{response}` (OPAQUE) |
| `POST /auth/register/finish` | `{username, upload, kdf_header, wrapped_account_keys, seal_public, signing_public}` | `{account_id}` |
| `POST /auth/login/start` | `{username, request}` | `{login_id, response}` |
| `POST /auth/login/finish` | `{login_id, finalization}` | `{token, expires_at, account_id, wrapped_account_keys, seal_public, signing_public}` |
| `POST /devices` | `{device_public, name, cert}` | `DeviceInfo` |
| `POST /vaults` | `{vault_id, sealed_key}` | 201 |
| `PUT /vaults/{v}/members/{account}` | `{role, sealed_key?}` | 204 |
| `POST /vaults/{v}/records` | `{envelope, expected}` | 200 `{"status":"accepted","seq"}` or 409 `{"status":"conflict","reason":"cas"\|"stale","current":Envelope\|null}` |
| `GET /vaults/{v}/records?since=&limit=` | | `{records:[{seq,envelope}], cursor, has_more}` |
| `GET /vaults/{v}/records/{r}/history` | | `{versions:[{seq,replaced_at,envelope}]}` newest first |

(Other device/vault/account endpoints are listed in the server README.)

### Sign-in

The passphrase is stretched locally (Argon2id, `easyssh-crypto`), giving an
`AuthSecret` and an `AccountKey`. The `AuthSecret` is the OPAQUE (RFC 9807,
`opaque-ke`) password: Ristretto255 OPRF, 3DH, SHA-512, identity KSF (the
stretching already happened). The server stores only the OPAQUE record and
never sees the passphrase, the `AuthSecret` or the `AccountKey`. After login
the client unwraps the account key pairs from the stored blob; a wrong
passphrase fails inside the client (OPAQUE finish) before any token exists.
`GET /auth/kdf` returns a deterministic fake header for unknown users so
account existence is not revealed there.

### Push (compare-and-set) and conflicts

`expected` is the version vector of the server copy the client believes it
replaces (empty for a new record). The server accepts only if its current
copy has exactly that vector (`cas`) and the new vector is strictly `After`
it (`stale`); otherwise 409 with the server's envelope. The client verifies
and merges that envelope, bumps its own counter (a merge is a new version
dominating both inputs) and pushes again, up to 8 rounds per record.
A retry after a lost response is harmless: the server copy then equals what
was pushed and merges as `Equal`.

## Merge rules

`merge(local, remote)` is pure, deterministic and symmetric:

1. Compare version vectors.
   - `Equal`: nothing to do.
   - `After`: keep local, no field work (it still needs pushing if the server lacks it).
   - `Before`: adopt remote wholesale; local values it replaces are reported.
   - `Concurrent` (or equal vectors with different content, e.g. a reused counter): combine.
2. Combine, field by field: the side with the larger stamp `(lamport, device id)`
   wins that field (lamport first, then byte-wise device id). A field edited on one
   side only survives. The vector becomes the element-wise maximum.
3. Overwritten values are returned in `Merged::overwritten` so the store writes
   them to `record_history`: a replaced local value is always reported; a
   replaced remote value only if the local side never saw the write that made
   it (its device counter in the remote vector is higher than in the local one).
   The server keeps the last 50 replaced envelopes per record independently.

Local edits use `RecordVersion::edit` (counter + 1; every changed field gets
one new stamp, lamport = highest seen + 1), so a later edit always outranks
whatever the editing device had seen.

### Tombstones

A delete is an ordinary signed envelope with `deleted = true` whose payload
holds a tombstone stamp (the record's data stays so history can restore it).
In a concurrent merge the tombstone is the larger of the two tombstones, and
**the record is deleted only if that tombstone stamp is strictly newer than
every field stamp of the merged record**. An edit stamped after the delete
(higher lamport, or equal lamport and larger device id) keeps the record alive;
a delete stamped after every edit removes it. A causally later edit of a
deleted record revives it. The server never deletes rows: it relays tombstones.

## Verification

`Verifier::verify_and_record` rejects, in this order: a missing or malformed
certificate, a certificate not signed by a trusted account key (an
uncertified device, or an account the client does not trust), a certificate
for a different device than the signer, a bad signature (any changed field),
and **rollback**: an envelope whose vector is `Before` the high-water vector
already seen for that record (the join of everything accepted or pushed).
Re-delivery (`Equal`) and newer or concurrent versions pass. Persist
`HighWater::snapshot()` with the local database so protection survives
restarts. Rejected envelopes are reported in `SyncReport::rejected`; the
sync continues without them.

Trusted accounts: the client's own, plus members of shared vaults added with
`trust_members`, which takes their signing keys from the server's member
list (trust on first use; compare fingerprints out of band for sensitive vaults).

## Request size

The server sends `x-easyssh-max-body: <bytes>` on every response. `HttpClient`
remembers it and, for a later request whose JSON body is larger, fails
locally with `TransportError::Api { status: 413, code: TooLarge, .. }`, the
error the server itself would answer with, without sending the body. Until
the first response the limit is unknown and the server decides (it answers
413 `too_large`, reading and discarding bodies up to 4 times its limit so the
answer arrives). Servers that do not send the header are never limited by the
client, and older clients simply ignore it: the change is additive.

## Limits

- The crate does not touch SQLite: `SyncStore` is the adapter the engine
  crate implements over `easyssh-store` (`MemoryStore` is for tests).
- No key rotation on member removal, no certificate expiry, no push of
  multiple records per request, no websocket notifications (poll with `pull`).
- Concurrent edits to one field keep one winner; the loser is restorable
  from history, not merged textually.

## In the app

`easyssh-store` implements `SyncStore` (`StoreSync`) over the app database:
hosts, groups, snippets, tunnels and known hosts, with history kept in
`record_history`, the version vectors and field stamps in the tables, and the
pull cursor and `HighWater` persisted. Saved secrets are not mapped. The
daemon's `syncsvc` drives it; see `crates/easysshd/README.md`.
