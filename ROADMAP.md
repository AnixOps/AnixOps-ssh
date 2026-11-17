# Roadmap

Goal: an open-source, local-first SSH client that matches the best
commercial clients on everyday use and beats them on trust. Every version
ships a downloadable build.

The v2 direction (proposed 2026-11-17, waiting for approval) is described in
[docs/architecture-v2.md](docs/architecture-v2.md).

## Done

| Version | Theme | Scope |
|---|---|---|
| v0.5 | Foundation | Tauri 2 + React shell on AnixOps Design; host list; tabs; CI on three platforms |
| v0.6 | Embedded terminal | xterm.js tabs on a PTY running the system `ssh`; local shell tab |
| v0.7 | SFTP pane | Long-lived `sftp` per pane; transfers; password prompts through `SSH_ASKPASS` |
| v0.8 | Productivity | Host editor, snippets, port forwarding, command palette |
| v1.0 | Polish | Settings, SSH environment check, export, import and Git sync, user guide |
| v1.0.1 | Motion | Motion in the macOS and Linear style; browser preview site |

## What drives the work

In order of weight:

1. **Real servers in CI.** End-to-end tests against an OpenSSH server
   container: terminal, sftp, tunnels and password prompts. Until these
   exist, a green build only proves the code compiles and renders.
2. **Daily use.** The owner uses the app instead of their usual SSH client
   and files every rough edge as a GitHub issue. Started 2026-10-14; 1.1 is
   planned from these issues after two weeks.
3. **User issues**, with the version, OS and `ssh -V` attached.
4. **Comparison with Termius, Xshell and FinalShell**, for frequent tasks
   only.

Telemetry: crash reports are asked for on first start; anonymous usage
statistics only when the user turns them on. Terminal content, host names and
addresses are never collected.

## Next

Each phase ends with a release and `main` stays releasable. Old code is
replaced piece by piece, and every switch must pass the end-to-end suite
first. Details per phase are in
[docs/architecture-v2.md](docs/architecture-v2.md#12-phases).

| Phase | Version | Theme | Scope |
|---|---|---|---|
| 0 | v1.1 | Baseline | End-to-end tests against an OpenSSH server container on Linux, recorded against v1 |
| 1 | v1.2 | Daemon | JSON Schema contract, `easysshd` with authenticated local IPC, sessions survive UI reloads |
| 2 | v1.3 | Engine | russh engine with OpenSSH fallback; one connection for terminal, files and tunnels; OpenSSH becomes an advanced option once russh passes the whole e2e suite |
| 3 | v2.0 | Store | SQLite with v1 migration (done: hosts, groups, settings and known hosts now live in `easyssh.db`), importers from every common client (next) |
| 4 | v2.1 | Vault | Encrypted passwords and keys, Windows Hello, Secure Enclave, FIDO2, serial ports |
| 5 | v2.2 | Sync | Open-source self-hosted server, signed end-to-end encrypted sync, team vaults |
| 6 | v2.x | Terminal | Splits and broadcast, shell integration, completion specs, AI with your own key, encrypted logs |
| 7 | — | Mobile | Tauri mobile app with the embedded engine |

## Not doing

Our own hosted-only cloud (the server is always self-hostable and open
source), storing any secret where a server can read it, Mosh and Telnet,
CRDT-based sync, telemetry without consent, AI through our own servers.

How versions are numbered and shipped: [RELEASING.md](RELEASING.md).
