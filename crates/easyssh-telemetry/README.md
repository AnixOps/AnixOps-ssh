# easyssh-telemetry

Opt-in crash reports and anonymous usage statistics for AnixOps SSH (MIT).
Inert unless the build names an endpoint **and** the user said yes.

## Build-time configuration

| Variable | Meaning |
|---|---|
| `EASYSSH_SENTRY_DSN` | `https://<key>@<host>/<project>` of a GlitchTip/Sentry project. Absent or invalid: crash reports are fully disabled and the UI says so. |
| `EASYSSH_USAGE_URL` | HTTPS URL that receives the daily usage `POST`. Absent: no usage statistics. |

`http://` is accepted only for `localhost`, `127.0.0.1` and `[::1]` (tests,
a server on the same machine).

## Pieces

- `scrub::scrub`: the single scrubber (table in its module docs; many unit tests).
  Its placeholders (`<url> <host> <ip> <email> <secret> <id>`) and core rules are the
  AnixOps shared redaction rule set (anix-kit `redact`); `<path>`, `<mac>` and
  `<account>` are stricter AnixOps SSH additions.
- `Report`: version, OS, arch, component (`app`/`daemon`/`ui`), kind, scrubbed
  message, symbol names. Scrubbed on construction and again when read back.
- `hook::install`/`set_capture`: panic hook that writes to the `Spool`; at most
  5 reports per process; does nothing while capture is off.
- `hook::capture_frontend`: `window.onerror` / `unhandledrejection` from the
  page, JS stacks reduced to function names, scrubbed.
- `Spool`: one JSON file per report; at most 20 files, 256 KiB in total, 16 KiB
  per report, 14 days; `list()` shows exactly what would be sent, `clear()`
  deletes it.
- `envelope` / `upload`: a hand-written Sentry envelope over HTTPS
  (`POST /api/<project>/envelope/`, `X-Sentry-Auth`), at most 5 reports per run
  and 10 per rolling day, back-off of one hour after any failure, HTTP 4xx
  (except 429) drops the report, everything else keeps it. Runs on a background
  thread; start-up never waits for it.
- `usage`: the daily ping.

## Why not the Sentry SDK

The `sentry` crate would bring its own transport, a second TLS and runtime
stack, default integrations (breadcrumbs, contexts, `server_name`, the
machine's host name, module list, environment) and a surface that collects
more by default than we are willing to send. Every one of those would have to
be switched off and audited on each upgrade. The envelope format for one
`event` item is three JSON lines; writing it by hand is about 60 lines, makes
"what is sent" exactly the code in `envelope.rs`, and lets "View pending
reports" show the bytes. The cost is that we do not get SDK features (release
health, sessions, breadcrumbs), none of which we want.

HTTP is `ureq` 2 with rustls and no redirects.

## Usage statistics: the complete field list

```json
{ "app_version": "1.0.2", "os": "linux", "arch": "x86_64",
  "engine": "openssh", "hosts": "6-20", "sessions": "1-5" }
```

`hosts` is the number of saved hosts and `sessions` the number of terminal
sessions opened on the last day the app was used, each as `0`, `1-5`, `6-20` or
`21+`. At most one ping per UTC day, checked at start. No identifier of any
kind is generated, stored or sent, so pings cannot be linked.

## Tests

`cargo test -p easyssh-telemetry`: scrubber table tests, envelope format and
consent gating against a mock HTTP server on `127.0.0.1` (nothing else is ever
contacted), spool caps, rate limit and back-off, DSN parsing.

### Shared redaction vectors

`tests/data/anixops-redact-vectors.json` is a vendored copy of
`redact/testdata/vectors.json` from `AnixOps/anix-kit` (its `_note` field names
the commit). `tests/redact_vectors.rs` runs every vector through `scrub` and
requires the exact output. To refresh, in CI or by hand:

```sh
git clone --depth 1 https://github.com/AnixOps/anix-kit /tmp/anix-kit
python3 - <<'PY'
import json, subprocess
src = json.load(open("/tmp/anix-kit/redact/testdata/vectors.json"))
sha = subprocess.check_output(["git", "-C", "/tmp/anix-kit", "rev-parse", "HEAD"], text=True).strip()
out = {"_note": f"Vendored copy. Source: AnixOps/anix-kit, redact/testdata/vectors.json at commit {sha}. Do not edit by hand; see the crate README for how to refresh."}
out.update(src)
open("crates/easyssh-telemetry/tests/data/anixops-redact-vectors.json", "w").write(json.dumps(out, indent=2, ensure_ascii=False) + "\n")
PY
cargo test -p easyssh-telemetry --test redact_vectors
```

A failing vector means the kit changed its rules: change `scrub.rs` to match.
