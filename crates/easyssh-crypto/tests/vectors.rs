//! Committed test vectors for cross-implementation checks (mobile, server).
//!
//! Each `tests/vectors/*.json` file is recomputed from its fixed inputs and
//! compared byte for byte; the committed outputs are then also fed to the
//! decrypting / verifying side. To regenerate after an intentional format
//! change (which needs a new format version byte!):
//! `UPDATE_VECTORS=1 cargo test -p easyssh-crypto --test vectors`.

use easyssh_crypto::keys::seal_vault_key_with;
use easyssh_crypto::record::encrypt_record_with_nonce;
use easyssh_crypto::*;
use serde_json::{json, Value};
use std::path::PathBuf;

fn h(b: &[u8]) -> String {
    hex::encode(b)
}

fn unh<const N: usize>(s: &str) -> [u8; N] {
    hex::decode(s).unwrap().try_into().unwrap()
}

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/vectors")
        .join(name)
}

/// Compare `actual` with the committed file (or rewrite it when updating).
fn check(name: &str, actual: &Value) -> Value {
    let p = path(name);
    if std::env::var_os("UPDATE_VECTORS").is_some() {
        std::fs::write(&p, serde_json::to_string_pretty(actual).unwrap() + "\n").unwrap();
    }
    let committed: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(&committed, actual, "{name} differs from recomputed output");
    committed
}

#[test]
fn kdf_vectors() {
    let cases = [
        ("test-params", KdfParams::insecure_for_tests(), [0x11u8; 16]),
        ("desktop-v1", KdfParams::DESKTOP_V1, [0x22u8; 16]),
    ];
    let mut out = Vec::new();
    for (name, params, salt) in cases {
        let header = KdfHeader { params, salt };
        let s = derive_account_secrets("correct horse battery staple".as_bytes(), &header).unwrap();
        let pair = AccountKeyPair::from_account_key(&s.account_key).unwrap();
        out.push(json!({
            "name": name,
            "passphrase_utf8": "correct horse battery staple",
            "header_hex": h(&header.to_bytes()),
            "account_key_hex": h(s.account_key.expose()),
            "auth_secret_hex": h(s.auth_secret.expose()),
            "account_x25519_public_hex": h(&pair.sealing.public().0),
            "account_ed25519_public_hex": h(&pair.signing.public().to_bytes()),
        }));
    }
    let v = check("kdf.json", &Value::Array(out));
    for c in v.as_array().unwrap() {
        let header =
            KdfHeader::from_bytes(&hex::decode(c["header_hex"].as_str().unwrap()).unwrap())
                .unwrap();
        let s = derive_account_secrets(c["passphrase_utf8"].as_str().unwrap().as_bytes(), &header)
            .unwrap();
        assert_eq!(h(s.account_key.expose()), c["account_key_hex"]);
        assert_ne!(s.account_key.expose(), s.auth_secret.expose());
    }
}

#[test]
fn record_vectors() {
    let vv = VersionVector::from_entries([("device-b", 2), ("device-a", 5)]);
    let key = VaultKey::from_bytes([0x01; 32]);
    let nonce = [0x02u8; 24];
    let aad = Aad {
        record_id: "11111111-1111-4111-8111-111111111111",
        vault_id: "22222222-2222-4222-8222-222222222222",
        schema_version: 1,
        version_vector_digest: vv.digest(),
    };
    let plaintext = br#"{"label":"prod","host":"example.com"}"#;
    let blob = encrypt_record_with_nonce(&key, &nonce, plaintext, &aad).unwrap();
    let v = check(
        "record.json",
        &json!({
            "key_hex": h(key.expose()),
            "nonce_hex": h(&nonce),
            "plaintext_hex": h(plaintext),
            "record_id": aad.record_id,
            "vault_id": aad.vault_id,
            "schema_version": aad.schema_version,
            "version_vector": [["device-a", 5], ["device-b", 2]],
            "version_vector_canonical_hex": h(&vv.to_canonical_bytes()),
            "version_vector_digest_hex": h(&vv.digest()),
            "aad_hex": h(&aad.to_bytes()),
            "blob_hex": h(&blob),
        }),
    );
    let committed = hex::decode(v["blob_hex"].as_str().unwrap()).unwrap();
    assert_eq!(decrypt_record(&key, &committed, &aad).unwrap(), plaintext);
}

#[test]
fn sealed_vault_key_vectors() {
    let acct = AccountKeyPair::from_account_key(&AccountKey::from_bytes([0x03; 32])).unwrap();
    let vk = VaultKey::from_bytes([0x04; 32]);
    let eph = [0x05u8; 32];
    let nonce = [0x06u8; 24];
    let sealed = seal_vault_key_with(&vk, &acct.sealing.public(), &eph, &nonce).unwrap();
    let v = check(
        "sealed_vault_key.json",
        &json!({
            "account_key_hex": h(&[0x03; 32]),
            "recipient_x25519_public_hex": h(&acct.sealing.public().0),
            "vault_key_hex": h(vk.expose()),
            "ephemeral_secret_hex": h(&eph),
            "nonce_hex": h(&nonce),
            "sealed_hex": h(&sealed),
        }),
    );
    let committed = hex::decode(v["sealed_hex"].as_str().unwrap()).unwrap();
    assert_eq!(open_vault_key(&committed, &acct.sealing).unwrap(), vk);
}

#[test]
fn envelope_vectors() {
    let acct = AccountKeyPair::from_account_key(&AccountKey::from_bytes([0x03; 32])).unwrap();
    let device = DeviceSigningKey::from_seed([0x07; 32]);
    let cert = certify_device(
        &acct.signing,
        &device.public(),
        "Aiko's laptop",
        1_700_000_000,
    );
    let mut envs = Vec::new();
    let mut verify = Vec::new();
    for (deleted, ct) in [(false, vec![0xde, 0xad, 0xbe, 0xef]), (true, vec![])] {
        let env = sign_envelope(
            EnvelopeFields {
                record_id: "rec-1".into(),
                vault_id: "vault-1".into(),
                version_vector: VersionVector::from_entries([("device-b", 2), ("device-a", 5)]),
                deleted,
                ciphertext: ct,
            },
            &device,
        );
        envs.push(json!({
            "record_id": env.record_id,
            "vault_id": env.vault_id,
            "version_vector": [["device-a", 5], ["device-b", 2]],
            "deleted": env.deleted,
            "ciphertext_hex": h(&env.ciphertext),
            "signer_hex": h(&env.signer),
            "signing_bytes_hex": h(&env.signing_bytes()),
            "signature_hex": h(&env.signature),
        }));
        verify.push(env);
    }
    let v = check(
        "envelope.json",
        &json!({
            "account_key_hex": h(&[0x03; 32]),
            "account_ed25519_public_hex": h(&acct.signing.public().to_bytes()),
            "device_seed_hex": h(&[0x07; 32]),
            "device_public_hex": h(&device.public()),
            "cert": {
                "device_name": cert.device_name,
                "issued_at": cert.issued_at,
                "bytes_hex": h(&cert.to_bytes()),
            },
            "envelopes": envs,
        }),
    );
    // Verify purely from committed data.
    let account_pub = ed25519_dalek::VerifyingKey::from_bytes(&unh(v
        ["account_ed25519_public_hex"]
        .as_str()
        .unwrap()))
    .unwrap();
    let cert =
        DeviceCert::from_bytes(&hex::decode(v["cert"]["bytes_hex"].as_str().unwrap()).unwrap())
            .unwrap();
    for e in v["envelopes"].as_array().unwrap() {
        let env = Envelope {
            record_id: e["record_id"].as_str().unwrap().into(),
            vault_id: e["vault_id"].as_str().unwrap().into(),
            version_vector: VersionVector::from_entries(
                e["version_vector"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| (p[0].as_str().unwrap(), p[1].as_u64().unwrap())),
            ),
            deleted: e["deleted"].as_bool().unwrap(),
            ciphertext: hex::decode(e["ciphertext_hex"].as_str().unwrap()).unwrap(),
            signer: unh(e["signer_hex"].as_str().unwrap()),
            signature: unh(e["signature_hex"].as_str().unwrap()),
        };
        verify_envelope(&env, &cert, &account_pub).unwrap();
    }
    assert_eq!(verify.len(), 2);
}

#[test]
fn field_cipher_vectors() {
    let c = DeviceFieldCipher::new(&DeviceKey::from_bytes([0x08; 32])).unwrap();
    let nonce = [0x09u8; 24];
    let blob = c
        .encrypt_with_nonce(&nonce, b"hunter2", b"hosts/1/password")
        .unwrap();
    let v = check(
        "field.json",
        &json!({
            "device_key_hex": h(&[0x08; 32]),
            "nonce_hex": h(&nonce),
            "aad_utf8": "hosts/1/password",
            "plaintext_utf8": "hunter2",
            "blob_hex": h(&blob),
        }),
    );
    let committed = hex::decode(v["blob_hex"].as_str().unwrap()).unwrap();
    assert_eq!(
        c.decrypt(&committed, b"hosts/1/password").unwrap(),
        b"hunter2"
    );
}
