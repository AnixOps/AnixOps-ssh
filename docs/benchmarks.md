# Benchmarks

Reproducible performance and stability numbers for the built-in SSH engine
(`crates/easyssh-ssh`), measured against a real OpenSSH server in a container
and, where stated, against the OpenSSH client. The targets come from the
evaluation report. Results are reported, not gated, until the numbers have been
stable for a few releases.

The harness is `crates/easyssh-e2e/examples/bench.rs`; the statistics and the
reports are in `crates/easyssh-e2e/src/bench.rs` (unit tested without Docker).
The workflow is `.github/workflows/bench.yml`.

## Metrics

All times are wall-clock, measured by the harness on the client.

| Id | What | How it is measured | Target |
| --- | --- | --- | --- |
| M1 | Connection success rate | N serial, independent connections (a new TCP connection and a new `Session` each, nothing shared) to a trusted host with a valid key. Success = the reply `ready-<n>` of `exec echo ready-<n>` arrives. A connection that has no reply after 10 s, or fails, is a failure. | successes / N >= 99.5% |
| M2 | Cold connect ready time | From just before `Session::connect` to the probe reply, for the successes of the M1 loop (so M1 and M2 share one run). Median and P95; timeouts are counted separately. | P95 <= 1.5 s |
| M3 | Keystroke echo latency | **Engine-level approximation, not the UI.** On an open PTY shell running `cat`, one byte is written to the channel and the time until the echoed byte arrives is taken (500 samples; median and P95). It contains the network and the server's tty, not terminal rendering or the browser. Nightly and release only. | report only |
| M4 | Stability | 25 interactive PTY sessions held open together, each answering a heartbeat command (`echo`) every 30 s. A session that ends, fails to write, or does not answer within 15 s is one unexpected disconnect (it is then reconnected). Result: disconnects per 100 session-hours. Nightly only, default 60 min (`BENCH_SOAK_MINUTES`); the evaluation report's 8 h can be run by hand. | report only |
| M5 | Reconnect after an outage | Runs the network through 100% loss (both directions) for 10 s, restores it, then opens new sessions (attempt timeout 4 s, retry after 200 ms) until one answers the probe. Recovered = a reply within 10 s after the network returned. 30 runs. Whether the old session survived the outage is recorded for information. This is the engine level (`Session::connect`); the app's auto-reconnect is built on it. | >= 29 of 30 |
| M6 | SFTP throughput | A random (incompressible) file; upload and download 5 times each with the engine (`Sftp::upload`/`download`, connection included) and with OpenSSH `sftp -b` (same key, cipher and kex, `BatchMode`), alternating so drift hits both. Throughput = bytes / wall time from start to confirmed completion; median per tool and direction; ratio = engine / OpenSSH. SHA-256 of every transferred file is compared with the source; a mismatch fails the run. | ratio >= 85% each way |

P95 is the nearest-rank percentile: sort ascending, take the sample at 1-based
rank `ceil(0.95 * n)`. The median of an even count is the mean of the two middle
samples.

## Conditions

Server: Debian bookworm OpenSSH from `e2e/Dockerfile`, as the e2e tests, in the
container `easyssh-e2e-bench` (`e2e/docker-compose.bench.yml`, port 2224).
Its cipher and key exchange are pinned (`chacha20-poly1305@openssh.com`,
`curve25519-sha256`; `BENCH_CIPHER`, `BENCH_KEX`), so the engine and OpenSSH
must use the same algorithms. Both clients also run on the same host as the
Docker daemon; the report records the CPU model, kernel, `ssh -V`, the server
banner and the commit.

Two network profiles, applied by `e2e/netem.sh` with `tc netem` in the
container's network namespace, on its interface for what it sends and on an
`ifb` device (ingress redirect) for what it receives:

| Profile | RTT | Rate | Loss | netem arguments (each direction) |
| --- | --- | --- | --- | --- |
| normal | 20 ms | 100 Mbit/s each way | 0 | `delay 10ms rate 100mbit limit 20000` |
| weak | 150 ms | not limited | 1% each way | `delay 75ms loss 1% limit 20000` |

netem's own `rate` option stands in for a separate `tbf`. The harness connects
to the container's bridge address (not Docker's published port, whose userspace
proxy would hide loss from TCP). The outage is the profile `down` (`loss 100%`).
`netem.sh` needs root (it uses `sudo`), `nsenter`, `tc`, `ip` and the `ifb`
module; the container needs no extra capability.

The targets apply to the **normal** profile. The weak profile is reported
without pass/fail, and its M6 file is smaller (`BENCH_SFTP_MIB_WEAK`, 32 MiB):
at 150 ms and 1% loss a single TCP stream moves a few Mbit/s, so 1 GiB would
take hours. Report values, not the targets, are what the weak profile is for.

## What runs when

| Trigger | Profiles | Metrics | Sizes |
| --- | --- | --- | --- |
| Pull request touching `crates/easyssh-ssh`, `crates/easyssh-e2e`, `e2e/` or `bench.yml` | normal | M1, M2, M5, M6 | 200 connections, 10 outages, 256 MiB |
| Nightly (02:17 UTC) and manual run | normal, weak | M1, M2, M3, M5, M6 | 1000 connections, 30 outages, 1 GiB (32 MiB weak) |
| Nightly and manual run, job "Stability" | normal | M4 | 25 sessions x `soak_minutes` (default 60) |
| Release (`release.yml`) | normal, weak | M1, M2, M3, M5, M6 | as nightly |

On a pull request the result is in the job summary and the artifact
`bench-results` (`bench.json`, `bench-report.md`). It does **not** fail the pull
request for a missed target; it fails for a harness error (server unreachable,
`tc` failed, a transfer failed) and for a SHA-256 mismatch. Set `BENCH_ENFORCE=1`
to also fail on a miss.

On a release, `bench.yml` runs for the tag next to the installer builds. It is
not a requirement of publishing, so a failure or a long run never holds the
release back. When it succeeds, `bench-report.md` and `bench.json` are attached
to the GitHub release and a short "Benchmarks" block (the normal profile) is
appended to its notes.

## Run it locally

Needs Docker, Rust, the OpenSSH client (`ssh`, `sftp`), and for the profiles
and M5 root with `tc`, `nsenter` and `sudo modprobe ifb`. Do not run the
profiles on a machine whose Docker network you care about: they only touch the
bench container's namespace, and `netem.sh <container> off` removes them.

```sh
e2e/prepare.sh
docker compose -f e2e/docker-compose.bench.yml up -d --build
export BENCH_HOST=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' easyssh-e2e-bench) BENCH_PORT=22
export BENCH_NETEM=$PWD/e2e/netem.sh             # omit to measure the unshaped network (M5 is then skipped)
cargo run --release -p easyssh-e2e --example bench -- --profiles normal --metrics m1,m2,m5,m6 --out bench-out
cargo run --release -p easyssh-e2e --example bench -- --profiles normal --metrics m4 --out bench-out-soak
docker compose -f e2e/docker-compose.bench.yml down
```

Settings (environment): `BENCH_CONNECTIONS` (200), `BENCH_SFTP_MIB` (256),
`BENCH_SFTP_MIB_WEAK` (32), `BENCH_SFTP_REPEATS` (5), `BENCH_OUTAGE_RUNS` (30),
`BENCH_OUTAGE_SECS` (10), `BENCH_ECHO_SAMPLES` (500), `BENCH_SOAK_SESSIONS` (25),
`BENCH_SOAK_MINUTES` (60), `BENCH_SOAK_HEARTBEAT_SECS` (30), `BENCH_HOST`,
`BENCH_PORT`, `BENCH_USER`, `BENCH_KEY`, `BENCH_NETEM`, `BENCH_CONTAINER`,
`BENCH_CIPHER`, `BENCH_KEX`, `BENCH_ENFORCE`. Without the netem script the
profile is **not** applied, the report says so, and the numbers describe the
loopback-fast Docker bridge.

The statistics and report rendering are tested without Docker:
`cargo test -p easyssh-e2e --lib bench`.

## Reading the results

`bench-report.md` has the environment, then one table per profile: metric,
result, target, and PASS, MISS or info. `info` means there is no target (weak
profile, M3, M4). `bench.json` (`schema` 1) holds the same plus every raw
sample (`ready_secs`, `echo_secs`, `recovery_secs`, `bytes_per_sec`), so a
different percentile or a comparison between releases can be computed later.

Things to keep in mind:

- Hosted CI runners share hardware. Compare the ratio in M6 (engine against
  OpenSSH on the same run), not absolute MiB/s, and look at the trend over
  several nightly runs before calling one MISS a regression.
- M1 and M2 share a loop: a timeout appears in both (as a failure in M1 and in
  the timeout count of M2).
- M5's old-session note shows how often TCP simply rode out the 10 s outage;
  the metric is the time to a *new* session either way.
- M3 is not the keystroke latency of the app: no terminal rendering, no
  browser. Treat it as the floor.

## Not done yet

- True M3 (key press to painted glyph in the UI).
- M4 at the full 8 hours as a scheduled job. The nightly default is 60 min; a manual run accepts `soak_minutes` up to about 360 (the job limit is 7 h).
- Gating pull requests on the targets (after the numbers are stable).
