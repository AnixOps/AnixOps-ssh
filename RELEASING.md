# Releasing

## Versions

Semantic versioning:

- **Patch** `1.0.x`: fixes only. Ship whenever a fix is ready.
- **Minor** `1.x.0`: features. About every four to six weeks.
- **Major** `2.0.0`: only if a saved configuration could no longer be read.
  Configuration changes are additive with defaults, so this should not
  happen; a user must never lose hosts to an upgrade.

## Rules for `main`

- `main` is always releasable. Changes land through pull requests.
- Branch protection requires the CI checks (frontend and screenshots, Rust
  on Windows, macOS and Linux) before merging.
- One feature per pull request, squash-merged, branch deleted afterwards.

## Shipping a version

1. Open a pull request that bumps the version in `Cargo.toml` (workspace),
   `package.json`, `src-tauri/tauri.conf.json`, and the two lockfiles.
2. When CI is green, run the smoke test below on one real machine against a
   real server, using the installers the pull request's CI uploaded
   (`AnixOps-SSH-<OS>` artifacts). Merging is the release, so test first.
   For a patch release, the checks touching the fix are enough.
3. Merge. `tag.yml` sees the new version on `main`, checks that the three
   files agree, tags the merge commit `vX.Y.Z` and runs `release.yml`, which
   builds the installers on all three platforms, signs macOS when the Apple
   secrets exist ([docs/signing.md](docs/signing.md)) and publishes the
   release. Nobody tags by hand. If a version's tag already exists, nothing
   happens; Actions → Tag release → Run workflow retries a missed run.
4. Watch the issues for a few days. A regression gets a patch release, not a
   revert of the tag.

If a release build fails for an external reason (a download returning 500,
a runner outage), re-run the failed job; do not change the code.

## Smoke test (about ten minutes)

Install the build from the CI run or release, then:

- [ ] The app starts; Settings → SSH environment shows OpenSSH and the agent.
- [ ] A host from `~/.ssh/config` and a saved host both open a terminal.
- [ ] A password-only host asks for the password in the terminal.
- [ ] `htop` or `vim` fills the window and follows a resize; Chinese text renders.
- [ ] Ctrl+Shift+C / V copy and paste; Ctrl+Shift+F finds text.
- [ ] `exit`, then Reconnect works.
- [ ] The file pane lists the home folder; upload by drag and drop and a download both finish.
- [ ] On a password-only host the file pane asks in a dialog.
- [ ] A local tunnel turns Running and its port answers; turning it off frees the port.
- [ ] Ctrl+Shift+K finds a host and opens it.
- [ ] Quitting the app leaves no `ssh` or `sftp` processes behind.
- [ ] Windows or macOS: with a passphrase set, enable Windows Hello or Touch ID in Settings, Vault; lock, then unlock with it, and cancel the prompt once (the passphrase still works).
- [ ] With a USB serial adapter (or a loopback), a saved serial host opens from the new-tab menu, types and echoes; on Linux a port without access shows the `dialout` hint.
