#![cfg(unix)]

use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::{run_command, E2e, Reaper};
use easyssh_ssh::{AgentSource, AuthMethod, Error, Session};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn key_auth_for_keyuser_reports_the_identity_file() {
    let e2e = E2e::new();
    let interaction = Scripted::trusting();
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &interaction),
    )
    .await
    .expect("connect");
    assert_eq!(
        session.auth_method(),
        &AuthMethod::IdentityFile(e2e.key.clone())
    );
    // A key login asks nothing but the host key.
    assert_eq!(interaction.log().len(), 1, "{:?}", interaction.log());
}

/// Copies the test key and protects the copy with a passphrase.
fn encrypted_copy(e2e: &E2e, passphrase: &str) -> PathBuf {
    let copy = e2e.dir.path().join("id_e2e_encrypted");
    std::fs::copy(&e2e.key, &copy).unwrap();
    let mut command = Command::new("ssh-keygen");
    command
        .args(["-q", "-p", "-N", passphrase, "-f"])
        .arg(&copy);
    let output = run_command(command, Duration::from_secs(30));
    assert!(output.status.success(), "ssh-keygen -p failed");
    copy
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn encrypted_identity_file_asks_for_the_passphrase() {
    let e2e = E2e::new();
    let key = encrypted_copy(&e2e, "e2e passphrase");
    let mut params = e2e.russh_target();
    params.identity_files = vec![key.clone()];
    let interaction = Scripted::trusting().with_passphrase("e2e passphrase");
    let session = within("connect", Session::connect(params, &interaction))
        .await
        .expect("connect");
    assert_eq!(session.auth_method(), &AuthMethod::IdentityFile(key));
    assert_eq!(
        interaction.asked("passphrase"),
        1,
        "{:?}",
        interaction.log()
    );
    let output = within("exec", session.exec("id -un")).await.unwrap();
    assert_eq!(output.stdout_text(), "keyuser\n");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn wrong_passphrase_fails_with_the_key_tried() {
    let e2e = E2e::new();
    let key = encrypted_copy(&e2e, "right one");
    let mut params = e2e.russh_target();
    params.identity_files = vec![key];
    let interaction = Scripted::trusting().with_passphrase("wrong one");
    let error = within("connect", Session::connect(params, &interaction))
        .await
        .expect_err("must not authenticate");
    match error {
        Error::AuthFailed { tried, notes } => {
            assert!(tried.is_empty(), "{tried:?}");
            assert!(
                notes.iter().any(|n| n.contains("wrong passphrase")),
                "{notes:?}"
            );
        }
        other => panic!("expected AuthFailed, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn later_identity_is_used_when_the_first_is_not_accepted() {
    let e2e = E2e::new();
    // A key the server does not know, then the right one.
    let other = e2e.dir.path().join("id_other");
    let mut command = Command::new("ssh-keygen");
    command
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&other);
    assert!(run_command(command, Duration::from_secs(30))
        .status
        .success());
    let mut params = e2e.russh_target();
    params.identity_files = vec![other, e2e.key.clone()];
    let session = within("connect", Session::connect(params, &Scripted::trusting()))
        .await
        .expect("connect");
    assert_eq!(
        session.auth_method(),
        &AuthMethod::IdentityFile(e2e.key.clone())
    );
}

/// A throwaway ssh-agent that holds the test key.
fn start_agent(e2e: &E2e, with_key: bool) -> (Reaper, PathBuf) {
    let socket = e2e
        .dir
        .path()
        .join(if with_key { "agent" } else { "agent-empty" });
    let child = Command::new("ssh-agent")
        .args(["-D", "-a"])
        .arg(&socket)
        .spawn()
        .expect("start ssh-agent");
    let reaper = Reaper(child);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "agent socket never appeared"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    if with_key {
        let mut add = Command::new("ssh-add");
        add.arg(&e2e.key).env("SSH_AUTH_SOCK", &socket);
        let output = run_command(add, Duration::from_secs(20));
        assert!(output.status.success(), "ssh-add failed");
    }
    (reaper, socket)
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn agent_auth_is_tried_first_and_reported() {
    let e2e = E2e::new();
    let (_agent, socket) = start_agent(&e2e, true);
    let mut params = e2e.russh_target();
    params.agent = AgentSource::Path(socket);
    // The identity file is also configured; the agent must win.
    let session = within("connect", Session::connect(params, &Scripted::trusting()))
        .await
        .expect("connect");
    assert_eq!(session.auth_method(), &AuthMethod::Agent);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn empty_agent_falls_through_to_the_identity_file() {
    let e2e = E2e::new();
    let (_agent, socket) = start_agent(&e2e, false);
    let mut params = e2e.russh_target();
    params.agent = AgentSource::Path(socket);
    let session = within("connect", Session::connect(params, &Scripted::trusting()))
        .await
        .expect("connect");
    assert_eq!(
        session.auth_method(),
        &AuthMethod::IdentityFile(e2e.key.clone())
    );
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn missing_agent_socket_is_not_fatal() {
    let e2e = E2e::new();
    let mut params = e2e.russh_target();
    params.agent = AgentSource::Path(Path::new("/nonexistent/agent.sock").to_path_buf());
    let session = within("connect", Session::connect(params, &Scripted::trusting()))
        .await
        .expect("connect");
    assert!(matches!(session.auth_method(), AuthMethod::IdentityFile(_)));
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn no_credentials_fails_with_the_methods_tried() {
    let e2e = E2e::new();
    let mut params = e2e.russh_target();
    params.identity_files.clear();
    let error = within("connect", Session::connect(params, &Scripted::trusting()))
        .await
        .expect_err("keyuser needs a key");
    assert!(matches!(error, Error::AuthFailed { .. }), "{error:?}");
}
