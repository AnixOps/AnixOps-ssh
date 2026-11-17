#![cfg(unix)]

use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::E2e;
use easyssh_ssh::{AuthMethod, Decision, Error, ExitStatus, Session};

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn exec_with_password_from_the_interaction() {
    let e2e = E2e::new();
    let interaction = e2e.pw_scripted();
    let session = within(
        "connect",
        Session::connect(e2e.russh_pw_target(), &interaction),
    )
    .await
    .expect("connect");
    assert_eq!(session.auth_method(), &AuthMethod::Password);
    assert_eq!(
        interaction.asked("password pwuser@127.0.0.1"),
        1,
        "{:?}",
        interaction.log()
    );
    let output = within("exec", session.exec("id -un")).await.expect("exec");
    assert_eq!(output.stdout_text(), "pwuser\n");
    assert_eq!(output.exit, ExitStatus::Code(0));
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn wrong_password_fails_cleanly() {
    let e2e = E2e::new();
    let interaction = Scripted::trusting().with_password("definitely-wrong");
    let error = within(
        "connect",
        Session::connect(e2e.russh_pw_target(), &interaction),
    )
    .await
    .expect_err("a wrong password must not authenticate");
    match &error {
        Error::AuthFailed { tried, .. } => {
            assert_eq!(tried, &[AuthMethod::Password], "{error}");
        }
        other => panic!("expected AuthFailed, got {other:?}"),
    }
    // The prompt is repeated (three attempts by default), then it gives up.
    assert_eq!(interaction.asked("password"), 3, "{:?}", interaction.log());
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn cancelled_password_prompt_fails_without_hanging() {
    let e2e = E2e::new();
    let interaction = Scripted::trusting(); // no password to give
    let error = within(
        "connect",
        Session::connect(e2e.russh_pw_target(), &interaction),
    )
    .await
    .expect_err("no password, no login");
    assert!(matches!(error, Error::AuthFailed { .. }), "{error:?}");
    assert_eq!(interaction.asked("password"), 1);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn chain_falls_through_to_password_when_keys_are_not_accepted() {
    let e2e = E2e::new();
    // Offer the test key too: pwuser does not accept public keys, so the
    // engine must skip straight to the password.
    let params = e2e.russh_pw_target().identity_file(&e2e.key);
    let interaction = e2e.pw_scripted();
    let session = within("connect", Session::connect(params, &interaction))
        .await
        .expect("connect");
    assert_eq!(session.auth_method(), &AuthMethod::Password);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn sftp_session_with_password() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_pw_target(), &e2e.pw_scripted()),
    )
    .await
    .expect("connect");
    let sftp = within("sftp", session.sftp()).await.expect("sftp");
    assert_eq!(sftp.home().await.expect("home"), "/home/pwuser");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn tunnel_with_password() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_pw_target(), &e2e.pw_scripted()),
    )
    .await
    .expect("connect");
    let forward = session
        .forward_local("127.0.0.1", 0, "127.0.0.1", easyssh_e2e::ECHO_PORT)
        .await
        .expect("forward");
    let port = forward.local_addr().unwrap().port();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    easyssh_e2e::russh_support::echo_roundtrip(&mut stream, "pw-tunnel").await;
    forward.stop().await.unwrap();
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn rejected_host_key_stops_before_any_password_is_asked() {
    let e2e = E2e::new();
    let interaction = Scripted::new(Decision::Reject).with_password("never-sent");
    let error = within(
        "connect",
        Session::connect(e2e.russh_pw_target(), &interaction),
    )
    .await
    .expect_err("rejected key");
    assert!(matches!(error, Error::HostKeyRejected { .. }), "{error:?}");
    assert_eq!(interaction.asked("password"), 0);
}
