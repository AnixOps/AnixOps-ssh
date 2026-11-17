#![cfg(unix)]

use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::{sample_bytes, unique_name, E2e, ECHO_PORT};
use easyssh_ssh::{Decision, Error, Session};

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn the_bastion_is_a_different_machine() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_bastion(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    let output = within("exec", session.exec("hostname"))
        .await
        .expect("exec");
    assert_eq!(output.stdout_text(), "bastion\n");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn exec_through_the_bastion() {
    let e2e = E2e::new();
    let interaction = Scripted::trusting();
    let session = within(
        "connect via bastion",
        Session::connect_via(
            &[e2e.russh_bastion()],
            e2e.russh_target_behind_bastion(),
            &interaction,
        ),
    )
    .await
    .expect("connect via bastion");
    let output = within("exec", session.exec("hostname"))
        .await
        .expect("exec");
    assert_eq!(output.stdout_text(), "target\n");
    // Each hop verified its own host key.
    assert_eq!(interaction.asked("hostkey 127.0.0.1:2223 unknown"), 1);
    assert_eq!(interaction.asked("hostkey target:22 unknown"), 1);
    assert_eq!(session.host(), "target");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn sftp_through_the_bastion() {
    let e2e = E2e::new();
    let session = within(
        "connect via bastion",
        Session::connect_via(
            &[e2e.russh_bastion()],
            e2e.russh_target_behind_bastion(),
            &Scripted::trusting(),
        ),
    )
    .await
    .expect("connect via bastion");
    let sftp = within("sftp", session.sftp()).await.expect("sftp");
    assert_eq!(sftp.home().await.unwrap(), "/home/keyuser");

    let data = sample_bytes(20_000);
    let remote = format!("/home/keyuser/{}.bin", unique_name("e2e-russh-jump"));
    within("transfer", async {
        sftp.write_file(&remote, &data).await.expect("upload");
        assert_eq!(sftp.read_file(&remote).await.expect("download"), data);
        sftp.remove_file(&remote).await.expect("rm");
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn forward_through_the_bastion() {
    let e2e = E2e::new();
    let session = within(
        "connect via bastion",
        Session::connect_via(
            &[e2e.russh_bastion()],
            e2e.russh_target_behind_bastion(),
            &Scripted::trusting(),
        ),
    )
    .await
    .expect("connect via bastion");
    let forward = session
        .forward_local("127.0.0.1", 0, "127.0.0.1", ECHO_PORT)
        .await
        .expect("forward");
    let port = forward.local_addr().unwrap().port();
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    easyssh_e2e::russh_support::echo_roundtrip(&mut stream, "via-bastion").await;
    forward.stop().await.unwrap();
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_rejected_hop_key_stops_the_chain() {
    let e2e = E2e::new();
    let error = within(
        "connect via bastion",
        Session::connect_via(
            &[e2e.russh_bastion()],
            e2e.russh_target_behind_bastion(),
            &Scripted::new(Decision::Reject),
        ),
    )
    .await
    .expect_err("rejected");
    match error {
        Error::HostKeyRejected { host, port, .. } => {
            assert_eq!((host.as_str(), port), ("127.0.0.1", 2223));
        }
        other => panic!("expected HostKeyRejected for the bastion, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn an_unreachable_target_behind_the_bastion_is_a_channel_error() {
    let e2e = E2e::new();
    let mut target = e2e.russh_target_behind_bastion();
    target.port = 2; // nothing listens there
    let error = within(
        "connect via bastion",
        Session::connect_via(&[e2e.russh_bastion()], target, &Scripted::trusting()),
    )
    .await
    .expect_err("unreachable");
    assert!(matches!(error, Error::Channel(_)), "{error:?}");
}
