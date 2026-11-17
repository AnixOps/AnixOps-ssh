//! The app routes passwords through `SSH_ASKPASS` (`src-tauri/src/askpass.rs`:
//! `SSH_ASKPASS`, `SSH_ASKPASS_REQUIRE=force`, `DISPLAY`). That module needs
//! a Tauri `AppHandle`, so these tests exercise the OpenSSH contract it
//! relies on with a tiny shell helper in its place.
#![cfg(unix)]

use easyssh_core::sftp::{join_remote, spawn_transfer};
use easyssh_core::tunnel::spawn_tunnel;
use easyssh_core::{OpenSsh, SftpSession, Tunnel, TunnelKind};
use easyssh_e2e::{
    echo_roundtrip, finish, free_port, sample_bytes, text, unique_name, wait_for_port,
    with_timeout, E2e, Reaper, CMD_TIMEOUT, ECHO_PORT, PW_PASSWORD,
};
use std::net::TcpStream;
use std::path::Path;
use std::time::Duration;

fn prompts(log: &Path) -> String {
    std::fs::read_to_string(log).unwrap_or_default()
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn exec_with_password_from_askpass() {
    let e2e = E2e::new();
    let (env, log) = e2e.askpass(PW_PASSWORD);
    let output = e2e.exec_with_env(&e2e.alias("e2e-target-pw"), "id -un", &env);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "pwuser\n");
    let asked = prompts(&log);
    assert!(
        asked.contains("password:"),
        "askpass was not asked for a password: {asked:?}"
    );
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn wrong_password_fails_cleanly() {
    let e2e = E2e::new();
    let (env, log) = e2e.askpass("definitely-wrong");
    let output = e2e.exec_with_env(&e2e.alias("e2e-target-pw"), "id -un", &env);
    assert!(!output.status.success());
    assert!(
        text(&output.stderr).contains("Permission denied"),
        "{}",
        text(&output.stderr)
    );
    assert!(prompts(&log).contains("password:"));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn sftp_session_with_password_from_askpass() {
    let e2e = E2e::new();
    let (env, log) = e2e.askpass(PW_PASSWORD);
    let connection = e2e.alias("e2e-target-pw");
    let mut session = with_timeout(CMD_TIMEOUT, "SftpSession::connect", move || {
        SftpSession::connect(&OpenSsh, &connection, &env)
    })
    .expect("sftp connect with password");
    assert_eq!(session.home(), "/home/pwuser");
    let dir = join_remote(session.home(), &unique_name("e2e-pw"));
    session.mkdir(&dir).expect("mkdir");
    session.remove(&dir, true).expect("rmdir");
    assert!(prompts(&log).contains("password:"));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn sftp_session_with_wrong_password_errors_without_hanging() {
    let e2e = E2e::new();
    let (env, _log) = e2e.askpass("definitely-wrong");
    let connection = e2e.alias("e2e-target-pw");
    // The core waits up to five minutes for a person to type a password; a
    // rejected one must end the session at once instead.
    let result = with_timeout(Duration::from_secs(90), "SftpSession::connect", move || {
        SftpSession::connect(&OpenSsh, &connection, &env).map(|_| ())
    });
    let error = result.expect_err("a wrong password must not connect");
    println!("wrong-password sftp error: {error}");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn transfer_with_password_from_askpass() {
    let e2e = E2e::new();
    let (env, log) = e2e.askpass(PW_PASSWORD);
    let connection = e2e.alias("e2e-target-pw");
    let data = sample_bytes(50_000);
    let local = e2e.dir.path().join("pw-up.bin");
    std::fs::write(&local, &data).unwrap();
    let remote = format!("/home/pwuser/{}", unique_name("e2e-pw.bin"));

    let child = spawn_transfer(&OpenSsh, &connection, true, &local, &remote, &env).unwrap();
    let output = finish(child, CMD_TIMEOUT);
    assert!(
        output.status.success(),
        "upload failed: {}; askpass prompts: {:?}",
        text(&output.stderr),
        prompts(&log)
    );
    let back = e2e.dir.path().join("pw-down.bin");
    let child = spawn_transfer(&OpenSsh, &connection, false, &back, &remote, &env).unwrap();
    let output = finish(child, CMD_TIMEOUT);
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(std::fs::read(&back).unwrap(), data);
}

fn local_tunnel(port: u16) -> Tunnel {
    Tunnel {
        id: "e2e".into(),
        name: "e2e".into(),
        host_id: "e2e".into(),
        kind: TunnelKind::Local,
        bind_address: Some("127.0.0.1".into()),
        bind_port: port,
        target_host: Some("127.0.0.1".into()),
        target_port: Some(ECHO_PORT),
    }
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn tunnel_with_password_from_askpass() {
    let e2e = E2e::new();
    let (env, log) = e2e.askpass(PW_PASSWORD);
    let port = free_port();
    let child = spawn_tunnel(
        &OpenSsh,
        &e2e.alias("e2e-target-pw"),
        &local_tunnel(port),
        &env,
    )
    .expect("spawn");
    let mut tunnel = Reaper(child);
    wait_for_port(port, &mut tunnel.0, Duration::from_secs(30));
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    echo_roundtrip(&mut stream, "password-tunnel");
    drop(stream);
    tunnel.stop();
    assert!(prompts(&log).contains("password:"));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn tunnel_with_wrong_password_exits() {
    let e2e = E2e::new();
    let (env, _log) = e2e.askpass("definitely-wrong");
    let port = free_port();
    let child = spawn_tunnel(
        &OpenSsh,
        &e2e.alias("e2e-target-pw"),
        &local_tunnel(port),
        &env,
    )
    .expect("spawn");
    let output = finish(child, Duration::from_secs(60));
    assert!(!output.status.success());
    assert!(
        text(&output.stderr).contains("Permission denied"),
        "{}",
        text(&output.stderr)
    );
}
