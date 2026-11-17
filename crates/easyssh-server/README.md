# easyssh-server

Self-hosted sync server for EasySSH v2 (architecture-v2.md §8). One binary,
one SQLite file. **License: AGPL-3.0-only** (see `LICENSE`); the desktop app
and every other crate stay MIT.

## What the server knows

| Stored | Why | Can the server read it? |
|---|---|---|
| username, device names, vault ids, record ids, table names, version vectors, deleted flag, timestamps | routing | yes (plain metadata) |
| OPAQUE registration record | sign-in | it holds no passphrase; a leak allows an offline guess attack that pays Argon2id (client side) plus OPAQUE per guess |
| KDF header (public salt and cost) | the client derives its auth secret from it | yes (public) |
| wrapped account key pairs | new devices recover them with the passphrase | no (encrypted under a key derived from the passphrase) |
| account X25519 / Ed25519 public keys, device public keys and certificates | sharing, verifying | yes (public) |
| sealed vault keys, one per member | members open them with their account key | no |
| record ciphertexts and signatures | the data | no (XChaCha20-Poly1305 under the vault key) |
| session tokens | API access | only their SHA-256 is stored |

Labels, addresses, usernames, passwords, keys and snippets are inside the
ciphertext. The server never receives the passphrase, the auth secret, the
account key or a vault key. It verifies envelope signatures and device
certificates (defence in depth) but cannot decrypt or merge; clients merge.

## Run it

```sh
cargo build --release -p easyssh-server
EASYSSH_LISTEN=127.0.0.1:8787 EASYSSH_DB=/var/lib/easyssh/server.db \
  ./target/release/easyssh-server
```

or with Docker (build from the repository root):

```sh
docker build -f crates/easyssh-server/Dockerfile -t easyssh-server .
docker run -d --name easyssh -p 127.0.0.1:8787:8787 -v easyssh-data:/data easyssh-server
```

### Behind TLS

The server speaks plain HTTP and **must** sit behind a TLS terminator:
session tokens are bearer credentials. Caddy example:

```caddyfile
sync.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

nginx: `proxy_pass http://127.0.0.1:8787;` with `proxy_set_header X-Forwarded-For $remote_addr;`
and `client_max_body_size 2m;`. Start the server with `EASYSSH_TRUST_PROXY=true`
so rate limiting sees the real client address, and only then (otherwise a
client could choose its own address).

### Configuration

Flag / environment variable (flags win):

| Flag | Variable | Default |
|---|---|---|
| `--listen` | `EASYSSH_LISTEN` | `127.0.0.1:8787` |
| `--db` | `EASYSSH_DB` | `easyssh-server.db` |
| `--session-days` | `EASYSSH_SESSION_DAYS` | `30` |
| `--allow-registration` | `EASYSSH_ALLOW_REGISTRATION` | `true` (set `false` after creating your accounts) |
| `--max-body-bytes` | `EASYSSH_MAX_BODY_BYTES` | `2097152` |
| `--auth-rate-per-minute` | `EASYSSH_AUTH_RATE_PER_MINUTE` | `30` (per client address, and per username) |
| `--trust-proxy` | `EASYSSH_TRUST_PROXY` | `false` |
| `--log-format` | `EASYSSH_LOG_FORMAT` | `text` (`json` for structured logs; `RUST_LOG` filters) |

Logs carry the method, route pattern, status and latency of each request and
a few audit events (account registered, session created, device revoked,
membership changed). No tokens, headers, bodies or concrete ids are logged.

### Operations

Back up the SQLite file (it is in WAL mode: use `sqlite3 server.db ".backup out.db"`).
The file also holds the OPAQUE server key (`meta.opaque_setup`); losing it
locks every user out of sign-in. Health check: `GET /healthz`.

## API

All paths are under `/v1`, JSON bodies, `Authorization: Bearer <token>` except
the `auth/*` endpoints. Binary fields are base64. Types live in
`easyssh-sync::wire`; the sync crate README has the full protocol.

| Method and path | Who | Notes |
|---|---|---|
| `GET /auth/kdf?username=` | anyone | public KDF header; stable fake for unknown names |
| `POST /auth/register/start`, `/finish` | anyone | OPAQUE registration; closable |
| `POST /auth/login/start`, `/finish` | anyone | OPAQUE login; finish returns the token and the wrapped key blob |
| `POST /auth/logout` | session | |
| `GET /accounts/lookup?username=` | session | public keys of an account (to share a vault) |
| `POST /devices`, `GET /devices`, `DELETE /devices/{id}` | session | register (account-signed cert required), list, revoke |
| `POST /vaults`, `GET /vaults` | session | create (caller becomes owner), list mine with sealed keys |
| `GET /vaults/{v}/members` | member | |
| `PUT`/`DELETE /vaults/{v}/members/{account}` | owner | add (needs sealed key), change role, remove; last owner is protected |
| `POST /vaults/{v}/records` | owner, editor | compare-and-set push; 409 returns the server copy |
| `GET /vaults/{v}/records?since=&limit=` | member | pull by monotonic sequence |
| `GET /vaults/{v}/records/{r}/history` | member | last 50 replaced envelopes, newest first |

Every response carries `x-easyssh-max-body: <bytes>`, the largest request
body accepted. A request that announces a larger `Content-Length` gets
`413` with error code `too_large`; if it announces at most 4 times the limit
the server first reads and discards the body (frame by frame, nothing is
kept), so the client finishes sending and reliably sees the 413 instead of a
connection reset. Bodies announcing more than that are refused at once and
their sender may see a reset. Older clients ignore the header.

Limits: 2 MiB request body (configurable), 1 MiB ciphertext per record,
ids 1-128 characters, 50 history versions per record. Auth endpoints are
rate limited per client address and per username (HTTP 429 with `Retry-After`).

## Known limits

- Removing a vault member does not rotate the vault key; the former member
  keeps the key (and any data they already pulled). Key rotation is future work.
- A revoked device's earlier envelopes stay valid (their certificate is
  unchanged); revocation stops new pushes and ends its sessions.
- Public keys shown to a sharing owner come from the server. A malicious
  server could substitute its own; verify account fingerprints out of band
  for vaults that matter (not automated yet).
- Single process, single SQLite writer; there is no clustering.
