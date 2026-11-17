#![cfg(unix)]

use easyssh_core::sftp::{join_remote, spawn_transfer};
use easyssh_core::{Connection, OpenSsh, SftpSession};
use easyssh_e2e::{finish, sample_bytes, text, unique_name, with_timeout, E2e, CMD_TIMEOUT};

/// A connection to the target that jumps through the bastion because the
/// connection itself says so (`proxy_jump`), not the ssh config.
fn jump_connection() -> Connection {
    Connection {
        proxy_jump: Some("e2e-bastion".into()),
        ..Connection::alias("via bastion", "e2e-target-internal")
    }
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn exec_through_proxy_jump_alias_from_config() {
    let e2e = E2e::new();
    let output = e2e.exec(&e2e.alias("e2e-target-via-bastion"), "hostname");
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "target\n");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn exec_through_connection_proxy_jump() {
    let e2e = E2e::new();
    let output = e2e.exec(&jump_connection(), "hostname");
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "target\n");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn the_bastion_is_a_different_machine() {
    let e2e = E2e::new();
    let output = e2e.exec(&e2e.alias("e2e-bastion"), "hostname");
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "bastion\n");
}

fn sftp_roundtrip(e2e: &E2e, connection: Connection) {
    let for_session = connection.clone();
    let mut session = with_timeout(CMD_TIMEOUT, "SftpSession::connect", move || {
        SftpSession::connect(&OpenSsh, &for_session, &[])
    })
    .expect("sftp connect through the jump host");
    assert_eq!(session.home(), "/home/keyuser");

    let dir = join_remote(session.home(), &unique_name("e2e-jump"));
    session.mkdir(&dir).expect("mkdir");

    let data = sample_bytes(20_000);
    let local = e2e.dir.path().join("jump-up.bin");
    std::fs::write(&local, &data).unwrap();
    let remote = join_remote(&dir, "file.bin");
    let child = spawn_transfer(&OpenSsh, &connection, true, &local, &remote, &[]).unwrap();
    let output = finish(child, CMD_TIMEOUT);
    assert!(output.status.success(), "upload: {}", text(&output.stderr));

    let back = e2e.dir.path().join("jump-down.bin");
    let child = spawn_transfer(&OpenSsh, &connection, false, &back, &remote, &[]).unwrap();
    let output = finish(child, CMD_TIMEOUT);
    assert!(
        output.status.success(),
        "download: {}",
        text(&output.stderr)
    );
    assert_eq!(std::fs::read(&back).unwrap(), data);

    session.remove(&remote, false).expect("rm");
    session.remove(&dir, true).expect("rmdir");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn sftp_through_proxy_jump_alias_from_config() {
    let e2e = E2e::new();
    sftp_roundtrip(&e2e, e2e.alias("e2e-target-via-bastion"));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn sftp_through_connection_proxy_jump() {
    let e2e = E2e::new();
    sftp_roundtrip(&e2e, jump_connection());
}
