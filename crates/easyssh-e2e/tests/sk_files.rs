#![cfg(unix)]

//! The committed `e2e/id_e2e_sk.pub` must be what the seeds produce, or the
//! container would authorize other keys than the tests use. Needs no Docker.

use easyssh_e2e::sk::{authorized_file, e2e_security_keys};

#[test]
fn the_authorized_sk_keys_are_the_ones_the_seeds_make() {
    let text = std::fs::read_to_string(authorized_file()).expect("e2e/id_e2e_sk.pub");
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let (_, ed25519, ecdsa) = e2e_security_keys();
    let expected = [ed25519.private, ecdsa.private];
    assert_eq!(lines.len(), expected.len(), "{lines:?}");
    for (line, key) in lines.iter().zip(expected) {
        // `<type> <base64> <comment>`: the comment may differ.
        let wanted = key.public_key().to_openssh().expect("public key line");
        let head = |text: &str| {
            text.split_whitespace()
                .take(2)
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            head(line),
            head(&wanted),
            "e2e/id_e2e_sk.pub is stale; regenerate it from the seeds in crates/easyssh-e2e/src/sk.rs"
        );
    }
}
