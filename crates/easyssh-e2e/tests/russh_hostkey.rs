#![cfg(unix)]

use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::{E2e, TARGET_PORT};
use easyssh_ssh::{Decision, Error, KnownHostsFile, Session};
use std::sync::Arc;

/// An ed25519 public key that no container has.
const OTHER_KEY: &str = "AAAAC3NzaC1lZDI1NTE5AAAAIKxXnqrgYJGHpjbb27Ddo024CtOCBG5K2xV1JzRg3eRu";

fn entry_name() -> String {
    format!("[127.0.0.1]:{TARGET_PORT}")
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn trust_on_first_use_then_silent() {
    let e2e = E2e::new();
    let first = Scripted::trusting();
    within("first", Session::connect(e2e.russh_target(), &first))
        .await
        .expect("first connect");
    assert_eq!(
        first.log(),
        [format!("hostkey 127.0.0.1:{TARGET_PORT} unknown")]
    );

    let stored = std::fs::read_to_string(e2e.russh_known_hosts()).expect("known_hosts written");
    assert!(
        stored.starts_with(&format!("{} ssh-", entry_name())),
        "{stored}"
    );

    // Second time the key is on record: nothing is asked, even by an
    // Interaction that would reject everything.
    let second = Scripted::new(Decision::Reject);
    within("second", Session::connect(e2e.russh_target(), &second))
        .await
        .expect("second connect");
    assert!(second.log().is_empty(), "{:?}", second.log());
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn rejected_key_is_not_recorded() {
    let e2e = E2e::new();
    let error = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::new(Decision::Reject)),
    )
    .await
    .expect_err("rejected");
    assert!(matches!(error, Error::HostKeyRejected { .. }), "{error:?}");
    assert!(!e2e.russh_known_hosts().exists());
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn accept_once_records_nothing() {
    let e2e = E2e::new();
    let interaction = Scripted::new(Decision::AcceptOnce);
    within(
        "connect",
        Session::connect(e2e.russh_target(), &interaction),
    )
    .await
    .expect("connect");
    assert!(!e2e.russh_known_hosts().exists());
    within("again", Session::connect(e2e.russh_target(), &interaction))
        .await
        .expect("connect again");
    assert_eq!(interaction.asked("hostkey"), 2);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn changed_key_is_detected_and_never_auto_accepted() {
    let e2e = E2e::new();
    within(
        "first",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("first connect");

    // Swap the stored key for another one, as if the server had changed.
    let path = e2e.russh_known_hosts();
    let stored = std::fs::read_to_string(&path).unwrap();
    let mut fields: Vec<&str> = stored.split_whitespace().collect();
    assert_eq!(fields.len(), 3, "{stored}");
    assert_ne!(fields[2], OTHER_KEY);
    fields[2] = OTHER_KEY;
    let tampered = format!("{}\n", fields.join(" "));
    std::fs::write(&path, &tampered).unwrap();

    // An Interaction that says yes to everything but a replacement.
    for decision in [
        Decision::AcceptAndSave,
        Decision::AcceptOnce,
        Decision::Reject,
    ] {
        let interaction = Scripted::new(decision);
        let error = within(
            "connect",
            Session::connect(e2e.russh_target(), &interaction),
        )
        .await
        .expect_err("a changed key must be refused");
        match error {
            Error::HostKeyMismatch {
                host,
                port,
                expected,
                ..
            } => {
                assert_eq!(host, "127.0.0.1");
                assert_eq!(port, TARGET_PORT);
                assert_eq!(expected.len(), 1);
            }
            other => panic!("expected HostKeyMismatch with {decision:?}, got {other:?}"),
        }
        assert_eq!(
            interaction.log(),
            [format!("hostkey 127.0.0.1:{TARGET_PORT} changed")]
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), tampered);
    }

    // The user can replace the key on purpose; afterwards it is trusted.
    within(
        "replace",
        Session::connect(e2e.russh_target(), &Scripted::new(Decision::ReplaceAndSave)),
    )
    .await
    .expect("replace");
    assert_ne!(std::fs::read_to_string(&path).unwrap(), tampered);
    let quiet = Scripted::new(Decision::Reject);
    within(
        "trusted again",
        Session::connect(e2e.russh_target(), &quiet),
    )
    .await
    .expect("trusted again");
    assert!(quiet.log().is_empty());
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn hashed_known_hosts_work() {
    let e2e = E2e::new();
    let store = Arc::new(KnownHostsFile::new(e2e.russh_known_hosts()).hash_new_entries(true));
    within(
        "first",
        Session::connect(
            e2e.russh_target().known_hosts(store.clone()),
            &Scripted::trusting(),
        ),
    )
    .await
    .expect("first connect");
    let stored = std::fs::read_to_string(e2e.russh_known_hosts()).unwrap();
    assert!(stored.starts_with("|1|"), "{stored}");
    assert!(!stored.contains("127.0.0.1"));
    let quiet = Scripted::new(Decision::Reject);
    within(
        "second",
        Session::connect(e2e.russh_target().known_hosts(store), &quiet),
    )
    .await
    .expect("second connect");
    assert!(quiet.log().is_empty());
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn host_keys_are_recorded_per_port() {
    let e2e = E2e::new();
    within(
        "target",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("target");
    // The bastion is another host:port; it is new even though the file has
    // an entry for the target.
    let interaction = Scripted::trusting();
    within(
        "bastion",
        Session::connect(e2e.russh_bastion(), &interaction),
    )
    .await
    .expect("bastion");
    assert_eq!(interaction.asked("hostkey 127.0.0.1:2223 unknown"), 1);
    let stored = std::fs::read_to_string(e2e.russh_known_hosts()).unwrap();
    assert_eq!(stored.lines().count(), 2, "{stored}");
}
