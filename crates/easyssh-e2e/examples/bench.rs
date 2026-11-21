//! The benchmark harness (docs/benchmarks.md).
//!
//! ```text
//! cargo run --release -p easyssh-e2e --example bench -- \
//!     [--profiles normal,weak] [--metrics m1,m2,m3,m5,m6] [--out bench-out]
//! cargo run --release -p easyssh-e2e --example bench -- --metrics m4
//! ```
//!
//! Needs the bench container (`e2e/docker-compose.bench.yml`) and, for the
//! network profiles and M5, `e2e/netem.sh` (`BENCH_NETEM`). Settings come from
//! `BENCH_*` environment variables, see `easyssh_e2e::bench::Settings`.
//! Writes `bench.json` and `bench-report.md` to the output directory and the
//! Markdown also to `$GITHUB_STEP_SUMMARY`. Exit status: 1 for a harness
//! error or a SHA-256 mismatch (after writing what was measured), also 1 for a
//! missed target when `BENCH_ENFORCE=1`.
// The harness drives Unix tools (tc, nsenter, ssh, sftp); Windows and macOS
// CI only need the example to compile.
#[cfg(unix)]
fn main() {
    imp::main()
}

#[cfg(not(unix))]
fn main() {
    eprintln!("the benchmark harness runs on Linux only");
    std::process::exit(2);
}

#[cfg(unix)]
mod imp {

    use easyssh_e2e::bench::*;
    use easyssh_e2e::russh_support::Scripted;
    use easyssh_ssh::{AgentSource, ConnectParams, KnownHostsFile, Session, ShellOptions};
    use rand::{RngCore, SeedableRng};
    use sha2::{Digest, Sha256};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    type Res<T> = Result<T, String>;

    /// A connection or probe that takes longer is a failure (M1).
    const READY_LIMIT: Duration = Duration::from_secs(10);

    struct Ctx {
        cfg: Settings,
        dir: tempfile::TempDir,
        key: PathBuf,
        known_hosts: PathBuf,
        interaction: Scripted,
    }

    impl Ctx {
        fn params(&self) -> ConnectParams {
            let mut params = ConnectParams::new(&self.cfg.host, self.cfg.port, &self.cfg.user)
                .known_hosts(Arc::new(KnownHostsFile::new(self.known_hosts.clone())))
                .identity_file(&self.key);
            params.agent = AgentSource::Disabled;
            params.connect_timeout = READY_LIMIT;
            params
        }

        /// Connects and returns the session once `echo ready-<n>` has been
        /// answered. The caller times it.
        async fn connect_ready(&self, n: u32) -> Res<Session> {
            let session = Session::connect(self.params(), &self.interaction)
                .await
                .map_err(|e| format!("connect: {e}"))?;
            let out = session
                .exec(&format!("echo ready-{n}"))
                .await
                .map_err(|e| format!("probe: {e}"))?;
            if out.stdout_text().trim() != format!("ready-{n}") {
                return Err(format!("wrong probe reply {:?}", out.stdout_text()));
            }
            Ok(session)
        }

        /// OpenSSH client arguments with the same key, cipher and kex, and no
        /// access to the real configuration.
        fn ssh_options(&self) -> Vec<String> {
            let o = |s: String| ["-o".to_owned(), s];
            let mut args = vec!["-F".to_owned(), "/dev/null".to_owned()];
            for option in [
                "BatchMode=yes".to_owned(),
                "IdentitiesOnly=yes".to_owned(),
                "StrictHostKeyChecking=accept-new".to_owned(),
                "LogLevel=ERROR".to_owned(),
                "ConnectTimeout=20".to_owned(),
                format!(
                    "UserKnownHostsFile={}",
                    self.dir.path().join("openssh_known_hosts").display()
                ),
                format!("Ciphers={}", self.cfg.cipher),
                format!("KexAlgorithms={}", self.cfg.kex),
            ] {
                args.extend(o(option));
            }
            args.extend(["-i".to_owned(), self.key.display().to_string()]);
            args
        }

        fn target(&self) -> String {
            format!("{}@{}", self.cfg.user, self.cfg.host)
        }

        async fn netem(&self, profile: &str) -> Res<()> {
            let Some(script) = &self.cfg.netem_script else {
                return Ok(());
            };
            let out = tokio::process::Command::new("bash")
                .arg(script)
                .arg(&self.cfg.container)
                .arg(profile)
                .output()
                .await
                .map_err(|e| format!("running {script}: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "{script} {} {profile} failed: {}",
                    self.cfg.container,
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(())
        }
    }

    async fn bye(session: &Session) {
        let _ = tokio::time::timeout(Duration::from_secs(2), session.disconnect()).await;
    }

    // ------------------------------------------------------------------- M1, M2

    async fn measure_connects(ctx: &Ctx) -> Connects {
        let n = ctx.cfg.connections;
        let (mut timeouts, mut errors) = (0, 0);
        let mut ready = Vec::new();
        let mut failures = Vec::new();
        for i in 0..n {
            let start = Instant::now();
            let result = tokio::time::timeout(READY_LIMIT, ctx.connect_ready(i)).await;
            let elapsed = start.elapsed();
            match result {
                Ok(Ok(session)) => {
                    ready.push(elapsed.as_secs_f64());
                    bye(&session).await;
                }
                Ok(Err(e)) => {
                    errors += 1;
                    if failures.len() < 5 {
                        failures.push(format!("#{i}: {e}"));
                    }
                }
                Err(_) => {
                    timeouts += 1;
                    if failures.len() < 5 {
                        failures.push(format!("#{i}: no probe reply within 10 s"));
                    }
                }
            }
            if (i + 1) % 50 == 0 {
                eprintln!("M1/M2: {}/{n} done, {} ok", i + 1, ready.len());
            }
        }
        Connects::new(n, timeouts, errors, ready, failures)
    }

    // ---------------------------------------------------------------------- M3

    /// A byte written to the PTY channel until its echo arrives. The shell runs
    /// `cat`, so the echo comes from the terminal's line discipline on the
    /// server: network and engine only, no rendering.
    async fn measure_echo(ctx: &Ctx) -> Res<Echo> {
        let session = ctx.connect_ready(0).await?;
        let mut shell = session
            .shell(ShellOptions::default())
            .await
            .map_err(|e| format!("shell: {e}"))?;
        shell.write(b"cat\n").await.map_err(|e| e.to_string())?;
        // Wait for the prompt and the command's own echo to settle.
        loop {
            match tokio::time::timeout(Duration::from_millis(700), shell.read()).await {
                Ok(Some(_)) => {}
                Ok(None) => return Err("shell ended before the echo test".into()),
                Err(_) => break,
            }
        }
        let n = ctx.cfg.echo_samples;
        let (mut samples, mut missing) = (Vec::new(), 0);
        for i in 0..n {
            let byte = b'a' + (i % 26) as u8;
            let start = Instant::now();
            shell.write(&[byte]).await.map_err(|e| e.to_string())?;
            let got = tokio::time::timeout(Duration::from_secs(5), async {
                while let Some(chunk) = shell.read().await {
                    if chunk.contains(&byte) {
                        return true;
                    }
                }
                false
            })
            .await;
            match got {
                Ok(true) => samples.push(start.elapsed().as_secs_f64()),
                _ => missing += 1,
            }
            // Keep the line short and let late echoes of a missing sample land.
            if i % 50 == 49 {
                let _ = shell.write(b"\n").await;
                while let Ok(Some(_)) =
                    tokio::time::timeout(Duration::from_millis(100), shell.read()).await
                {
                }
            }
        }
        let _ = shell.write(&[4, 4]).await;
        bye(&session).await;
        Ok(Echo::new(n, samples, missing))
    }

    // ---------------------------------------------------------------------- M5

    async fn measure_outage(ctx: &Ctx, profile: &Profile) -> Res<Outage> {
        let window = 10u64;
        let mut recovery = Vec::new();
        let mut survived = 0;
        for run in 0..ctx.cfg.outage_runs {
            let old = ctx.connect_ready(run).await.ok();
            ctx.netem("down").await?;
            tokio::time::sleep(Duration::from_secs(ctx.cfg.outage_secs)).await;
            let restore = ctx.netem(profile.name).await;
            // The network is back from here on.
            let back = Instant::now();
            restore?;
            let deadline = back + Duration::from_secs(window);
            let mut found = None;
            if old.is_some() {
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        break;
                    }
                    let attempt = tokio::time::timeout(
                        left.min(Duration::from_secs(4)),
                        ctx.connect_ready(1000 + run),
                    )
                    .await;
                    if let Ok(Ok(session)) = attempt {
                        found = Some(back.elapsed().as_secs_f64());
                        bye(&session).await;
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
            if let Some(old) = old {
                if tokio::time::timeout(Duration::from_secs(5), old.ping())
                    .await
                    .is_ok_and(|r| r.is_ok())
                {
                    survived += 1;
                }
                bye(&old).await;
            }
            eprintln!("M5 run {}: {found:?}", run + 1);
            recovery.push(found);
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        Ok(Outage::new(
            ctx.cfg.outage_runs,
            ctx.cfg.outage_secs,
            window,
            recovery,
            survived,
        ))
    }

    // ---------------------------------------------------------------------- M6

    fn sha256_file(path: &Path) -> Res<String> {
        let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = std::io::Read::read(&mut file, &mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        Ok(hex(&hash.finalize()))
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Writes `size` bytes of random data (incompressible) and returns its SHA-256.
    fn make_random_file(path: &Path, size: u64) -> Res<String> {
        let mut rng = rand::rngs::StdRng::from_entropy();
        let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut left = size;
        while left > 0 {
            let n = left.min(buf.len() as u64) as usize;
            rng.fill_bytes(&mut buf[..n]);
            hash.update(&buf[..n]);
            std::io::Write::write_all(&mut file, &buf[..n]).map_err(|e| e.to_string())?;
            left -= n as u64;
        }
        Ok(hex(&hash.finalize()))
    }

    async fn local_sha(path: PathBuf) -> Res<String> {
        tokio::task::spawn_blocking(move || sha256_file(&path))
            .await
            .map_err(|e| e.to_string())?
    }

    async fn remote_sha(session: &Session, path: &str) -> Res<String> {
        let out = session
            .exec(&format!("sha256sum '{path}'"))
            .await
            .map_err(|e| format!("sha256sum: {e}"))?;
        out.stdout_text()
            .split_whitespace()
            .next()
            .map(str::to_owned)
            .ok_or_else(|| format!("sha256sum printed nothing: {}", out.stderr_text()))
    }

    fn check_sha(what: &str, got: &str, want: &str) -> Res<()> {
        if got == want {
            Ok(())
        } else {
            Err(format!(
                "SHA-256 MISMATCH after {what}: expected {want}, got {got}"
            ))
        }
    }

    async fn engine_upload(ctx: &Ctx, local: &Path, remote: &str) -> Res<Duration> {
        let start = Instant::now();
        let session = Session::connect(ctx.params(), &ctx.interaction)
            .await
            .map_err(|e| e.to_string())?;
        let sftp = session.sftp().await.map_err(|e| e.to_string())?;
        let mut file = tokio::fs::File::open(local)
            .await
            .map_err(|e| e.to_string())?;
        sftp.upload(remote, &mut file)
            .await
            .map_err(|e| e.to_string())?;
        let elapsed = start.elapsed();
        let _ = sftp.close().await;
        bye(&session).await;
        Ok(elapsed)
    }

    async fn engine_download(ctx: &Ctx, remote: &str, local: &Path) -> Res<Duration> {
        let start = Instant::now();
        let session = Session::connect(ctx.params(), &ctx.interaction)
            .await
            .map_err(|e| e.to_string())?;
        let sftp = session.sftp().await.map_err(|e| e.to_string())?;
        let mut file = tokio::fs::File::create(local)
            .await
            .map_err(|e| e.to_string())?;
        sftp.download(remote, &mut file)
            .await
            .map_err(|e| e.to_string())?;
        file.flush().await.map_err(|e| e.to_string())?;
        let elapsed = start.elapsed();
        let _ = sftp.close().await;
        bye(&session).await;
        Ok(elapsed)
    }

    /// `sftp -b` with one `put` or `get`; the time is the process's lifetime.
    async fn openssh_sftp(ctx: &Ctx, batch_line: String) -> Res<Duration> {
        let batch = ctx.dir.path().join("batch.txt");
        std::fs::write(&batch, format!("{batch_line}\n")).map_err(|e| e.to_string())?;
        let start = Instant::now();
        let out = tokio::process::Command::new("sftp")
            .args(ctx.ssh_options())
            .arg("-P")
            .arg(ctx.cfg.port.to_string())
            .arg("-b")
            .arg(&batch)
            .arg(ctx.target())
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .map_err(|e| format!("sftp: {e}"))?;
        let elapsed = start.elapsed();
        if !out.status.success() {
            return Err(format!(
                "sftp `{batch_line}` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(elapsed)
    }

    async fn measure_sftp(ctx: &Ctx, size_mib: u64) -> Res<Sftp> {
        let size = size_mib * 1024 * 1024;
        let work = ctx.dir.path().join("sftp");
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let source = work.join("source.bin");
        let sink = work.join("sink.bin");
        let src = source.clone();
        let sha = tokio::task::spawn_blocking(move || make_random_file(&src, size))
            .await
            .map_err(|e| e.to_string())??;

        let admin = ctx.connect_ready(0).await?;
        let home = {
            let sftp = admin.sftp().await.map_err(|e| e.to_string())?;
            sftp.home().await.map_err(|e| e.to_string())?
        };
        let remote_dir = format!("{home}/bench-{}", std::process::id());
        admin
            .exec(&format!("mkdir -p '{remote_dir}'"))
            .await
            .map_err(|e| e.to_string())?;
        let remote = format!("{remote_dir}/blob.bin");

        let result = async {
            let (mut eu, mut ed, mut ou, mut od) = (vec![], vec![], vec![], vec![]);
            let rate = |d: Duration| size as f64 / d.as_secs_f64();
            for i in 0..ctx.cfg.sftp_repeats {
                // Alternate who goes first so a drifting network favours neither.
                let engine_first = i % 2 == 0;
                for pass in 0..2 {
                    if (pass == 0) == engine_first {
                        let d = engine_upload(ctx, &source, &remote).await?;
                        check_sha("engine upload", &remote_sha(&admin, &remote).await?, &sha)?;
                        eu.push(rate(d));
                        let d = engine_download(ctx, &remote, &sink).await?;
                        check_sha("engine download", &local_sha(sink.clone()).await?, &sha)?;
                        ed.push(rate(d));
                    } else {
                        let d =
                            openssh_sftp(ctx, format!("put {} {remote}", source.display())).await?;
                        check_sha("OpenSSH upload", &remote_sha(&admin, &remote).await?, &sha)?;
                        ou.push(rate(d));
                        let d =
                            openssh_sftp(ctx, format!("get {remote} {}", sink.display())).await?;
                        check_sha("OpenSSH download", &local_sha(sink.clone()).await?, &sha)?;
                        od.push(rate(d));
                    }
                    let _ = std::fs::remove_file(&sink);
                }
                eprintln!("M6 round {}/{} done", i + 1, ctx.cfg.sftp_repeats);
            }
            Ok::<_, String>((eu, ed, ou, od))
        }
        .await;

        let _ = admin.exec(&format!("rm -rf '{remote_dir}'")).await;
        bye(&admin).await;
        let _ = std::fs::remove_dir_all(&work);
        let (eu, ed, ou, od) = result?;
        Ok(Sftp::new(size, ctx.cfg.sftp_repeats, sha, eu, ed, ou, od))
    }

    // ---------------------------------------------------------------------- M4

    async fn soak_session(
        ctx: Arc<Ctx>,
        index: u32,
        start: Instant,
        end: Instant,
    ) -> (u64, Vec<(u32, f64, String)>) {
        let heartbeat = Duration::from_secs(ctx.cfg.soak_heartbeat_secs);
        let (mut ok, mut events) = (0u64, Vec::new());
        let mut seq = 0u64;
        'outer: while Instant::now() < end {
            // (Re)start: not a disconnect while the session never came up, but
            // it is reported.
            let session = match ctx.connect_ready(index).await {
                Ok(s) => s,
                Err(e) => {
                    events.push((
                        index,
                        start.elapsed().as_secs_f64(),
                        format!("could not (re)connect: {e}"),
                    ));
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            let mut shell = match session.shell(ShellOptions::default()).await {
                Ok(s) => s,
                Err(e) => {
                    events.push((index, start.elapsed().as_secs_f64(), format!("shell: {e}")));
                    bye(&session).await;
                    continue;
                }
            };
            let mut next = Instant::now() + heartbeat;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(next.into()) => {
                        if next >= end { bye(&session).await; break 'outer; }
                        next += heartbeat;
                        seq += 1;
                        let marker = format!("soak-42-{index}-{seq}");
                        let line = format!("echo soak-$((6*7))-{index}-{seq}\n");
                        if shell.write(line.as_bytes()).await.is_err() {
                            events.push((index, start.elapsed().as_secs_f64(), "write failed".into()));
                            break;
                        }
                        let mut seen = String::new();
                        let reply = tokio::time::timeout(Duration::from_secs(15), async {
                            while let Some(chunk) = shell.read().await {
                                seen.push_str(&String::from_utf8_lossy(&chunk));
                                if seen.contains(&marker) { return true; }
                            }
                            false
                        }).await;
                        match reply {
                            Ok(true) => ok += 1,
                            Ok(false) => { events.push((index, start.elapsed().as_secs_f64(), "shell ended".into())); break; }
                            Err(_) => { events.push((index, start.elapsed().as_secs_f64(), "heartbeat unanswered for 15 s".into())); break; }
                        }
                    }
                    chunk = shell.read() => {
                        if chunk.is_none() {
                            events.push((index, start.elapsed().as_secs_f64(), "shell ended".into()));
                            break;
                        }
                    }
                }
            }
            bye(&session).await;
        }
        (ok, events)
    }

    async fn measure_soak(ctx: Arc<Ctx>, profile: &str) -> Soak {
        let duration = Duration::from_secs(ctx.cfg.soak_minutes * 60);
        let start = Instant::now();
        let end = start + duration;
        let tasks: Vec<_> = (0..ctx.cfg.soak_sessions)
            .map(|i| tokio::spawn(soak_session(ctx.clone(), i, start, end)))
            .collect();
        let (mut ok, mut events) = (0, Vec::new());
        for task in tasks {
            if let Ok((o, e)) = task.await {
                ok += o;
                events.extend(e);
            }
        }
        events.sort_by(|a, b| a.1.total_cmp(&b.1));
        Soak::new(
            profile,
            ctx.cfg.soak_sessions,
            duration.as_secs(),
            ctx.cfg.soak_heartbeat_secs,
            ok,
            events,
        )
    }

    // ------------------------------------------------------------- environment

    async fn command_line(program: &str, args: &[&str]) -> String {
        match tokio::process::Command::new(program)
            .args(args)
            .output()
            .await
        {
            Ok(out) => {
                let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&out.stderr));
                text.trim().to_owned()
            }
            Err(e) => format!("unknown ({e})"),
        }
    }

    async fn server_banner(ctx: &Ctx) -> String {
        let connect = tokio::net::TcpStream::connect((ctx.cfg.host.as_str(), ctx.cfg.port));
        let Ok(Ok(mut stream)) = tokio::time::timeout(Duration::from_secs(5), connect).await else {
            return "unknown".into();
        };
        let mut buf = [0u8; 128];
        let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(0);
        String::from_utf8_lossy(&buf[..n])
            .lines()
            .next()
            .unwrap_or("unknown")
            .to_owned()
    }

    async fn environment(ctx: &Ctx) -> Environment {
        let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        let cpu_model = cpuinfo
            .lines()
            .find(|l| l.starts_with("model name"))
            .and_then(|l| l.split(':').nth(1))
            .map_or("unknown", str::trim)
            .to_owned();
        let commit = match std::env::var("GITHUB_SHA") {
            Ok(sha) if !sha.is_empty() => sha,
            _ => command_line("git", &["rev-parse", "HEAD"]).await,
        };
        let shaping = if ctx.cfg.netem_script.is_some() {
            format!(
            "tc netem (delay, loss, rate) on the `{}` container's interface, both directions (egress and ifb for ingress), by e2e/netem.sh",
            ctx.cfg.container
        )
        } else {
            "none: the network was not shaped (BENCH_NETEM unset)".to_owned()
        };
        Environment {
            commit,
            engine_version: env!("CARGO_PKG_VERSION").to_owned(),
            client_openssh: command_line("ssh", &["-V"]).await,
            server_banner: server_banner(ctx).await,
            cipher: ctx.cfg.cipher.clone(),
            kex: ctx.cfg.kex.clone(),
            cpu_model,
            cpu_count: std::thread::available_parallelism().map_or(1, usize::from),
            kernel: command_line("uname", &["-sr"]).await,
            network_shaping: shaping,
            notes: vec![],
        }
    }

    // -------------------------------------------------------------------- main

    struct Args {
        profiles: Vec<Profile>,
        metrics: Vec<String>,
        out: PathBuf,
    }

    fn parse_args() -> Res<Args> {
        let mut profiles = vec!["normal".to_owned()];
        let mut metrics: Vec<String> = ["m1", "m2", "m5", "m6"].map(String::from).to_vec();
        let mut out = PathBuf::from("bench-out");
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || args.next().ok_or_else(|| format!("{arg} needs a value"));
            match arg.as_str() {
                "--profiles" => profiles = value()?.split(',').map(str::to_owned).collect(),
                "--metrics" => {
                    metrics = value()?
                        .split(',')
                        .map(|m| m.trim().to_lowercase())
                        .collect()
                }
                "--out" => out = PathBuf::from(value()?),
                other => return Err(format!("unknown argument {other}")),
            }
        }
        for m in &metrics {
            if !["m1", "m2", "m3", "m4", "m5", "m6"].contains(&m.as_str()) {
                return Err(format!("unknown metric {m}"));
            }
        }
        Ok(Args {
            profiles: profiles
                .iter()
                .map(|p| Profile::parse(p))
                .collect::<Res<_>>()?,
            metrics,
            out,
        })
    }

    #[tokio::main]
    pub async fn main() {
        let code = match run().await {
            Ok(code) => code,
            Err(e) => {
                eprintln!("bench: error: {e}");
                1
            }
        };
        std::process::exit(code);
    }

    async fn run() -> Res<i32> {
        let args = parse_args()?;
        let cfg = Settings::from_lookup(|name| std::env::var(name).ok())?;
        let key = if cfg.key.is_empty() {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e/.keys/id_e2e")
        } else {
            PathBuf::from(&cfg.key)
        };
        if !key.is_file() {
            return Err(format!(
                "key {} not found (run e2e/prepare.sh)",
                key.display()
            ));
        }
        let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
        // A private copy: ssh refuses keys that are not mode 0600.
        let private_key = dir.path().join("id_bench");
        std::fs::copy(&key, &private_key).map_err(|e| e.to_string())?;
        std::fs::set_permissions(
            &private_key,
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .map_err(|e| e.to_string())?;
        let ctx = Arc::new(Ctx {
            known_hosts: dir.path().join("russh_known_hosts"),
            key: private_key,
            interaction: Scripted::trusting(),
            cfg,
            dir,
        });
        let wants = |m: &str| args.metrics.iter().any(|x| x == m);

        // The server must answer before anything is measured.
        ctx.netem("off").await?;
        let first = ctx.connect_ready(0).await.map_err(|e| {
            format!(
                "the bench server {}:{} does not answer: {e}",
                ctx.cfg.host, ctx.cfg.port
            )
        })?;
        bye(&first).await;

        let mut env = environment(&ctx).await;
        let mut profiles = Vec::new();
        let mut harness_error: Option<String> = None;

        for profile in &args.profiles {
            eprintln!("== profile {} ({})", profile.name, profile.describe());
            if let Err(e) = ctx.netem(profile.name).await {
                harness_error = Some(e);
                break;
            }
            let mut result = ProfileResult::new(profile.clone());
            let skip = |result: &mut ProfileResult, metric: &str, why: &str| {
                result.skipped.insert(metric.to_owned(), why.to_owned());
            };
            if ctx.cfg.netem_script.is_none() {
                env.notes.push(format!("profile {} was NOT applied (BENCH_NETEM unset); numbers are for the unshaped network", profile.name));
            }
            if wants("m1") || wants("m2") {
                result.connects = Some(measure_connects(&ctx).await);
            }
            if wants("m3") {
                match measure_echo(&ctx).await {
                    Ok(e) => result.echo = Some(e),
                    Err(e) => skip(&mut result, "M3", &e),
                }
            }
            if wants("m5") {
                if ctx.cfg.netem_script.is_none() {
                    skip(
                        &mut result,
                        "M5",
                        "needs e2e/netem.sh (BENCH_NETEM) to cut the network",
                    );
                } else {
                    match measure_outage(&ctx, profile).await {
                        Ok(o) => result.outage = Some(o),
                        Err(e) => {
                            harness_error = Some(format!("M5: {e}"));
                        }
                    }
                }
            }
            if wants("m6") && harness_error.is_none() {
                let mib = if profile.name == "weak" {
                    ctx.cfg.sftp_mib_weak
                } else {
                    ctx.cfg.sftp_mib
                };
                match measure_sftp(&ctx, mib).await {
                    Ok(s) => result.sftp = Some(s),
                    Err(e) => harness_error = Some(format!("M6: {e}")),
                }
            }
            profiles.push(result);
            if harness_error.is_some() {
                break;
            }
        }

        let mut soak = None;
        if wants("m4") && harness_error.is_none() {
            let name = args.profiles.first().map_or("normal", |p| p.name);
            if let Err(e) = ctx.netem(name).await {
                harness_error = Some(e);
            } else {
                soak = Some(measure_soak(ctx.clone(), name).await);
            }
        }
        let _ = ctx.netem("off").await;

        let report = Report {
            schema: 1,
            generated_at_unix: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            environment: env,
            settings: ctx.cfg.clone(),
            profiles,
            soak,
        };
        let mut markdown = render_markdown(&report);
        if let Some(e) = &harness_error {
            markdown.push_str(&format!(
                "\n**Harness error, the run is incomplete:** {e}\n"
            ));
        }
        std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;
        std::fs::write(args.out.join("bench.json"), report.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(args.out.join("bench-report.md"), &markdown).map_err(|e| e.to_string())?;
        std::fs::write(
            args.out.join("release-summary.md"),
            render_release_summary(&report),
        )
        .map_err(|e| e.to_string())?;
        println!("{markdown}");
        if let Ok(summary) = std::env::var("GITHUB_STEP_SUMMARY") {
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(summary)
            {
                let _ = std::io::Write::write_all(&mut file, markdown.as_bytes());
            }
        }
        if let Some(e) = harness_error {
            eprintln!("bench: harness error: {e}");
            return Ok(1);
        }
        if ctx.cfg.enforce && !misses(&report).is_empty() {
            eprintln!("bench: targets missed (BENCH_ENFORCE=1)");
            return Ok(1);
        }
        Ok(0)
    }
}
