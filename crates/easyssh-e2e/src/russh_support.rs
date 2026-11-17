//! Helpers for the `russh_*` tests: the same containers as the OpenSSH
//! baseline, driven through `easyssh-ssh`.

use crate::{E2e, BASTION_PORT, CMD_TIMEOUT, PW_PASSWORD, PW_USER, TARGET_PORT};
use async_trait::async_trait;
use easyssh_ssh::{
    AgentSource, ConnectParams, Decision, Interaction, KbdPrompt, KnownHostStatus, KnownHostsFile,
};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Fails the test when `work` takes longer than `timeout`.
pub async fn within_for<T>(what: &str, timeout: Duration, work: impl Future<Output = T>) -> T {
    match tokio::time::timeout(timeout, work).await {
        Ok(value) => value,
        Err(_) => panic!("{what} did not finish within {timeout:?}"),
    }
}

/// [`within_for`] with the suite's usual 60 s.
pub async fn within<T>(what: &str, work: impl Future<Output = T>) -> T {
    within_for(what, CMD_TIMEOUT, work).await
}

/// An [`Interaction`] with fixed answers that records what it was asked.
pub struct Scripted {
    pub host_key: Decision,
    pub password: Option<String>,
    pub passphrase: Option<String>,
    log: Mutex<Vec<String>>,
}

impl Scripted {
    pub fn new(host_key: Decision) -> Self {
        Self {
            host_key,
            password: None,
            passphrase: None,
            log: Mutex::new(Vec::new()),
        }
    }

    /// Trusts unknown hosts on first use and records them.
    pub fn trusting() -> Self {
        Self::new(Decision::AcceptAndSave)
    }

    pub fn with_password(mut self, password: &str) -> Self {
        self.password = Some(password.to_owned());
        self
    }

    pub fn with_passphrase(mut self, passphrase: &str) -> Self {
        self.passphrase = Some(passphrase.to_owned());
        self
    }

    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    /// How many recorded questions start with `prefix`.
    pub fn asked(&self, prefix: &str) -> usize {
        self.log().iter().filter(|e| e.starts_with(prefix)).count()
    }

    fn record(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }
}

#[async_trait]
impl Interaction for Scripted {
    async fn verify_host_key(
        &self,
        host: &str,
        port: u16,
        fingerprint: &str,
        known: KnownHostStatus,
    ) -> Decision {
        let kind = match known {
            KnownHostStatus::Unknown => "unknown",
            KnownHostStatus::Changed { .. } => "changed",
            KnownHostStatus::Trusted => "trusted",
            KnownHostStatus::Revoked => "revoked",
        };
        assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
        self.record(format!("hostkey {host}:{port} {kind}"));
        self.host_key
    }

    async fn password(&self, prompt: &str) -> Option<String> {
        self.record(format!("password {prompt}"));
        self.password.clone()
    }

    async fn keyboard_interactive(
        &self,
        name: &str,
        _instructions: &str,
        prompts: &[KbdPrompt],
    ) -> Option<Vec<String>> {
        self.record(format!("kbd {name} {}", prompts.len()));
        let password = self.password.clone()?;
        Some(prompts.iter().map(|_| password.clone()).collect())
    }

    async fn passphrase(&self, key_path: &Path) -> Option<String> {
        self.record(format!("passphrase {}", key_path.display()));
        self.passphrase.clone()
    }
}

impl E2e {
    /// The known_hosts file the russh tests use (not created until needed).
    pub fn russh_known_hosts(&self) -> PathBuf {
        self.dir.path().join("russh_known_hosts")
    }

    fn base_params(&self, port: u16, user: &str) -> ConnectParams {
        let mut params = ConnectParams::new("127.0.0.1", port, user)
            .known_hosts(Arc::new(KnownHostsFile::new(self.russh_known_hosts())));
        // Never the real agent of the machine running the tests.
        params.agent = AgentSource::Disabled;
        params.connect_timeout = Duration::from_secs(20);
        params
    }

    /// `keyuser` on the target with the test key as identity file.
    pub fn russh_target(&self) -> ConnectParams {
        self.base_params(TARGET_PORT, "keyuser")
            .identity_file(&self.key)
    }

    /// `keyuser` on the bastion with the test key.
    pub fn russh_bastion(&self) -> ConnectParams {
        self.base_params(BASTION_PORT, "keyuser")
            .identity_file(&self.key)
    }

    /// The target as the bastion sees it (`target:22`).
    pub fn russh_target_behind_bastion(&self) -> ConnectParams {
        let mut params = self.base_params(22, "keyuser").identity_file(&self.key);
        params.host = "target".into();
        params
    }

    /// `pwuser` on the target; no keys, password only.
    pub fn russh_pw_target(&self) -> ConnectParams {
        self.base_params(TARGET_PORT, PW_USER)
    }

    pub fn pw_scripted(&self) -> Scripted {
        Scripted::trusting().with_password(PW_PASSWORD)
    }
}

/// Sends a line to an echo service and expects the same line back.
pub async fn echo_roundtrip(stream: &mut TcpStream, line: &str) {
    within_for("echo round trip", Duration::from_secs(15), async {
        stream
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write to echo");
        let mut received = Vec::new();
        let mut byte = [0u8; 1];
        while stream.read_exact(&mut byte).await.is_ok() {
            received.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        assert_eq!(String::from_utf8_lossy(&received), format!("{line}\n"));
    })
    .await;
}

/// Waits until `port` accepts connections.
pub async fn wait_for_port_open(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "port {port} did not open within {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Waits until nothing accepts connections on `port` any more.
pub async fn wait_for_port_shut(port: u16, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
        assert!(
            Instant::now() < deadline,
            "port {port} still open after {timeout:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
