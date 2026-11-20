#![cfg(unix)]

use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::E2e;
use easyssh_ssh::{AuthMethod, ExitStatus, Session};

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn exec_returns_stdout_stderr_and_exit() {
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

    let output = within(
        "exec",
        session.exec("echo e2e-exec-ok; id -un; hostname; echo oops >&2"),
    )
    .await
    .expect("exec");
    assert_eq!(output.stdout_text(), "e2e-exec-ok\nkeyuser\ntarget\n");
    assert_eq!(output.stderr_text(), "oops\n");
    assert_eq!(output.exit, ExitStatus::Code(0));
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn exec_propagates_remote_exit_code() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    let output = within("exec", session.exec("echo before; exit 7"))
        .await
        .expect("exec");
    assert_eq!(output.exit, ExitStatus::Code(7));
    assert!(!output.exit.success());
    assert_eq!(output.stdout_text(), "before\n");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn many_channels_share_one_connection() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    // One connection, several channels at once: the sshd of the container
    // reports the same client port for all of them.
    let probe = "echo $SSH_CONNECTION | cut -d' ' -f2; sleep 1";
    let (a, b, c) = within("parallel exec", async {
        tokio::join!(
            session.exec(probe),
            session.exec(probe),
            session.exec(probe)
        )
    })
    .await;
    let ports: Vec<String> = [a, b, c]
        .into_iter()
        .map(|r| r.expect("exec").stdout_text())
        .collect();
    assert!(ports[0].trim().parse::<u16>().is_ok(), "{ports:?}");
    assert_eq!(ports[0], ports[1]);
    assert_eq!(ports[1], ports[2]);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn exec_after_disconnect_fails() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    session.disconnect().await.expect("disconnect");
    let result = within("exec", session.exec("true")).await;
    assert!(result.is_err(), "{result:?}");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn sessions_past_max_sessions_are_refused_with_their_own_error() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    // OpenSSH's default MaxSessions is 10.
    let mut shells = Vec::new();
    for _ in 0..10 {
        let shell = within(
            "shell",
            session.shell(easyssh_ssh::ShellOptions {
                term: "xterm".into(),
                cols: 80,
                rows: 24,
            }),
        )
        .await
        .expect("a shell within MaxSessions");
        shells.push(shell);
    }
    let error = within("one too many", session.exec("true"))
        .await
        .expect_err("the eleventh session is refused");
    assert!(
        matches!(error, easyssh_ssh::Error::ChannelRefused(_)),
        "{error:?}"
    );
    // The connection itself is fine.
    within("ping", session.ping()).await.expect("ping");
}
