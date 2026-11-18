# Roadmap

Goal: an open-source, local-first SSH client that matches the best
commercial clients on everyday use and beats them on trust. Every version
ships a downloadable build.

The v2 direction (approved 2026-11-17, shipped in 2.0) is described in
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
| v2.0 | v2 architecture | Background service, built-in russh engine (default) with OpenSSH fallback, SQLite store and importers, encrypted vault with Windows Hello and Touch ID, end-to-end encrypted sync and team vaults, splits, shell integration, completion, AI with your own key, encrypted session logs, serial ports, AnixOps Design 1.2.0 |

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
addresses are never collected. Built: `easyssh-telemetry` (Sentry envelope to
a self-hosted GlitchTip, scrubber, consent, Privacy settings); see
[docs/telemetry.md](docs/telemetry.md).

## Next

Each phase ends with a release and `main` stays releasable. Old code is
replaced piece by piece, and every switch must pass the end-to-end suite
first. Details per phase are in
[docs/architecture-v2.md](docs/architecture-v2.md#12-phases).

| Phase | Version | Theme | Scope |
|---|---|---|---|
| 0 | v1.1 | Baseline | End-to-end tests against an OpenSSH server container on Linux, recorded against v1 |
| 1 | v1.2 | Daemon | JSON Schema contract, `easysshd` with authenticated local IPC, sessions survive UI reloads |
| 2 | v1.3 | Engine | russh engine with OpenSSH fallback; one connection for terminal, files and tunnels (done). The built-in engine now passes the whole end-to-end suite, including the daemon scenarios run on both engines, saved keys and passphrases from the vault and host keys in the store, so it is the default and OpenSSH is an advanced option in Settings that only gets fixes (done) |
| 3 | v2.0 | Store | SQLite with v1 migration (done: hosts, groups, settings and known hosts now live in `easyssh.db`), importers from every common client (done: OpenSSH config, PuTTY, SecureCRT, MobaXterm, Xshell, FinalShell, Termius and CSV, with preview, duplicate handling and an Import hosts dialog) |
| 4 | v2.1 | Vault | Encrypted passwords, keys and key passphrases with a device key in the OS keychain and an optional Argon2id vault passphrase, auto-lock, askpass answers from the vault (done, step 1); Windows Hello, Touch ID and serial ports (done); FIDO2 `sk-ed25519` / `sk-ecdsa` SSH keys on the built-in engine with touch and PIN prompts (done: Linux and macOS over USB HID, Windows through the Windows WebAuthn API for `sk-ecdsa` keys; `sk-ed25519` keys on Windows keep OpenSSH); FIDO2 vault unlock (next) |
| 5 | v2.2 | Sync | Open-source self-hosted server, signed end-to-end encrypted sync, team vaults (done in the app, step 1: sign up and in, personal sync across devices with field-level merge and restorable history, team vaults with roles and safety-number checks, Git sync is now "Backup to Git"; vault key rotation on member removal, dated device revocation, certificate expiry and a batch push are done; next: a hosted offer) |
| 6 | v2.x | Terminal | Splits and broadcast, shell integration, completion specs, AI with your own key (done: Anthropic, OpenAI-compatible and Ollama, review before send, secret scrubbing), encrypted logs (done: per-host recording, viewer, export, retention; sharing through a vault is a hook only) |
| 7 | — | Mobile | Tauri mobile app with the embedded engine. Viability prototype done: engine crates build for Android and iOS (`easyssh-engine`), CI builds an Android debug APK and an iOS simulator app, a touch UI has the host list and a terminal with a key bar. Recommendation: commit, Android first, after one device test; evidence and blockers in [docs/mobile-prototype.md](docs/mobile-prototype.md). Vault on mobile done (compiled in CI, not yet run on a device): Android Keystore and iOS Keychain key stores, biometric unlock bound to the sensor with the passphrase as fallback, saved passwords and keys, lock on background, first-run flow and vault settings. Terminal experience done (Playwright touch tests in CI, not yet run on a device): IME and soft-keyboard handling, full and customisable key bar, touch selection with handles and copy/paste, one- and two-finger scrolling, pinch font size, hardware shortcuts on tablets, background policy with clean close, tmux/screen attach with automatic reconnect, optional Android foreground notification, accessibility pass |

## Not doing

Our own hosted-only cloud (the server is always self-hostable and open
source), storing any secret where a server can read it, Mosh and Telnet,
CRDT-based sync, telemetry without consent, AI through our own servers.

How versions are numbered and shipped: [RELEASING.md](RELEASING.md).
