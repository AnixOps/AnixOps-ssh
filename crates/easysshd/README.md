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
  v1 app, same validation). Every successful `hosts.save` or `hosts.delete`
  broadcasts a `hosts.changed` event to all connected clients, so each window
  refreshes its list. `hosts.save` may carry a `group` to create with the host.
- `session.open|write|resize|close`: a PTY running the system `ssh` exactly as
  v1 builds it (or the local shell for a null host). `session.list`,
  `session.attach {session_id, from_seq?}` and `session.detach` add reattach.
- `shell_integration` on a saved host (opt-in, default off): once the session
  shows a shell prompt, the daemon types the scripts of `shell-integration/`
  (leading space, hidden from the output and the scrollback, never into an
  authentication prompt or after the user typed) and the shell reports prompts,
  command lines, exit codes and the working directory as `OSC 6973`. The daemon,
  not the UI, does this so it works for every engine and survives UI reloads;
  see `src/integration.rs` and `shell-integration/README.md`.
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
  helper, and are never stored or logged. When a prompt is answered,
  cancelled or times out, every client gets `askpass.resolved {id}` and closes
  its dialog.
- SSH engine: with `ssh_engine = russh` in the config (or `engine: russh` on
  a host) the daemon runs the session on the built-in engine
  (`easyssh-ssh`) instead of a PTY running `ssh`. Terminal, files
  (`sftp.*`, `transfer.*`) and tunnels (`tunnel.*`) of one host share one SSH
  connection, kept for 30 seconds after the last user left (`engine.connections`
  shows what is open). The system OpenSSH still runs a host that needs
  `ProxyCommand`, `Match`, `LocalCommand`, certificates, security keys, a
  stored remote command or inline forwards, or when the built-in engine
  reports `unsupported` while connecting; `session.open` answers with the
  `engine` and the `engine_reason`. Passwords, passphrases and
  keyboard-interactive prompts reuse `askpass.prompt` (with `kind`), and host
  keys use `hostkey.prompt`/`hostkey.reply`/`hostkey.resolved`. Host keys are
  recorded in a `known_hosts` file beside the host list, with
  `~/.ssh/known_hosts` read as a fallback. A changed key is refused unless the
  client answers `replace_and_save`.
- A client that stops reading (4096 queued frames) is disconnected rather than
  buffered without bound; it can reconnect and replay with `from_seq`.
- The daemon exits after 10 minutes with no clients and no running sessions,
  or as soon as no session runs once it was asked to drain
  (`daemon.shutdown_when_idle`).

`link` (the app side: request/reply, per-session sinks, ordered fire-and-forget
`post`), `launch` (connect, or start detached and retry) and `supervisor`
(below) are toolkit-free so the Tauri layer stays thin.

## Reconnecting

The app's `Supervisor` owns the `Link`. When the connection drops it retries
after 0.5 s, 1 s, 2 s, ... up to 10 s, starting the daemon again if none runs
(`launch::connect_or_spawn_with`). Every session the app is attached to is
tracked with the offset after the last byte its consumer saw; on reconnect the
supervisor sends `session.attach {from_seq}` for each, so output printed
meanwhile is replayed exactly once, with no gap and no duplicate (unless the
1 MiB buffer wrapped, which `SessionAttachResult.truncated` reports). Sinks see
`SessionMsg::Reconnecting`, then `Resumed`. A daemon that restarted knows none
of the sessions (their processes died with it): their sinks get `Exit` and the
tab shows the session as ended. While reconnecting, the app uses its
in-process paths for hosts and new sessions.

## Versions

`proto.hello` carries the client's `schema_version` and its `app_version`
(build); the reply carries the daemon's own and whether it is `draining`.

- **Schema mismatch.** A daemon answers a client that speaks another schema
  with the error `schema_mismatch` naming both versions and does nothing else.
  If the client is *newer* the daemon is the stale one: it also drains, so it
  exits once its sessions end and the next app start brings up a matching
  daemon. (A daemon cannot serve two schemas, and the schema changes only for
  breaking changes.)
- **Build mismatch** (same schema, other app build, for example after an
  update while sessions were running). The app sends `daemon.shutdown_when_idle`
  to the old daemon. The old daemon refuses new sessions (`draining`) and
  exits as soon as no session is running, even if clients stay connected.
  With no sessions running it is gone within a moment and the app starts the
  matching daemon at once. With sessions running, the app stays attached to
  the old daemon for those, opens new sessions in-process meanwhile, and its
  reconnect loop starts the matching daemon when the old one has left. There
  is deliberately no second daemon beside the old one: both would need the
  same runtime directory, instance lock and secret.
- A daemon older than this protocol (no `daemon.shutdown_when_idle`) is simply
  used as it is until it exits when idle.

## Agent environment

`session.open` may carry the client's current `ssh_auth_sock`. The daemon
outlives logins, so its own `SSH_AUTH_SOCK` can name an agent that is gone.
The value is applied to the session's environment only when it is an absolute
path to a socket owned by the daemon's user (symlinks are followed); otherwise
it is ignored with a log line and the session starts with the daemon's own
environment. The app's `Link` sends its own `SSH_AUTH_SOCK` on Unix. Windows
sends and applies nothing: the OpenSSH agent is a fixed named pipe.

## Log

`easysshd.log` in the runtime directory (owner-only), rotated at 1 MiB, three
files kept (`easysshd.log`, `.1`, `.2`). It records start and stop, why the
daemon stopped, client connections, the kind of each command with the session
id it names (not `session.write` or `session.resize`), session exits with their
exit codes, error codes and ignored `ssh_auth_sock` values. It never records
payloads, terminal input or output, askpass prompts or answers, host names or
error messages that could quote what a client sent. Every entry is one line.

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
