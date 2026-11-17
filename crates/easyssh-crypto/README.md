# easyssh-crypto

Local cryptographic building blocks for EasySSH v2 (architecture-v2.md §6 and
§8). There are no custom primitives: XChaCha20-Poly1305, Argon2id,
HKDF-SHA256, X25519 and Ed25519 from RustCrypto and dalek, composed as
described below. Every format starts with a version byte and is pinned by
the committed vectors in `tests/vectors/*.json`, which mobile and server
implementations can check against.

Features: `os-keyring` adds `OsKeyStore` (Windows Credential Manager, macOS
Keychain, Linux Secret Service). Default features need no secret service.

## Key hierarchy

```text
passphrase ──Argon2id(m=64 MiB, t=3, p=1, salt16)──▶ master (32 B, transient)
                 │
                 ├─HKDF "easyssh/v1/account-key"──▶ AccountKey ──wraps (XChaCha20-Poly1305)──▶ account key pairs blob
                 └─HKDF "easyssh/v1/auth"─────────▶ AuthSecret   (sent to the server at sign-in; phase 5, OPAQUE)

Account key pairs (random, generated once; stored only wrapped):
   X25519  ──seals──▶ VaultKey per member
   Ed25519 ──certifies──▶ device signing keys

VaultKey  (32 random B) ──sealed to each member's X25519 public key──▶ protects records of one vault
DeviceKey (32 random B, OS keychain) ──HKDF "easyssh/v1/field-cipher"──▶ protects local database fields
DeviceSigningKey (Ed25519, seed in keychain) ──certified by the account Ed25519 key──▶ signs sync envelopes
```

| Key | Stored | Protects / is used for |
|---|---|---|
| Passphrase | nowhere | root of `AccountKey` and `AuthSecret` |
| `AccountKey` | never (re-derived) | wraps the account key pairs |
| `AuthSecret` | never (re-derived) | server sign-in only; independent of `AccountKey` |
| Account X25519 / Ed25519 pairs | wrapped blob (device profile, later the server) | open sealed vault keys; sign device certificates |
| `VaultKey` | sealed per member, server may hold the sealed copies | record ciphertexts |
| `DeviceKey` | OS keychain | local database fields |
| Device Ed25519 | seed in OS keychain | envelope signatures |

The account key pairs are **random**, not derived from the passphrase. A
passphrase change derives a new `AccountKey` and only re-wraps the blob
(`unwrap_account_keys` with the old key, `wrap_account_keys` with the new
one): vault shares and device certificates stay valid. A new device recovers
the pairs by unwrapping the stored blob with the passphrase-derived
`AccountKey`; the server stores the blob but never sees the account key.

## Formats

All integers are big-endian. `lp(x)` = `u32 length || bytes`.

**KDF header** (29 bytes, public, stored with the account):
`0x01 || m_kib(u32) || t(u32) || p(u32) || salt(16)`. Argon2id v1.3, 32-byte
output. Parsing rejects p/t = 0, `m < 8*p` KiB and anything above 1 GiB, t 16 or
p 16 so a hostile server cannot make a client allocate arbitrarily. The default
for desktop is m=65536, t=3, p=1 (`KdfParams::DESKTOP_V1`).
`master` is split with HKDF-SHA256 (no salt) using the infos shown above.

**Record blob**: `0x01 || nonce(24) || ciphertext || tag(16)`, XChaCha20-Poly1305
keyed by the `VaultKey`. Associated data:
`"easyssh/v1/record" || lp(record_id) || lp(vault_id) || schema_version(u32) || version_vector_digest(32)`.
`version_vector_digest` is SHA-256 of the version vector's canonical encoding.

**Wrapped account keys** (105 bytes): `0x01 || nonce(24) || ct(64) || tag(16)`;
plaintext is `x25519_secret(32) || ed25519_seed(32)`. Key =
`HKDF-SHA256(ikm = AccountKey, info = "easyssh/v1/account-keys-wrap")`; AAD =
`"easyssh/v1/account-keys" || key_pair_version(0x01)`. The leading byte is both
the blob version and the key-pair layout version. A wrong passphrase, any
tampering or truncation fails to unwrap.

**Sealed vault key** (105 bytes): `0x01 || eph_pub(32) || nonce(24) || ct(32) || tag(16)`.
An ECIES construction from standard parts (the `crypto_box` crate has no
stable release on the current RustCrypto traits): `shared = X25519(eph_secret, recipient_pub)`
(rejected if all-zero); `key = HKDF-SHA256(salt = eph_pub || recipient_pub, ikm = shared, info = "easyssh/v1/sealed-vault-key")`;
XChaCha20-Poly1305 with AAD `"easyssh/v1/sealed-vault-key" || eph_pub || recipient_pub`.
Anyone can seal to a public key; only the holder can open. The sender is
not authenticated by this layer.

**Field blob**: `0x01 || nonce(24) || ciphertext || tag(16)`; key =
`HKDF-SHA256(DeviceKey, info = "easyssh/v1/field-cipher")`; AAD =
`"easyssh/v1/field" || lp(caller_aad)`. Callers should put the row and column
identity in `caller_aad`.

**Version vector, canonical encoding**: `count(u32)` then each entry in
ascending byte order of the device id (UTF-8): `lp(device_id) || counter(u64)`.
Counters of 0 are never stored (missing = 0), so equal vectors always
encode identically. The parser is strict: unsorted or duplicate ids, zero
counters, truncation and trailing bytes are errors. `compare` returns
`Before`, `After`, `Equal` or `Concurrent`; `merge` is the element-wise
maximum.

**Device certificate**, signed by the account Ed25519 key over
`"easyssh/v1/device-cert" || device_public(32) || issued_at(i64, unix s) || lp(device_name)`.
Wire form: `0x01 || device_public(32) || issued_at(i64) || lp(device_name) || signature(64)`.

**Envelope**, signed by the device Ed25519 key over (this is the canonical
encoding; the signature is not part of it):

```text
"easyssh/v1/envelope"          19 ASCII bytes, domain separation
lp(record_id)                  UTF-8
lp(vault_id)                   UTF-8
version_vector                 canonical encoding above (self-delimiting)
deleted                        1 byte, 0x00 or 0x01 (tombstone)
lp(ciphertext)                 the record blob; may be empty for a tombstone
signer                         device public key, 32 bytes
```

`verify_envelope(envelope, cert, account_public)` checks, in order: the
certificate signature under the account key, `cert.device_public == envelope.signer`,
and the envelope signature (`verify_strict`). Any changed field, an
uncertified device or a certificate from another account fails.

## Out of scope

- OPAQUE / server sign-in (phase 5); this crate only produces `AuthSecret`.
- Rollback protection state (remembering the newest vector seen per record):
  `verify_envelope` is stateless, callers compare version vectors.
- Certificate revocation and expiry policy, cert distribution.
- Rotating the account key pairs themselves (new random pairs would need every vault re-sealed and devices re-certified).
- Authenticating who sealed a vault key; vault membership / key rotation.
- Session-log encryption (secretstream), SSH key handling, the SQLite schema.
- Hardware-backed key unlock (Windows Hello, Touch ID, FIDO2).
- Protection against a compromised host (memory scraping while unlocked) and
  side channels beyond what the underlying crates provide. Secrets are
  zeroized on drop on a best-effort basis; `Vec<u8>` plaintext returned from
  decryption is the caller's responsibility.

## Vectors

`cargo test -p easyssh-crypto --test vectors` recomputes every file in
`tests/vectors/` from fixed keys and nonces and compares it byte for byte,
then feeds the committed outputs to the decrypting/verifying side. Regenerate
only for an intentional format change (which needs a new version byte):
`UPDATE_VECTORS=1 cargo test -p easyssh-crypto --test vectors`.
