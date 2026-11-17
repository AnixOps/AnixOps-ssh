# easysshd

The EasySSH desktop daemon: the authenticated local IPC transport (`Server`,
`Client`) and the services on top of it. Terminal sessions live here, so they
survive a reload or close of the UI window. The binary
(`easysshd [--runtime-dir <path>] [--config <connections.json>] [--idle-timeout-secs <n>]`)
serves the `easyssh-proto` command set; the app starts the same code from its
own executable with the hidden `--easysshd` flag.

## Services

Every message after the handshake is an `easyssh-proto` `Command`; anything
that fails `validate_command` is answered with a `ProtoError` (`invalid_command`,
or `unknown_command` for an unknown kind) and the connection stays open.

- `proto.hello`, `hosts.list|save|delete` (the same `connections.json` as the
  v1 app, same validation).
- `session.open|write|resize|close`: a PTY running the system `ssh` exactly as
  v1 builds it (or the local shell for a null host). `session.list`,
  `session.attach {session_id, from_seq?}` and `session.detach` add reattach.
- Output is UTF-8 text; every byte has an offset in the session's stream.
  `session.output` events carry it as `seq`. Each session keeps the last 1 MiB
  (ring buffer). `session.attach` replies, then replays the buffer from
  `from_seq`, then `session.exit` if the session already ended, then streams
  live, atomically, so a client sees neither gap nor duplicate. Several clients
  can attach; output fans out. Ended sessions stay (with their scrollback)
  until `session.close`.
- `askpass.prompt` / `askpass.reply` / `askpass.endpoint`: OpenSSH's
  `SSH_ASKPASS` helper (the app executable in helper mode) talks to a
  loopback listener guarded by a random token; the daemon forwards prompts to
  every attached client. Answers are held in memory only until handed to the
  helper, and are never stored or logged.
- A client that stops reading (4096 queued frames) is disconnected rather than
  buffered without bound; it can reconnect and replay with `from_seq`.
- The daemon exits after 10 minutes with no clients and no running sessions.

`link` (the app side: request/reply, per-session sinks, ordered fire-and-forget
`post`) and `launch` (connect, or start detached and retry) are toolkit-free so
the Tauri layer stays thin.

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
