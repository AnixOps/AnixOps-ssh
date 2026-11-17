//! Helpers for the end-to-end tests in `tests/`.
//!
//! The tests drive the real `easyssh-core` code, which spawns the system
//! `ssh`, `sftp` and `scp`, against the OpenSSH containers in `e2e/`.
//!
//! OpenSSH takes the home directory from the password database, not from
//! `$HOME`, so setting `HOME` alone would not keep it away from the real
//! `~/.ssh/config`. [`E2e`] therefore puts wrapper scripts for `ssh`, `sftp`
//! and `scp` first on `PATH`; each runs the real binary with `-F <test
//! config>`. The core finds the tools on `PATH`, so it runs the wrappers
//! without knowing. `HOME` points into the temporary directory as well and
//! `SSH_AUTH_SOCK` is removed.
#![cfg(unix)]

pub mod russh_support;

use easyssh_core::{Connection, OpenSsh, SshInvocation};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub const TARGET_PORT: u16 = 2222;
pub const BASTION_PORT: u16 = 2223;
/// Where the target container runs an echo service on its loopback.
pub const ECHO_PORT: u16 = 7777;
pub const PW_USER: &str = "pwuser";
/// Public on purpose; it is baked into `e2e/Dockerfile`.
pub const PW_PASSWORD: &str = "e2e-test-password";
pub const CMD_TIMEOUT: Duration = Duration::from_secs(60);

static LOCK: Mutex<()> = Mutex::new(());

/// An isolated OpenSSH environment for one test. Holds a process-wide lock
/// so tests cannot overlap even without `--test-threads=1`.
pub struct E2e {
    _lock: MutexGuard<'static, ()>,
    pub dir: tempfile::TempDir,
    pub key: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl E2e {
    pub fn new() -> Self {
        let lock = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        let home = root.join("home");
        let ssh_dir = home.join(".ssh");
        let bin = root.join("bin");
        for path in [&ssh_dir, &bin] {
            std::fs::create_dir_all(path).expect("create dir");
        }

        let source_key = std::env::var_os("EASYSSH_E2E_KEY")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../e2e/.keys/id_e2e")
            });
        assert!(
            source_key.is_file(),
            "test key {} not found; run e2e/prepare.sh and start the containers (see e2e/README.md)",
            source_key.display()
        );
        let key = ssh_dir.join("id_e2e");
        std::fs::copy(&source_key, &key).expect("copy test key");
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).expect("chmod key");

        let config = ssh_dir.join("config");
        std::fs::write(&config, ssh_config(root, &key)).expect("write ssh config");

        let path = std::env::var_os("PATH").unwrap_or_default();
        for tool in ["ssh", "sftp", "scp"] {
            let real = find_in(&path, tool)
                .unwrap_or_else(|| panic!("`{tool}` is not installed on this machine"));
            let script = format!(
                "#!/bin/sh\nexec '{}' -F '{}' \"$@\"\n",
                real.display(),
                config.display()
            );
            let wrapper = bin.join(tool);
            std::fs::write(&wrapper, script).expect("write wrapper");
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755))
                .expect("chmod wrapper");
        }

        let mut saved = Vec::new();
        let mut set = |name: &'static str, value: Option<OsString>| {
            saved.push((name, std::env::var_os(name)));
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        };
        let mut new_path = OsString::from(&bin);
        new_path.push(":");
        new_path.push(&path);
        set("PATH", Some(new_path));
        set("HOME", Some(home.into_os_string()));
        set("SSH_AUTH_SOCK", None);
        set("SSH_ASKPASS", None);
        set("SSH_ASKPASS_REQUIRE", None);

        Self {
            _lock: lock,
            dir,
            key,
            saved,
        }
    }

    /// A connection that names an alias from the generated config.
    pub fn alias(&self, alias: &str) -> Connection {
        Connection::alias(alias, alias)
    }

    /// Writes an `SSH_ASKPASS` helper that answers every prompt with
    /// `password`, appends each prompt to a log, and returns the environment
    /// the app's `askpass::env_for` sets, plus the log's path.
    pub fn askpass(&self, password: &str) -> (Vec<(String, String)>, PathBuf) {
        let id = unique_suffix();
        let script = self.dir.path().join(format!("askpass-{id}.sh"));
        let log = self.dir.path().join(format!("askpass-{id}.log"));
        let body = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\nprintf '%s\\n' '{}'\n",
            log.display(),
            password.replace('\'', "'\\''")
        );
        std::fs::write(&script, body).expect("write askpass");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
            .expect("chmod askpass");
        let mut env = vec![
            (
                "SSH_ASKPASS".to_owned(),
                script.to_string_lossy().into_owned(),
            ),
            ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
        ];
        if std::env::var_os("DISPLAY").is_none() {
            env.push(("DISPLAY".to_owned(), ":0".to_owned()));
        }
        (env, log)
    }

    /// Runs a fixed remote command through `OpenSsh::output`, like the app.
    pub fn exec(&self, connection: &Connection, command: &'static str) -> Output {
        let invocation = SshInvocation::for_fixed_remote_command(&OpenSsh, connection, command)
            .expect("build ssh invocation");
        with_timeout(CMD_TIMEOUT, "ssh exec", move || OpenSsh.output(&invocation)).expect("run ssh")
    }

    /// Like [`E2e::exec`] with extra environment (for `SSH_ASKPASS`).
    pub fn exec_with_env(
        &self,
        connection: &Connection,
        command: &'static str,
        env: &[(String, String)],
    ) -> Output {
        let invocation = SshInvocation::for_fixed_remote_command(&OpenSsh, connection, command)
            .expect("build ssh invocation");
        let mut process = Command::new(&invocation.executable);
        process.args(&invocation.args).envs(env.iter().cloned());
        run_command(process, CMD_TIMEOUT)
    }
}

impl Default for E2e {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for E2e {
    fn drop(&mut self) {
        for (name, value) in self.saved.drain(..).rev() {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn ssh_config(root: &Path, key: &Path) -> String {
    format!(
        "\
Host e2e-target
    HostName 127.0.0.1
    Port {TARGET_PORT}
    User keyuser

Host e2e-bastion
    HostName 127.0.0.1
    Port {BASTION_PORT}
    User keyuser

# Only resolvable from inside the compose network.
Host e2e-target-internal
    HostName target
    Port 22
    User keyuser

Host e2e-target-via-bastion
    HostName target
    Port 22
    User keyuser
    ProxyJump e2e-bastion

# The target through a ProxyCommand, which only OpenSSH can run: the daemon
# must fall back to it.
Host e2e-target-proxycmd
    HostName target
    Port 22
    User keyuser
    ProxyCommand ssh -W target:22 e2e-bastion

Host e2e-target-pw
    HostName 127.0.0.1
    Port {TARGET_PORT}
    User {PW_USER}
    PreferredAuthentications password
    PubkeyAuthentication no
    NumberOfPasswordPrompts 1

Host *
    StrictHostKeyChecking accept-new
    UserKnownHostsFile {known_hosts}
    GlobalKnownHostsFile /dev/null
    LogLevel ERROR
    IdentitiesOnly yes
    IdentityFile {key}
    ConnectTimeout 20
",
        known_hosts = root.join("known_hosts").display(),
        key = key.display(),
    )
}

fn find_in(path: &OsString, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn unique_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default()
}

/// A name that cannot collide with earlier runs on a reused server.
pub fn unique_name(prefix: &str) -> String {
    format!("{prefix}-{}-{}", std::process::id(), unique_suffix())
}

/// Runs `work` on a thread and fails the test if it takes longer than
/// `timeout`. The thread is abandoned on timeout, which only matters for a
/// test that is already failing.
pub fn with_timeout<T: Send + 'static>(
    timeout: Duration,
    what: &str,
    work: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(work());
    });
    receiver
        .recv_timeout(timeout)
        .unwrap_or_else(|_| panic!("{what} did not finish within {timeout:?}"))
}

/// Runs a command to completion with captured output; kills it and fails the
/// test on timeout.
pub fn run_command(mut command: Command, timeout: Duration) -> Output {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().expect("spawn command");
    finish(child, timeout)
}

/// Waits for a child whose stdout and stderr may be piped, collecting both.
pub fn finish(mut child: Child, timeout: Duration) -> Output {
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let status = wait_status(&mut child, timeout);
    let collect = |reader: Option<mpsc::Receiver<Vec<u8>>>| {
        reader
            .and_then(|reader| reader.recv_timeout(Duration::from_secs(5)).ok())
            .unwrap_or_default()
    };
    Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    }
}

fn drain(mut stream: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        let _ = sender.send(bytes);
    });
    receiver
}

/// Waits for exit; on timeout kills the child and fails the test.
pub fn wait_status(child: &mut Child, timeout: Duration) -> ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Kills the child when dropped so a failing test leaves no `ssh -N` behind.
pub struct Reaper(pub Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Reaper {
    /// Stops the child and returns once it has exited.
    pub fn stop(&mut self) {
        self.0.kill().expect("kill child");
        wait_status(&mut self.0, Duration::from_secs(10));
    }
}

pub fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

/// Waits until `port` accepts connections. Fails early, with the process's
/// standard error, if the tunnel process exits first.
pub fn wait_for_port(port: u16, tunnel: &mut Child, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = tunnel.try_wait().expect("poll tunnel") {
            let mut stderr = String::new();
            if let Some(mut pipe) = tunnel.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("tunnel exited early ({status}): {}", stderr.trim());
        }
        if TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(1))
            .is_ok()
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "port {port} did not open within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Waits until nothing accepts connections on `port` any more.
pub fn wait_for_port_closed(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(1)).is_ok()
    {
        assert!(
            Instant::now() < deadline,
            "port {port} still open after {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Sends a line to an echo service and expects the same line back.
pub fn echo_roundtrip(stream: &mut TcpStream, line: &str) {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("write timeout");
    stream
        .write_all(format!("{line}\n").as_bytes())
        .expect("write to echo");
    let mut received = Vec::new();
    let mut byte = [0u8; 1];
    while stream.read_exact(&mut byte).is_ok() {
        received.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    assert_eq!(text(&received), format!("{line}\n"), "echo mismatch");
}

/// Deterministic, non-trivial bytes for transfer tests.
pub fn sample_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 24) as u8
        })
        .collect()
}
