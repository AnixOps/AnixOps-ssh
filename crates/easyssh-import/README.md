# easyssh-import

Parses other SSH clients' exports into a neutral host list. A pure library:
it takes bytes (and, for OpenSSH `Include`, a base directory), never reads the
user's own files by itself, and never connects anywhere. UI and storage wiring
belong to the caller.

```rust
let report = easyssh_import::import_auto(&bytes);          // detect + parse
let report = easyssh_import::putty::parse(&bytes);         // or pick a format
let batch  = report.to_entities();                         // easyssh_core Connection + Group
```

Every format module has `detect(&[u8]) -> bool` and `parse(&[u8]) -> ImportReport`.
`detect_format` / `import_as` / `import_auto` / `import_auto_in(bytes, base_dir)`
dispatch over them (specific markers first, OpenSSH syntax last).

## Output

`ImportReport { hosts, groups, warnings, skipped }`

- `ImportedHost { label, hostname, port, username, group_path, identity_file, jump, openssh_only, notes }`
  - `group_path`: folders from the root; `groups` lists every path used (and
    empty folders where the format has them), parents before children.
  - `jump`: labels or aliases of jump hosts in order; not resolved to ids.
  - `openssh_only`: reasons the host needs the OpenSSH engine.
- `ImportWarning { line, record, message }`: problems that did not stop the import.
- `skipped`: records recognised but not imported (non-SSH sessions, rejected hosts).

Hosts are rejected (counted in `skipped`, with a warning) when the hostname is
empty, contains whitespace or control characters, starts with `-`, the port is
0, or the username starts with `-`, so an imported value can never be read as
an ssh option.

**Secrets.** Passwords, passphrases and key material have no field in the model.
Importers do not read them; when a source holds one, a warning says it was not
imported. Key *file paths* are kept (`identity_file`) but the file is never opened.

## Mapping to entities

`ImportReport::to_entities()` returns `EntityBatch { groups, connections }`
of `easyssh_core::Group` / `Connection` (the types `easyssh-store` persists).
Ids are fresh UUIDs; deduplication against existing hosts is up to the caller.

| import | entity |
|---|---|
| `label` | `Connection.name` |
| `hostname`, `port`, `username` | `ConnectionTarget::Endpoint` |
| `group_path` | `Group` chain via `parent_id`, `Connection.group_id` |
| `jump` | `proxy_jump`, comma-joined |
| `openssh_only` | tag `openssh-only`; reasons appended to `notes` |
| `identity_file` | appended to `notes` (`Connection` has no key path field) |

## Formats

### OpenSSH `~/.ssh/config` (`openssh`)

- Keys are case-insensitive; `Key value`, `Key=value`, `Key = value`; double
  quotes; `#` comment lines and trailing comments at a token start.
- One host per concrete `Host` alias. Patterns containing `*` or `?` are never
  hosts, but supply defaults. Options follow ssh_config first-match-wins in
  file order across all matching blocks (`!negation` supported, matching is
  case-insensitive); `IdentityFile` accumulates (the first becomes
  `identity_file`, the rest go to `notes`). `HostName` defaults to the alias
  (`%h` expands). Options before any `Host` apply to every host.
- `ProxyJump a,b` becomes `jump` (hops kept as written, e.g. `user@host:2200`).
- `openssh_only`: `ProxyCommand`, `LocalCommand`, `PermitLocalCommand yes`,
  `KnownHostsCommand`, `Ciphers`, `MACs`, `KexAlgorithms`, `HostKeyAlgorithms`,
  `PubkeyAcceptedAlgorithms`/`PubkeyAcceptedKeyTypes`, `HostbasedAcceptedAlgorithms`/
  `HostbasedKeyTypes`, `CASignatureAlgorithms`, `PKCS11Provider`, and `Match`.
- `LocalForward`/`RemoteForward`/`DynamicForward`/`RemoteCommand` are copied to `notes`.
- `Include`: `parse_with_base(bytes, base_dir)` resolves relative paths against
  `base_dir` (OpenSSH uses `~/.ssh`; a leading `~/.ssh/` is read as `base_dir`).
  Depth is capped at 8, at most 512 files of at most 4 MiB are read, and
  included files must resolve (symlinks followed) inside `base_dir`; other
  paths are skipped with a warning. Wildcards work in the last path component
  only (`config.d/*.conf`). An included file starts in the including block's
  context, and its `Host` lines do not leak back. Without a base dir, `Include`
  is reported and skipped.
- Limitations: `Match` is not evaluated. Its options are ignored and every host
  is marked `Match`, since any host might be affected. `Host` groups are not
  inferred. Unknown options are ignored silently. Tokens other than `%h` are not expanded.

### PuTTY (`putty`)

`.reg` export of `HKEY_CURRENT_USER\Software\SimonTatham\PuTTY\Sessions`
(UTF-8 or UTF-16 with BOM). Session names are percent-decoded; a `/` in the
name is read as a folder separator (PuTTY forks and managers use this).
Fields: `HostName` (`user@host` is split, and that user wins), `PortNumber`
(dword), `UserName`, `Protocol` (only `ssh`; missing means ssh), `PublicKeyFile`.
`ProxyMethod` 4 or 5 (command proxies) marks `ProxyCommand`; SOCKS/HTTP proxies
are reported and dropped. `Default Settings` is skipped. `ProxyPassword` is
never imported.

### SecureCRT (`securecrt`)

XML export. Keys under `Sessions` are folders; a key with a `Hostname` string is
a session. Fields: `Hostname`, `Username`, `Protocol Name` (must start with
`SSH`), `[SSH2] Port` / `[SSH1] Port` / `Port` (dword, **hexadecimal**),
`Identity Filename V2`, `Firewall Name` (`Session:<name>` becomes a jump host).
The `Default` template is skipped. Saved passwords are never imported.
A XML error stops reading and keeps what was parsed.

### MobaXterm (`mobaxterm`)

`.mxtsessions` INI. `[Bookmarks*]` sections; `SubRep` is the folder path
(`\`-separated). Each other key is a session named by the key, with value
`#icon#fields#...`; `fields` is `%`-separated. **Positions relied on:** 0 =
session type (only `0`, SSH), 1 = host, 2 = port (empty or `0` means 22),
3 = username, 14 = private key path (best effort). Everything else (terminal
colours, X11, gateway settings) is ignored. `[Passwords]`/`[Credentials]`
sections are never read. The format is undocumented; positions come from
observed exports.

### Xshell (`xshell`)

`.xsh` INI files (UTF-16LE or UTF-8), or a zip of `.xsh` files (`.xts` is
such a zip); folders in the zip become groups. Fields: `[CONNECTION]` `Host`,
`Port`, `Protocol` (only SSH); `[CONNECTION:AUTHENTICATION]` `UserName`,
`UserKey` (a key name in Xshell's store, kept in `notes`). The label is the file
name; use `parse_named(name, bytes)` for a lone file (otherwise the host is the
label). `Password` and `Passphrase` are never imported. Zip reads are bounded
(10,000 entries, 1 MiB per entry, 64 MiB total). Encrypted archives are not supported.

### FinalShell (`finalshell`)

**Best effort**, undocumented format. One JSON object, an array of objects, or
a zip of `.json` files (e.g. a zipped `conn/` directory). An object with a
non-empty `host` is a connection (`name`, `host`, `port` as number or string,
`user_name`, `description`, `jump_server_id` resolved against other objects in
the input, `private_key_id` noted); objects without `host` are folders linked
by `id`/`parent_id`. Connection types are not filtered, so non-SSH entries may
appear as hosts. `password` and key fields are never imported.

### Termius (`termius`)

**Best effort**: Termius publishes no export specification. CSV follows its
import template (`Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,Password,SSH Key`;
`Groups` is `/`-separated; non-`ssh` protocols are skipped; tags go to `notes`).
JSON expects `{"hosts": [...], "groups": [...]}` (or a bare host array) shaped
like the account API objects (`label`, `address`, `group`, `ssh_config.port`,
`ssh_config.username` or `ssh_config.identity.username`, `ssh_config.host_chain.host_ids`
for jump hosts). Passwords and keys are never imported. Expect to adjust this
against real exports.

### CSV (`csv_format`)

EasySSH's own format; the header row is required, columns are case-insensitive
and may be in any order:

```
label,hostname,port,username,group,identity_file,jump
web1,web1.example.com,2222,alice,Prod/Web,~/.ssh/web1,bastion
v6,2001:db8::2,22,,,,bastion;web1
```

`hostname` is required; `label` defaults to the hostname; `port` defaults to 22;
`group` is a `/`-separated path; `jump` is `;`-separated. Columns `password`,
`passphrase`, `private_key` are not imported. Fields follow RFC 4180 quoting.

## Robustness

No parser panics on arbitrary input (see `tests/fuzz.rs`: random bytes,
structured noise, mutated fixtures, hostile zips, deep nesting). Malformed
records produce warnings and, when no host can be made, a `skipped` count.
Tests use only the synthetic fixtures in `tests/fixtures` (example.com,
`192.0.2.x`, `2001:db8::`).
