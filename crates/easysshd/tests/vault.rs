//! Saved passwords and keys through the daemon: the vault commands, askpass
//! answers from the vault (only for the exact host, user and prompt kind),
//! "Remember in vault", saved keys as short-lived 0600 files, "Forget all",
//! and that no secret reaches the log. The vault uses an in-memory key store;
//! the real keychain is never touched.

use easyssh_core::{ConfigStore, Connection, ConnectionTarget};
use easyssh_crypto::{KdfParams, MemoryKeyStore};
use easyssh_proto as proto;
use easyssh_vault::Vault;
use easysshd::askpass;
use easysshd::link::{AskpassPrompt, Link, LinkHandler};
use easysshd::{Daemon, DaemonConfig, DaemonExtras, Server};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

const PASSWORD: &str = "pw-SECRET-4d1c9e";
const PASSPHRASE: &str = "phrase-SECRET-77aa31";
const KEY: &str =
    "-----BEGIN OPENSSH PRIVATE KEY-----\nKEYBODY-SECRET-5521\n-----END OPENSSH PRIVATE KEY-----\n";
const VAULT_PASSPHRASE: &str = "vault-pass-SECRET-9c3e";

#[derive(Default)]
struct Collect {
    prompts: Mutex<Vec<AskpassPrompt>>,
    vault: Mutex<Vec<bool>>,
}

impl LinkHandler for Collect {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        self.prompts.lock().unwrap().push(prompt);
    }
    fn vault_changed(&self, unlocked: bool) {
        self.vault.lock().unwrap().push(unlocked);
    }
}

struct Running {
    daemon: Arc<Daemon>,
    stop: oneshot::Sender<()>,
    task: JoinHandle<()>,
    link: Link,
    seen: Arc<Collect>,
    root: PathBuf,
    endpoint: String,
}

impl Running {
    async fn call(&self, kind: &str, payload: Value) -> Result<Value, String> {
        let command =
            json!({"id": uuid::Uuid::new_v4().to_string(), "kind": kind, "payload": payload});
        proto::validate_command(&command).map_err(|e| e.join("; "))?;
        self.link.request(kind, payload).await.map_err(|e| e.code)
    }

    /// What a helper process does for one prompt.
    async fn ask(&self, context: &str, prompt: &str) -> Option<String> {
        let (endpoint, context, prompt) =
            (self.endpoint.clone(), context.to_owned(), prompt.to_owned());
        tokio::task::spawn_blocking(move || askpass::ask(&endpoint, &context, "host", &prompt))
            .await
            .unwrap()
    }

    fn prompts(&self) -> Vec<AskpassPrompt> {
        self.seen.prompts.lock().unwrap().clone()
    }

    async fn next_prompt(&self, already: usize) -> AskpassPrompt {
        for _ in 0..200 {
            if let Some(p) = self.prompts().get(already) {
                return p.clone();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no prompt arrived");
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("run").join("easysshd.log")).unwrap_or_default()
    }

    async fn stop(self) {
        drop(self.link);
        let _ = self.stop.send(());
        self.task.await.unwrap();
        self.daemon.shutdown();
    }
}

/// The runtime directory must be owner-only, as the daemon insists.
fn private_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .unwrap();
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path).unwrap();
}

async fn start(root: &Path) -> Running {
    let cfg = DaemonConfig {
        config_store: ConfigStore::at(root.join("connections.json")),
        local_shell: Some("/bin/sh".into()),
        scrollback_bytes: 1 << 20,
        idle_timeout: Duration::from_secs(600),
    };
    let runtime = root.join("run");
    let vault = Arc::new(Vault::with_kdf(
        Arc::new(MemoryKeyStore::new()),
        KdfParams::insecure_for_tests(),
    ));
    let extras = DaemonExtras {
        log_dir: Some(runtime.clone()),
        runtime_dir: Some(runtime.clone()),
        vault: Some(vault),
        askpass_helper: Some("/nonexistent/askpass-helper".into()),
        ..DaemonExtras::default()
    };
    private_dir(&runtime);
    let daemon = Daemon::new_with(cfg, extras).await.unwrap();
    let server = Server::start_service(&runtime, daemon.clone()).unwrap();
    let (stop, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        server
            .serve(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    let seen = Arc::new(Collect::default());
    let link = Link::connect(&runtime, seen.clone()).await.unwrap();
    let endpoint = link.askpass_endpoint().to_owned();
    Running {
        daemon,
        stop,
        task,
        link,
        seen,
        root: root.to_owned(),
        endpoint,
    }
}

fn endpoint_host(name: &str, user: &str, hostname: &str) -> Connection {
    let mut connection = Connection::alias(name, "x");
    connection.target = ConnectionTarget::Endpoint {
        hostname: hostname.into(),
        username: Some(user.into()),
        port: 22,
    };
    connection
}

async fn save_host(s: &Running, connection: &Connection) {
    s.call(
        "hosts.save",
        json!({"connection": serde_json::to_value(connection).unwrap()}),
    )
    .await
    .unwrap();
}

async fn bind(s: &Running, context: &str, host_id: &str) {
    s.call(
        "askpass.bind",
        json!({"context": context, "host_id": host_id}),
    )
    .await
    .unwrap();
}

const USER_PROMPT: &str = "alice@db.example.test's password: ";

#[tokio::test]
async fn stored_password_answers_only_the_exact_host_and_user() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let host = endpoint_host("db", "alice", "db.example.test");
    save_host(&s, &host).await;
    let status = s
        .call(
            "secrets.set",
            json!({"host_id": host.id, "password": PASSWORD, "passphrase": PASSPHRASE}),
        )
        .await
        .unwrap();
    proto::validate("SecretsStatusResult", &status).unwrap();
    assert_eq!(status["password"], true);
    assert_eq!(status["private_key"], false);
    bind(&s, "ctx-a", &host.id).await;

    // The exact prompt: answered, no dialog.
    assert_eq!(s.ask("ctx-a", USER_PROMPT).await.as_deref(), Some(PASSWORD));
    assert!(s.prompts().is_empty(), "no dialog for a saved password");
    // OpenSSH asking again means the saved one was wrong: the user is asked,
    // the stored password is not sent a second time.
    let ask = {
        let (endpoint, prompt) = (s.endpoint.clone(), USER_PROMPT.to_owned());
        tokio::task::spawn_blocking(move || askpass::ask(&endpoint, "ctx-a", "host", &prompt))
    };
    let prompt = s.next_prompt(0).await;
    assert!(prompt.rememberable && !prompt.vault_locked);
    s.link
        .askpass_reply(prompt.id, Some("typed".into()))
        .unwrap();
    assert_eq!(ask.await.unwrap().as_deref(), Some("typed"));

    // The passphrase prompt of a key, once.
    let key_prompt = "Enter passphrase for key '/home/alice/.ssh/id_ed25519': ";
    assert_eq!(
        s.ask("ctx-a", key_prompt).await.as_deref(),
        Some(PASSPHRASE)
    );
    let before = s.prompts().len();

    // Nothing else is ever answered from the vault. Each prompt gets its own
    // process (context), as a cancelled prompt ends that process's prompts.
    for (n, prompt) in [
        "bob@db.example.test's password: ",
        "alice@other.example.test's password: ",
        "alice@db.example.test.evil's password: ",
        "Verification code: ",
        "Password: ",
        "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
    ]
    .into_iter()
    .enumerate()
    {
        let context = format!("ctx-other-{n}");
        bind(&s, &context, &host.id).await;
        let (endpoint, prompt_owned) = (s.endpoint.clone(), prompt.to_owned());
        let ask = tokio::task::spawn_blocking(move || {
            askpass::ask(&endpoint, &context, "host", &prompt_owned)
        });
        let shown = s.next_prompt(before + n).await;
        assert_eq!(shown.prompt, prompt);
        assert_eq!(shown.confirm, prompt.contains("(yes/no"));
        assert!(!shown.rememberable, "{prompt}");
        s.link.askpass_reply(shown.id, None).unwrap();
        assert_eq!(ask.await.unwrap(), None);
    }
    s.stop().await;
}

#[tokio::test]
async fn an_unbound_process_and_another_host_get_no_saved_answer() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let a = endpoint_host("a", "alice", "db.example.test");
    let b = endpoint_host("b", "alice", "other.example.test");
    save_host(&s, &a).await;
    save_host(&s, &b).await;
    s.call(
        "secrets.set",
        json!({"host_id": a.id, "password": PASSWORD}),
    )
    .await
    .unwrap();

    // Not bound at all.
    let ask = {
        let endpoint = s.endpoint.clone();
        tokio::task::spawn_blocking(move || {
            askpass::ask(&endpoint, "never-bound", "h", USER_PROMPT)
        })
    };
    let shown = s.next_prompt(0).await;
    assert!(!shown.rememberable);
    s.link.askpass_reply(shown.id, None).unwrap();
    assert_eq!(ask.await.unwrap(), None);

    // Bound to host b, which has nothing saved: asked, and offered to remember.
    bind(&s, "ctx-b", &b.id).await;
    let ask = {
        let endpoint = s.endpoint.clone();
        tokio::task::spawn_blocking(move || {
            askpass::ask(
                &endpoint,
                "ctx-b",
                "h",
                "alice@other.example.test's password: ",
            )
        })
    };
    let shown = s.next_prompt(1).await;
    assert!(shown.rememberable);
    // The prompt of host a's server is not b's prompt.
    s.link.askpass_reply(shown.id, None).unwrap();
    assert_eq!(ask.await.unwrap(), None);
    s.stop().await;
}

#[tokio::test]
async fn remember_in_vault_saves_the_typed_password_once() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let host = endpoint_host("db", "alice", "db.example.test");
    save_host(&s, &host).await;
    bind(&s, "ctx-1", &host.id).await;

    let ask = {
        let endpoint = s.endpoint.clone();
        tokio::task::spawn_blocking(move || askpass::ask(&endpoint, "ctx-1", "h", USER_PROMPT))
    };
    let shown = s.next_prompt(0).await;
    assert!(shown.rememberable);
    // Without the tick nothing is saved.
    s.link
        .askpass_reply_with(shown.id, Some("typed-once".into()), false)
        .unwrap();
    assert_eq!(ask.await.unwrap().as_deref(), Some("typed-once"));
    let status = s
        .call("secrets.status", json!({"host_id": host.id}))
        .await
        .unwrap();
    assert_eq!(status["password"], false);

    bind(&s, "ctx-2", &host.id).await;
    let ask = {
        let endpoint = s.endpoint.clone();
        tokio::task::spawn_blocking(move || askpass::ask(&endpoint, "ctx-2", "h", USER_PROMPT))
    };
    let shown = s.next_prompt(1).await;
    s.link
        .askpass_reply_with(shown.id, Some("remember-me".into()), true)
        .unwrap();
    assert_eq!(ask.await.unwrap().as_deref(), Some("remember-me"));
    let status = s
        .call("secrets.status", json!({"host_id": host.id}))
        .await
        .unwrap();
    assert_eq!(status["password"], true);

    // A new process of the same host is answered from the vault.
    bind(&s, "ctx-3", &host.id).await;
    assert_eq!(
        s.ask("ctx-3", USER_PROMPT).await.as_deref(),
        Some("remember-me")
    );
    assert_eq!(s.prompts().len(), 2);
    s.stop().await;
}

#[tokio::test]
async fn the_proxy_password_is_saved_like_a_secret_and_never_listed() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let host = endpoint_host("db", "alice", "db.example.test");
    save_host(&s, &host).await;
    let status = s
        .call(
            "secrets.set",
            json!({"host_id": host.id, "proxy_password": "proxy-pass-unique"}),
        )
        .await
        .unwrap();
    proto::validate("SecretsStatusResult", &status).unwrap();
    assert_eq!(status["proxy_password"], true);
    assert_eq!(status["password"], false);
    let again = s
        .call("secrets.status", json!({"host_id": host.id}))
        .await
        .unwrap();
    assert_eq!(again["proxy_password"], true);
    // Neither the host list nor the status carries the value.
    let list = s.call("hosts.list", json!({})).await.unwrap();
    assert!(!list.to_string().contains("proxy-pass-unique"));
    // An empty string clears it; a multi-line one is refused.
    let e = s
        .call(
            "secrets.set",
            json!({"host_id": host.id, "proxy_password": "a\nb"}),
        )
        .await
        .unwrap_err();
    assert_eq!(e, "invalid_command");
    let cleared = s
        .call(
            "secrets.set",
            json!({"host_id": host.id, "proxy_password": ""}),
        )
        .await
        .unwrap();
    assert_eq!(cleared["proxy_password"], false);
}

#[tokio::test]
async fn a_locked_vault_asks_for_the_passphrase_and_then_answers() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let host = endpoint_host("db", "alice", "db.example.test");
    save_host(&s, &host).await;
    s.call(
        "secrets.set",
        json!({"host_id": host.id, "password": PASSWORD}),
    )
    .await
    .unwrap();
    let status = s
        .call(
            "vault.set_passphrase",
            json!({"passphrase": VAULT_PASSPHRASE}),
        )
        .await
        .unwrap();
    proto::validate("VaultStatus", &status).unwrap();
    assert_eq!(status["passphrase_set"], true);
    let status = s.call("vault.lock", json!({})).await.unwrap();
    assert_eq!(status["unlocked"], false);

    // Flags need no key; values and writes do.
    let flags = s
        .call("secrets.status", json!({"host_id": host.id}))
        .await
        .unwrap();
    assert_eq!(flags["password"], true);
    assert_eq!(
        s.call("secrets.set", json!({"host_id": host.id, "password": "x"}))
            .await
            .unwrap_err(),
        "vault_locked"
    );

    bind(&s, "ctx-l", &host.id).await;
    let ask = {
        let endpoint = s.endpoint.clone();
        tokio::task::spawn_blocking(move || askpass::ask(&endpoint, "ctx-l", "h", USER_PROMPT))
    };
    let shown = s.next_prompt(0).await;
    assert!(shown.vault_locked, "the dialog must offer to unlock");
    assert_eq!(
        s.call(
            "askpass.unlock",
            json!({"id": shown.id, "passphrase": "wrong"})
        )
        .await
        .unwrap_err(),
        "wrong_passphrase"
    );
    s.call(
        "askpass.unlock",
        json!({"id": shown.id, "passphrase": VAULT_PASSPHRASE}),
    )
    .await
    .unwrap();
    assert_eq!(ask.await.unwrap().as_deref(), Some(PASSWORD));
    assert!(s.daemon.vault().is_unlocked());
    s.stop().await;
}

#[tokio::test]
async fn vault_commands_status_lock_auto_lock_and_forget_all() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let host = endpoint_host("db", "alice", "db.example.test");
    save_host(&s, &host).await;
    let status = s.call("vault.status", json!({})).await.unwrap();
    proto::validate("VaultStatus", &status).unwrap();
    assert_eq!(status["passphrase_set"], false);
    assert_eq!(status["auto_lock_minutes"], 15);
    assert_eq!(status["keychain_available"], true);

    s.call(
        "secrets.set",
        json!({"host_id": host.id, "password": PASSWORD, "private_key": KEY}),
    )
    .await
    .unwrap();
    let status = s
        .call("vault.set_auto_lock", json!({"minutes": 5}))
        .await
        .unwrap();
    assert_eq!(status["auto_lock_minutes"], 5);
    // The schema caps it at a day.
    assert!(s
        .call("vault.set_auto_lock", json!({"minutes": 100000}))
        .await
        .unwrap_err()
        .contains("maximum"));

    // Bad input is refused.
    assert_eq!(
        s.call(
            "secrets.set",
            json!({"host_id": host.id, "private_key": "ssh-ed25519 AAAA public key"})
        )
        .await
        .unwrap_err(),
        "invalid_key"
    );
    assert_eq!(
        s.call(
            "secrets.set",
            json!({"host_id": host.id, "password": "a\nb"})
        )
        .await
        .unwrap_err(),
        "invalid_command"
    );
    assert_eq!(
        s.call(
            "secrets.set",
            json!({"host_id": "missing", "password": "x"})
        )
        .await
        .unwrap_err(),
        "store_error"
    );

    // Setting, changing and removing the vault passphrase.
    s.call("vault.set_passphrase", json!({"passphrase": "one-one"}))
        .await
        .unwrap();
    assert_eq!(
        s.call("vault.set_passphrase", json!({"passphrase": "again"}))
            .await
            .unwrap_err(),
        "invalid_passphrase"
    );
    assert_eq!(
        s.call(
            "vault.set_passphrase",
            json!({"passphrase": "two-two", "current": "bad"})
        )
        .await
        .unwrap_err(),
        "wrong_passphrase"
    );
    s.call(
        "vault.set_passphrase",
        json!({"passphrase": "two-two", "current": "one-one"}),
    )
    .await
    .unwrap();
    s.call("vault.lock", json!({})).await.unwrap();
    s.call("vault.unlock", json!({"passphrase": "two-two"}))
        .await
        .unwrap();
    let status = s
        .call("vault.remove_passphrase", json!({"passphrase": "two-two"}))
        .await
        .unwrap();
    assert_eq!(status["passphrase_set"], false);

    // Forget all works even while locked and leaves hosts alone.
    s.call("vault.set_passphrase", json!({"passphrase": "three"}))
        .await
        .unwrap();
    s.call("vault.lock", json!({})).await.unwrap();
    let result = s.call("vault.forget_all", json!({})).await.unwrap();
    proto::validate("VaultForgetAllResult", &result).unwrap();
    assert_eq!(result["hosts"], 1);
    let flags = s
        .call("secrets.status", json!({"host_id": host.id}))
        .await
        .unwrap();
    assert_eq!(flags["password"], false);
    assert_eq!(flags["private_key"], false);
    let list = s.call("hosts.list", json!({})).await.unwrap();
    assert_eq!(list["connections"].as_array().unwrap().len(), 1);
    // The daemon told its clients about lock changes.
    assert!(s.seen.vault.lock().unwrap().contains(&false));
    s.stop().await;
}

#[tokio::test]
async fn deleting_a_host_deletes_its_secrets_and_nothing_secret_is_in_the_log() {
    let dir = TempDir::new().unwrap();
    let s = start(dir.path()).await;
    let host = endpoint_host("db", "alice", "db.example.test");
    save_host(&s, &host).await;
    s.call(
        "secrets.set",
        json!({
            "host_id": host.id,
            "password": PASSWORD,
            "passphrase": PASSPHRASE,
            "private_key": KEY,
        }),
    )
    .await
    .unwrap();
    s.call(
        "vault.set_passphrase",
        json!({"passphrase": VAULT_PASSPHRASE}),
    )
    .await
    .unwrap();
    s.call("vault.lock", json!({})).await.unwrap();
    s.call("vault.unlock", json!({"passphrase": VAULT_PASSPHRASE}))
        .await
        .unwrap();
    bind(&s, "ctx", &host.id).await;
    assert_eq!(s.ask("ctx", USER_PROMPT).await.as_deref(), Some(PASSWORD));
    // A rejected command with a secret in it must not echo it either.
    let _ = s
        .call(
            "secrets.set",
            json!({"host_id": host.id, "private_key": "not a key PASSWORD-IN-ERROR-31"}),
        )
        .await;

    let log = s.log();
    assert!(!log.is_empty(), "the log should have lifecycle lines");
    for needle in [
        PASSWORD,
        PASSPHRASE,
        "KEYBODY-SECRET-5521",
        VAULT_PASSPHRASE,
        "PASSWORD-IN-ERROR-31",
    ] {
        assert!(!log.contains(needle), "{needle} found in the log:\n{log}");
    }
    // Nor is anything readable in the database file or its journal.
    for entry in std::fs::read_dir(dir.path()).unwrap().flatten() {
        if entry.path().is_file() {
            let bytes = std::fs::read(entry.path()).unwrap();
            let text = String::from_utf8_lossy(&bytes);
            for needle in [PASSWORD, PASSPHRASE, "KEYBODY-SECRET-5521"] {
                assert!(
                    !text.contains(needle),
                    "{needle} found in {}",
                    entry.path().display()
                );
            }
        }
    }

    s.call("hosts.delete", json!({"id": host.id}))
        .await
        .unwrap();
    let flags = s
        .call("secrets.status", json!({"host_id": host.id}))
        .await
        .unwrap();
    assert_eq!(flags["password"], false);
    s.stop().await;
}

#[cfg(unix)]
mod key_files {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    static PATH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// A fake `ssh` first on `PATH` that records how it was called and
    /// lingers a moment, like a connected session.
    fn fake_ssh(dir: &Path, linger: &str) -> PathBuf {
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = format!(
            "#!/bin/sh\n\
             here=$(dirname \"$0\")\n\
             key=\n\
             while [ $# -gt 0 ]; do if [ \"$1\" = \"-i\" ]; then key=\"$2\"; fi; shift; done\n\
             {{ echo \"key=$key\"; stat -c %a \"$key\" 2>/dev/null || stat -f %Lp \"$key\"; cat \"$key\"; \
             echo \"askpass=$SSH_ASKPASS_REQUIRE\"; }} > \"$here/record.txt\"\n\
             sleep {linger}\n"
        );
        let path = bin.join("ssh");
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
        for _ in 0..200 {
            if done() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    fn key_files(root: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(root.join("run").join("keys"))
            .map(|entries| entries.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_saved_key_is_a_private_file_only_while_ssh_runs() {
        let _guard = PATH_LOCK.lock().await;
        let dir = TempDir::new().unwrap();
        let bin = fake_ssh(dir.path(), "1");
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = bin.into_os_string();
        new_path.push(":");
        new_path.push(&old_path);
        std::env::set_var("PATH", &new_path);

        let s = start(dir.path()).await;
        // These tests are about the key file OpenSSH gets, so the host is
        // pinned to it (the built-in engine never writes a key file).
        let mut host = endpoint_host("db", "alice", "db.example.test");
        host.engine = Some(easyssh_core::EnginePreference::Openssh);
        save_host(&s, &host).await;
        s.call(
            "secrets.set",
            json!({"host_id": host.id, "private_key": KEY}),
        )
        .await
        .unwrap();
        let opened = s
            .call(
                "session.open",
                json!({"host_id": host.id, "cols": 80, "rows": 24}),
            )
            .await;
        std::env::set_var("PATH", &old_path);
        opened.unwrap();

        let record = dir.path().join("bin").join("record.txt");
        wait_for("the fake ssh to record its call", || {
            std::fs::read_to_string(&record).is_ok_and(|t| t.contains("askpass="))
        })
        .await;
        let text = std::fs::read_to_string(&record).unwrap();
        let mut lines = text.lines();
        let key_line = lines.next().unwrap();
        let path = key_line.strip_prefix("key=").unwrap();
        assert!(path.contains("/run/keys/key-"), "{path}");
        assert_eq!(lines.next().unwrap(), "600", "the key file must be 0600");
        assert!(text.contains("KEYBODY-SECRET-5521"));
        assert!(
            text.contains("askpass=force"),
            "prompts must be routed to the daemon: {text}"
        );

        // Gone when ssh ends.
        wait_for("the key file to be deleted", || {
            key_files(dir.path()).is_empty()
        })
        .await;
        assert!(!Path::new(path).exists());
        s.stop().await;
    }

    #[tokio::test]
    async fn closing_the_session_deletes_the_key_and_a_locked_vault_fails_early() {
        let _guard = PATH_LOCK.lock().await;
        let dir = TempDir::new().unwrap();
        let bin = fake_ssh(dir.path(), "30");
        let old_path = std::env::var_os("PATH").unwrap_or_default();
        let mut new_path = bin.into_os_string();
        new_path.push(":");
        new_path.push(&old_path);
        std::env::set_var("PATH", &new_path);

        let s = start(dir.path()).await;
        // These tests are about the key file OpenSSH gets, so the host is
        // pinned to it (the built-in engine never writes a key file).
        let mut host = endpoint_host("db", "alice", "db.example.test");
        host.engine = Some(easyssh_core::EnginePreference::Openssh);
        save_host(&s, &host).await;
        s.call(
            "secrets.set",
            json!({"host_id": host.id, "private_key": KEY}),
        )
        .await
        .unwrap();
        let opened = s
            .call(
                "session.open",
                json!({"host_id": host.id, "cols": 80, "rows": 24}),
            )
            .await;
        let opened = opened.inspect_err(|_| {
            std::env::set_var("PATH", &old_path);
        });
        let session = opened.unwrap()["session_id"].as_str().unwrap().to_owned();
        wait_for("the key file to exist", || key_files(dir.path()).len() == 1).await;
        let file = key_files(dir.path()).remove(0);
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        s.call("session.close", json!({"session_id": session}))
            .await
            .unwrap();
        wait_for("the key file to be deleted", || !file.exists()).await;

        // A passphrase vault that is locked cannot hand out the key.
        s.call(
            "vault.set_passphrase",
            json!({"passphrase": VAULT_PASSPHRASE}),
        )
        .await
        .unwrap();
        s.call("vault.lock", json!({})).await.unwrap();
        let error = s
            .call(
                "session.open",
                json!({"host_id": host.id, "cols": 80, "rows": 24}),
            )
            .await;
        std::env::set_var("PATH", &old_path);
        assert_eq!(error.unwrap_err(), "vault_locked");
        assert!(key_files(dir.path()).is_empty());
        s.stop().await;
    }
}
