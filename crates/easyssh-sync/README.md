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
| `engine` | `SyncEngine` push/pull loop, batch upload and vault key rotation over a `Transport`; `SyncStore` adapter trait |
| `transport`, `http` | `Transport` trait, in-memory `MemoryServer`, `HttpClient` (reqwest + rustls) |
| `auth`, `account` | OPAQUE client side, register/login, device enrollment, vault sharing |

## Wire protocol

Every body is JSON; binary values are standard base64. Errors are
`{"code": "<snake_case>", "message": "..."}` with codes `bad_request`,
`unauthorized`, `forbidden`, `not_found`, `conflict`, `too_large`,
`rate_limited`, `device_not_registered`, `device_revoked`,
`invalid_signature`, `certificate_expired`, `stale_key_generation`,
`registration_closed`, `internal`.

### Envelope

```json
{
  "record_id": "…", "vault_id": "…", "table": "hosts", "key_generation": 2,
  "version_vector": {"<device id>": 3, "<device id>": 1},
  "deleted": false,
  "ciphertext": "<b64 record blob>",
  "signer": "<b64 device Ed25519 public key>",
  "signature": "<b64>",
  "cert": "<b64 DeviceCert signed by the account key>",
  "received_at": 1760000000
}
```

`signature` is `easyssh-crypto`'s envelope signature (it covers record id,
vault id, table, key generation, version vector, deleted flag, ciphertext and
signer; see that crate's README). The table and the delete flag are also
repeated inside the ciphertext and `open_record` rejects a mismatch. Device
ids are the lowercase hex of the device public key (`device_id_for`).

`received_at` is the one field the server sets: unix seconds at which it
stored this version (ignored on push, 0 when unknown, not signed). Clients
judge device revocation and certificate expiry against it.

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
| `PUT /vaults/{v}/members/{account}` | `{role, sealed_key?, key_generation?}` (`key_generation` required with a key) | 204 |
| `POST /vaults/{v}/rotate` | `{from_generation, remove_member?, sealed_keys:[{account_id, sealed_key}], records:[{envelope, expected}]}` | 200 or 409 `{committed, key_generation, members_changed, missing, results}` |
| `GET /vaults/{v}/revoked-devices` | | `[{device_id, revoked_at}]` for devices of the vault's members |
| `POST /vaults/{v}/records` | `{envelope, expected}` | 200 `{"status":"accepted","seq"}` or 409 `{"status":"conflict","reason":"cas"\|"stale","current":Envelope\|null}`; 412 `stale_key_generation` |
| `POST /vaults/{v}/records/batch` | `{items:[{envelope, expected}]}` (1 to 500) | 200 or 409 `{committed, results:[accepted\|conflict\|rejected\|not_applied]}` |
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

### Batch push

`POST /vaults/{v}/records/batch` carries up to 500 pushes. Each is checked
exactly like a single push (compare-and-set, certificate, signature, key
generation) in one transaction, and the batch is applied only if every item
passes: otherwise nothing is written and the response lists each item as
`accepted` (would have been fine, reported as `not_applied`), `conflict`
(with the server copy), or `rejected` (with the error code). The engine uses
it for records the server has never seen (first upload, 100 records or
256 KiB of ciphertext per request) and falls back to single pushes, which
merge and retry, for whatever a batch could not take.

### Vault key rotation

Every vault key has a generation (1 at creation). The generation of the key
a ciphertext is encrypted under is in the signed envelope; the server
accepts writes (single, batch, member keys) only under the current one and
answers anything else with 412 `stale_key_generation`. Pulled envelopes of a
generation the client has no key for fail with
`SyncError::UnknownKeyGeneration`; both errors mean "fetch the vault list
again" (`SyncError::is_stale_key`).

`SyncEngine::rotate_vault_key` (owners only) re-encrypts every record the
device holds under a fresh key as a new version signed by this device and
posts them with the new key sealed to each remaining member and, when
removing someone, that member's account id. The server checks in one
transaction that `from_generation` is current, that `sealed_keys` names
exactly the members left, and that `records` covers every record of the
vault, each compare-and-set against what the server holds; then it removes
the member, stores the sealed keys and bumps the generation. All or nothing:
otherwise the response says why (`RotationFailure`: generation changed,
members changed, records changed meanwhile, records this device does not have,
or a rejected record) and nothing was written. Sync and call again on the
retryable ones. Removing a member is only possible this way.

The removed member keeps what they had already pulled and their copy of the
old key; they cannot read the re-encrypted records or anything written later,
and the server no longer shows them the vault. Versions that the server keeps
as history from before the rotation stay under the old key and are skipped by
`history` on clients that no longer hold it.

### Revocation and expiry

A device certificate carries an expiry (one year). The server refuses to
register an expired one and refuses envelopes whose certificate has expired
when it receives them; a signed-in device renews its certificate (same key,
new certificate, `POST /devices` again) when less than 30 days are left, and
every sign-in issues a fresh one. Revoking a device records the server time.
The server refuses everything a revoked device sends afterwards. Clients take
the revocation times of the vault's members from `GET /vaults/{v}/revoked-devices`
(`Verifier::revoke_device`, `trust_revocations`) and reject an envelope signed
by a revoked device whose `received_at` is later than the revocation (or
unknown); earlier ones stay valid, as do envelopes whose certificate was
valid at `received_at` but has expired since. `received_at` comes from the
server and is not signed: a malicious server could lie about it. The
protection is against a stolen or lost device, not against a hostile server.

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
certificate, a signer revoked before the server received the envelope, a
certificate not signed by a trusted account key (an uncertified device, or an
account the client does not trust), a certificate that had expired when the
server received the envelope, a certificate for a different device than the
signer, a bad signature (any changed field, the table and key generation
included), and **rollback**: an envelope whose vector is `Before` the high-water vector
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
- No websocket notifications (poll with `pull`). Rotation re-encrypts the
  current version of every record; server-side history from before a rotation
  stays under the old key. A record of a table this version does not know
  cannot be re-encrypted, so rotation fails until the app is updated.
- Concurrent edits to one field keep one winner; the loser is restorable
  from history, not merged textually.

## In the app

`easyssh-store` implements `SyncStore` (`StoreSync`) over the app database:
hosts, groups, snippets, tunnels and known hosts, with history kept in
`record_history`, the version vectors and field stamps in the tables, and the
pull cursor and `HighWater` persisted. Saved secrets are not mapped. The
daemon's `syncsvc` drives it; see `crates/easysshd/README.md`.
