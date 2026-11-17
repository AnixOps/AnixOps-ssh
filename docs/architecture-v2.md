# Architecture v2

Status: **proposal, waiting for the owner's approval**. Nothing here is built
yet; the shipped app is still the v1 design described in the README.

v1 is a thin, careful shell over the system OpenSSH. v2 keeps that care but
changes what the product is: an open-source, local-first, end-to-end
encrypted home for hosts, keys and sessions, with an optional self-hosted
server for sync and teams. The terminal stays the place where those assets
are used.

The bar we set ourselves is to match the best commercial SSH clients on
everyday use and to beat them on trust: signed sync, authenticated local IPC,
no secret ever readable by a server, and every line of it open source.

## 1. Product tiers

| Tier | Who | Needs an account | Cost |
|---|---|---|---|
| Personal | One person, one or more devices | No | Free |
| Self-hosted sync | The same person, or a team that runs our open-source server | On their own server | Free |
| Team | Shared vaults, member roles, audit log, SSO, shared session logs | On a self-hosted or hosted server | Paid features; the hosted server is "we run it for you" |

A personal user is never blocked from a feature that works on one machine:
SFTP, multiple keys, snippets, tunnels, split panes and the vault are all
free. Money comes from what only a team needs: sharing, roles, audit, SSO and
operations.

## 2. Process model

```
┌──────────────┐  authenticated local IPC  ┌──────────────────────────────┐
│  Tauri UI    │ ◄───── events / commands ──► │  easysshd (desktop daemon)   │
│  React +     │                              │  └─ easyssh-engine (library) │
│  xterm.js    │                              │     sessions · sftp · tunnels│
└──────────────┘                              │     store · vault · sync     │
                                              └──────────────┬───────────────┘
 Mobile app (Tauri mobile): engine embedded in-process        │ HTTPS / WebSocket
                                                              ▼
                                              easyssh-server (optional, self-hosted)
```

- **The engine is a library.** On the desktop it runs inside `easysshd`, a
  separate process that outlives the UI: closing or reloading the window
  does not drop a session, and the UI reattaches and replays the scrollback
  buffer. On mobile, where a resident background process is not allowed, the
  same library runs inside the app and relies on reconnect and resume.
- **The backend is the single source of truth.** The UI holds a projection
  of engine state built from an event stream and sends commands; it never
  owns hosts, sessions or settings.
- **Local IPC is authenticated.** A Unix socket or Windows named pipe in the
  user's runtime directory, owner-only permissions, a per-launch 32-byte
  secret handed to the UI at start, and an HMAC on every frame. A different
  local user or a process without the secret cannot talk to the daemon.
- **Taking over a session from another device** goes through the sync
  server as a relay; the session itself stays on the device that opened it.

## 3. Contract

One JSON Schema set (`crates/easyssh-proto`) defines every command, event
and entity. Rust types are generated from it and validated at the IPC
boundary; TypeScript types and runtime validators are generated for the UI.
A message that does not match the schema is rejected on both sides. This
replaces the hand-written Tauri commands and `src/lib/backend.ts` mirror.

## 4. Crates

| Crate | Role |
|---|---|
| `easyssh-proto` | JSON Schema, generated types, schema version |
| `easyssh-ssh` | russh transport: auth chain, channels, jump chains, known hosts, agent bridge, FIDO2 |
| `easyssh-openssh` | Compatibility engine: today's `openssh.rs`, `sftp.rs`, `tunnel.rs`, `askpass` moved here |
| `easyssh-sftp` | File operations and the transfer queue over either engine |
| `easyssh-pty` | Local shells (ConPTY on Windows) and serial ports |
| `easyssh-store` | SQLite storage, entities, migrations (including from the v1 JSON file) |
| `easyssh-crypto` | Key hierarchy, vaults, entity encryption, device signing keys |
| `easyssh-sync` | Sync client: version vectors, field-level merge, history |
| `easyssh-engine` | Puts the above together: session registry, event bus, commands |
| `easysshd` | Desktop daemon binary and the local IPC server |
| `easyssh-server` | Self-hosted sync server (Rust, shares `proto`, `crypto` and `sync`) |

`src-tauri` becomes a thin client of `easysshd`.

## 5. SSH engine

**russh is the main engine; the system OpenSSH is the fallback.**

- One connection per host carries the terminal, the file pane and tunnels as
  separate channels, so a password or key prompt happens once.
- Authentication runs as a chain the engine controls: agent, keys from the
  vault, FIDO2 security keys, keyboard-interactive and password. Prompts are
  structured events the UI answers; nothing is typed into a hidden terminal.
- Agents: the system agent (`SSH_AUTH_SOCK`, or the OpenSSH named pipe on
  Windows) and the 1Password agent are used through their sockets.
- Jump hosts are a chain of entities edited visually. Each hop is opened as a
  forwarded channel inside the previous connection, and intermediate hops
  stay warm while a session needs them.
- Known hosts live in the store (syncable per vault), with a trust prompt
  for new keys, a clear warning for changed keys, and import from
  `~/.ssh/known_hosts`.
- `~/.ssh/config` is imported into host entities. A host that uses something
  the engine does not support (`ProxyCommand`, `Match`, `LocalCommand`,
  unusual algorithms) is marked and runs on the OpenSSH engine instead, so
  existing setups keep working.
- The OpenSSH engine is temporary as a first-class path. Once russh passes
  the whole end-to-end suite, OpenSSH becomes an advanced option per host
  and gets fixes only, no new features.
- FIDO2 `sk-ecdsa` / `sk-ed25519` support is added on top of russh, through
  libfido2, and offered upstream.
- Serial ports join SSH and local shells as a session type. Mosh and Telnet
  are not planned.

## 6. Secrets and encryption

The v1 promise "we never store your secrets" becomes "we store them only
encrypted, with keys only you hold, in code anyone can audit".

| Key | Where it lives | Protects |
|---|---|---|
| Device key (random 32 bytes) | OS keychain; unlocked with Windows Hello, Touch ID / Secure Enclave or a FIDO2 key where available | Local database fields |
| Account key | Derived from the passphrase with Argon2id; never stored, never sent | The account's X25519 key pair |
| Vault keys (random 32 bytes) | Sealed to each member's X25519 public key | Entities in that vault |
| Device signing key (Ed25519) | Keychain; certified by the account key | Signatures on every sync payload |

- Entities are encrypted as whole records with XChaCha20-Poly1305. The
  associated data binds the record id, vault id and version, so a server
  cannot move ciphertext between records or vaults.
- Only routing metadata stays in plain text: id, vault id, version vector,
  deleted flag, signer. Labels, addresses, usernames, passwords, keys and
  snippets are all inside the ciphertext.
- Signing in to a server proves knowledge of the passphrase without sending
  it (SRP-6a or OPAQUE; chosen in phase 5 with test vectors).
- Without a server, the personal vault is protected by the device key and an
  optional passphrase.
- Session logs are encrypted with libsodium secretstream on write.

## 7. Storage

SQLite (bundled `rusqlite`) is the local store: one table per entity type,
transactions, indexes and numbered migrations. Sensitive fields are
encrypted with the device key. On first start, v2 reads the v1 JSON
configuration, migrates it, and keeps the original file as a backup.

Git sync from v1 becomes an encrypted export and backup target. It is no
longer a sync channel.

## 8. Sync

The only sync channel is `easyssh-server`, which a user or a team runs
themselves (we may also offer a hosted one).

- **Envelope**: plain routing metadata, a version vector, the ciphertext,
  and the device's Ed25519 signature over all of it. Clients reject an
  envelope with a bad signature, an uncertified device or a version vector
  older than one they have already seen (rollback protection).
- **Merging happens on clients**, since the server cannot read records.
  Inside the ciphertext every field carries a stamp (Lamport clock and
  device id). When two version vectors are concurrent, records merge field
  by field; only edits to the same field fall back to last-writer-wins.
- **Nothing is lost silently**: a value that a merge overwrites goes into the
  record's history, kept locally and on the server, and can be restored in
  one click.
- **Push** is a compare-and-set on the version vector. If the server holds a
  concurrent version, it returns it; the client merges and pushes again.
- Deletes are signed tombstones, not server instructions.

## 9. Terminal

- **Splits and broadcast input** across panes and tabs.
- **Shell integration** is opt-in per host: a small script, sent only when
  enabled, reports prompt boundaries, the working directory, command start
  and exit codes through our own OSC sequence (`OSC 6973`, see
  `shell-integration/README.md`). bash, zsh and fish first; it degrades quietly
  on shells it does not know. The daemon delivers it, on the byte stream every
  engine produces, so russh needs nothing extra.
- **Completion** uses an open JSON spec format in a separate repository that
  anyone can contribute to, starting with about 50 frequent commands, plus
  history and remote paths read through the session's SFTP channel. Themes
  are contributed the same way.
- **AI** uses the user's own API key and provider. Terminal content leaves
  the device only when the user asks a question, and the request is shown
  before it is sent.
- **Session logs** are encrypted on disk. With a server they can be uploaded
  and shared through a vault.
- **Sessions survive restarts of the UI** because they live in the daemon.

## 10. Telemetry

Crash reports are asked for on first start and can be turned off.
Anonymous usage statistics are off unless the user turns them on. Terminal
content, host names and addresses are never collected.

## 11. Compatibility

Changing the storage format makes phase 3 release **2.0.0** under
[RELEASING.md](../RELEASING.md). Migration is automatic and keeps the v1
JSON file next to the database, so no user loses hosts. 2.x cannot be
downgraded to 1.x; the kept v1 file is the way back. Hosts that depend on OpenSSH-only features keep
working through the compatibility engine.

## 12. Phases

Each phase ends with a release; `main` stays releasable throughout. Old code
is replaced piece by piece behind the same UI, and every switch must pass the
end-to-end suite first.

| Phase | Weeks | Delivers |
|---|---|---|
| 0. Baseline | 1 | End-to-end tests in CI against an OpenSSH server container, on Linux only: terminal, SFTP, tunnels, password prompts, jump hosts. Run against v1 to record today's behaviour. Windows and macOS keep unit tests and the manual smoke test in RELEASING.md. |
| 1. Daemon | 3 | `easyssh-proto`, `easysshd`, authenticated IPC; today's PTY sessions moved behind the daemon (still system OpenSSH); UI reattaches after reload. |
| 2. Engine | 6 | russh engine behind a setting, OpenSSH fallback rules, one connection for terminal, files and tunnels. Done when russh passes the whole e2e suite; OpenSSH then becomes an advanced option. |
| 3. Store | 3 | Release 2.0. SQLite with v1 migration, known hosts store, importers (ssh_config, PuTTY, SecureCRT, MobaXterm, Xshell, FinalShell, Termius exports, CSV). |
| 4. Vault | 6 | Device key and keychain, passwords and private keys in the vault, Windows Hello, Secure Enclave, FIDO2, serial ports. |
| 5. Sync | 8 | `easyssh-server`, sign-in without sending the passphrase, signed envelopes, field-level merge, history, team vaults and roles. |
| 6. Terminal | 6 | Splits and broadcast, shell integration, completion specs, AI with the user's key, encrypted session logs and sharing. |
| 7. Mobile | 10+ | Tauri mobile app with the embedded engine. |

## 13. Open questions

To be decided at the start of the phase that needs them:

- SRP-6a or OPAQUE for server sign-in (phase 5).
- License of `easyssh-server` (MIT like the app, or AGPL to keep hosted forks
  open) (phase 5).
- Which hosted offering, if any, and its pricing (after phase 5).
- Serial port library and permissions on each platform (phase 4).
