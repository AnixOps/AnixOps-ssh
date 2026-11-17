#![cfg(unix)]

use easyssh_core::{Connection, ConnectionTarget};
use easyssh_e2e::{text, E2e, TARGET_PORT};

/// Proves the harness never reads the real user's configuration.
#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn config_is_isolated_from_the_real_home() {
    let e2e = E2e::new();
    let output = std::process::Command::new("ssh")
        .args(["-G", "e2e-target"])
        .output()
        .expect("run ssh -G");
    let resolved = text(&output.stdout);
    let known_hosts = e2e.dir.path().join("known_hosts");
    assert!(
        resolved.contains(&format!("userknownhostsfile {}", known_hosts.display())),
        "ssh does not use the test config:\n{resolved}"
    );
    assert!(resolved.contains("identityfile"), "{resolved}");
    assert!(resolved.contains("user keyuser"), "{resolved}");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn exec_through_alias_returns_stdout() {
    let e2e = E2e::new();
    let output = e2e.exec(
        &e2e.alias("e2e-target"),
        "echo e2e-exec-ok; id -un; hostname",
    );
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "e2e-exec-ok\nkeyuser\ntarget\n");
    assert_eq!(text(&output.stderr), "");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn exec_propagates_remote_exit_code() {
    let e2e = E2e::new();
    let output = e2e.exec(&e2e.alias("e2e-target"), "exit 7");
    assert_eq!(output.status.code(), Some(7));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn exec_through_endpoint_target() {
    let e2e = E2e::new();
    let connection = Connection {
        target: ConnectionTarget::Endpoint {
            hostname: "127.0.0.1".into(),
            username: Some("keyuser".into()),
            port: TARGET_PORT,
        },
        ..Connection::alias("endpoint", "unused")
    };
    let output = e2e.exec(&connection, "id -un");
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert_eq!(text(&output.stdout), "keyuser\n");
}
