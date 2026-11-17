# easysshd

The EasySSH desktop daemon's local IPC transport. This crate is the library
(`Server`, `Client`) and a binary (`easysshd [--runtime-dir <path>]`) that
answers `{"type":"ping"}` with `{"type":"pong"}`. Payloads are opaque JSON for
now.

## Threat model

Protects against:

- **Other local users** on the same machine. The runtime directory is `0700`,
  the socket and secret `0600`, all owned by the current user; the daemon
  refuses to start in a directory with looser permissions or another owner,
  and the client refuses a secret file that is not private. On Windows the
  named pipe and the runtime directory carry a protected ACL granting only the
  current user, and remote pipe clients are rejected.
- **Local processes of any user that do not hold the secret.** Every connection
  must prove knowledge of a 32-byte per-launch secret, and every frame in both
  directions carries an HMAC. Tampering, replay, reordering and gaps close the
  connection.
- A **rogue server** squatting the endpoint: the client does not send payloads
  until the server's first frame authenticates.

Does **not** protect against:

- **Malware running as the same user.** It can read the secret file (or the
  UI's memory) and speak to the daemon like the UI does. Same-user isolation
  needs OS sandboxing, which is out of scope here.
- A **root/Administrator** attacker.
- **Eavesdropping**: frames are authenticated, not encrypted. This is
  acceptable only because the channel never leaves the machine and is not
  readable by other users.
- Denial of service by the same user (connections are capped at 64 and
  handshakes time out after 5 s, but nothing more).

## Wire format

Framing: `u32` big-endian length, then the body. Maximum 4 MiB (1 KiB before
authentication); the length is checked before any allocation.

Handshake:

1. client: `{"hello":1,"client_nonce":b64(32 bytes)}`
2. server: `{"server_nonce":b64(32 bytes)}`
3. both derive `session_key = HMAC-SHA256(secret, "easysshd-ipc-v1" || client_nonce || server_nonce)`
4. server: first authenticated frame `{"ready":true}` (lets the client verify
   the server before sending anything; consumed by `Client::connect`)
5. client: first authenticated frame. If it does not verify the server closes.

Every authenticated frame body is `seq (u64 BE) || mac (32 bytes) || json`,
`mac = HMAC-SHA256(session_key, direction || seq || json)` with direction
`0x01` client to server and `0x02` server to client. `seq` starts at 0 and
rises by exactly 1 per direction. MACs are compared in constant time.

## Endpoint and files

| Item | Unix | Windows |
|---|---|---|
| Runtime dir (default) | `$XDG_RUNTIME_DIR/easyssh`, else `$TMPDIR/easyssh-<uid>` | `%LOCALAPPDATA%\EasySSH\run` |
| Endpoint | `<dir>/easysshd.sock` | `\\.\pipe\easysshd-<hash of dir>` |
| Secret | `<dir>/easysshd.secret` | same |
| Instance lock | `<dir>/easysshd.lock` | same |

On Windows the named pipe uses tokio's `named_pipe` with a
`windows-sys`-built security descriptor (`D:P(A;;GA;;;<current user SID>)`),
which avoids a heavier dependency for a handful of calls.

Unix socket paths must fit `sun_path` (107 bytes on Linux, 103 on macOS);
longer paths fail with an explicit error.

The files are removed on clean shutdown (Ctrl+C or SIGTERM). After a crash the
next start replaces the stale socket and secret once it holds the lock.
