# easyssh-update

Update check for AnixOps SSH (MIT): check and notify, never install.

It reads the signed `anixops-appcast-v1` feed of AnixOps (source of truth:
`anix-kit/update`) and verifies it exactly as the kit does: Ed25519 over
`"anixops-appcast-v1\n"` + the base64-decoded payload, base64 raw public keys,
product name, sequence anti-replay, HTTPS-only items. Archive verification
(`Verifier::verify_archive`: size, SHA-256, signature over
`"anixops-update-archive-v1\n"` + product, build, size, digest) is there for a
later installer step. Everything fails closed.

- Products: `anixops-ssh-windows`, `anixops-ssh-macos`, `anixops-ssh-linux`.
- Feed URL: `https://github.com/AnixOps/EasySSH/releases/latest/download/<product>.appcast.json`,
  fetched with a plain `GET`; redirects only to `github.com`,
  `objects.githubusercontent.com`, `release-assets.githubusercontent.com`.
- Items order by `build` = `major*1000000 + minor*1000 + patch` of the app version
  (`build_from_version`), not by a semver comparison: that is the kit's rule.
- Trusted keys: build variable `EASYSSH_UPDATE_PUBLIC_KEYS` (comma separated
  base64). None valid: `configured()` is false and nothing is fetched.

`tests/fixtures/` holds a feed, item and archive produced by the kit's Go tools
(see its README) and the kit's own foreign-feed vector; the tests verify them
with this crate.
