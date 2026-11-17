# Telemetry

AnixOps SSH can send two kinds of data, both opt-in, both described here
completely. Architecture background: [architecture-v2.md](architecture-v2.md) §10.

## What is never collected

Terminal content, host names, addresses, user names, file paths, command
lines, keys, passwords, host notes, snippets. The scrubber (below) removes
anything that looks like these from the few free-text fields that exist, and
reports never contain arguments, environment variables or file locations.

## Crash reports

- **Asked once**, on first start, by a small card in the corner that does not
  block the app (two equal buttons, no default). Changeable any time in
  Settings › Privacy. Off until answered.
- **Stored locally first.** A panic in the app or the daemon, or an uncaught
  error in the page, writes one JSON file to
  `<local data dir>/EasySSH/crash-reports/` (Windows `%LOCALAPPDATA%`, macOS
  `~/Library/Application Support`, Linux `~/.local/share`). Nothing is written
  while crash reports are off.
- **Sent on the next start**, on a background thread, only if consent is on and
  the build has a DSN. Offline or failing uploads keep the report and back off
  for an hour; at most 5 reports per start and 10 per day leave; the spool holds
  at most 20 reports, 256 KiB, 14 days.
- **Visible.** Settings › Privacy › View pending reports shows the exact
  envelope text that would be posted. Delete pending reports removes them.
  Turning crash reports off also deletes what was waiting.

Fields of one report (a Sentry `event`):

| Field | Content |
|---|---|
| `event_id`, `timestamp` | random 32-hex id, unix time |
| `release` | `anixops-ssh@<version>` |
| `tags.os`, `tags.arch`, `tags.component` | e.g. `linux`, `x86_64`, `app` / `daemon` / `ui` |
| `exception.type` | `panic`, `error` or `unhandledrejection` |
| `exception.value` | the message, scrubbed, at most 500 characters |
| `exception.stacktrace.frames[].function` | symbol names, at most 48, no file names or line numbers |
| `sdk` | `easyssh-telemetry` and its version |

Not sent: `server_name`, user, request, modules, environment, breadcrumbs,
file paths. The HTTP request necessarily reveals the sender's IP address to
the server; configure the GlitchTip project to not store it (see below).

### The scrubber

One function, `easyssh_telemetry::scrub::scrub`, applied when a report is
built and again whenever one is read back. It replaces: URLs, e-mail
addresses, `user@host`, dotted host names, IPv4 and IPv6 addresses, MAC
addresses, drive and UNC paths (including `C:\Users\<name>`), home and other
absolute Unix paths, `~/...`, hexadecimal strings of 16 or more digits and
base64-like strings of 20 or more characters containing a digit. Known limit:
a single-label name without a dot (`prod-db`) in a free-text message cannot be
told from a word; messages come from panics and error objects, which do not
embed user data in normal operation.

## Anonymous usage statistics

Off by default; needs a build with `EASYSSH_USAGE_URL`. When on, at most one
`POST` of this JSON per UTC day (checked at start):

```json
{ "app_version": "1.0.2", "os": "linux", "arch": "x86_64",
  "engine": "openssh", "hosts": "6-20", "sessions": "1-5" }
```

`hosts` and `sessions` are buckets `0`, `1-5`, `6-20`, `21+`. `sessions` is the
count of sessions opened on the last day the app was used. No identifier is
created or stored; the server sees an IP address as for any request and should
not log it.

## Build configuration

| Variable (build time) | Effect |
|---|---|
| `EASYSSH_SENTRY_DSN` | Enables crash reports; must be `https://` (or loopback `http://`). |
| `EASYSSH_USAGE_URL` | Enables the usage ping; same rule. |

Without them (a build from source, CI builds) nothing is collected, nothing
is sent, and Settings › Privacy says so. Release builds read both from the
repository secrets of the same names.

## Self-hosting GlitchTip

1. Run GlitchTip with its official Docker Compose file
   (<https://glitchtip.com/documentation/install>), behind HTTPS (a reverse
   proxy such as Caddy or nginx with a real certificate).
2. In GlitchTip create an organization and a project (platform "Other"). The
   project's *DSN* looks like `https://<key>@glitchtip.example.org/<id>`.
3. In the organization settings turn off storing client IP addresses, and set
   a retention you are comfortable with (for example 30 days).
4. Build with `EASYSSH_SENTRY_DSN=https://<key>@glitchtip.example.org/<id>`
   (for releases: add it as the repository secret `EASYSSH_SENTRY_DSN`).
5. Check: set `crash_reports` on in a development build and trigger a panic,
   or `curl` the envelope shown by "View pending reports" to
   `https://glitchtip.example.org/api/<id>/envelope/` with the header
   `X-Sentry-Auth: Sentry sentry_version=7, sentry_key=<key>`.

The public key in a DSN only allows submitting events; it is not a secret.

## For contributors

- All free text enters through `Report::new`; do not add fields that carry
  other data. Extend the scrubber table and tests when adding patterns.
- Tests never contact a real server: `cargo test -p easyssh-telemetry` uses a
  mock on `127.0.0.1`.
