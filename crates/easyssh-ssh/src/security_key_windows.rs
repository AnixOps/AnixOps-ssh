//! The pure parts of the Windows backend (everything that does not call
//! `webauthn.dll`), built and tested on every platform.
//!
//! The Windows WebAuthn API takes a *client data* buffer, hashes it with
//! SHA-256 itself and asks the authenticator to sign
//! `authenticatorData || SHA256(clientData)`. It does not look inside the
//! buffer, and that is what makes it usable for SSH: when the buffer is the
//! SSH message itself (`clientDataJSON = message`), the authenticator signs
//! `SHA256(application) || flags || counter || SHA256(message)`, which is
//! exactly the signed blob of a plain `sk-ssh-ed25519@openssh.com` or
//! `sk-ecdsa-sha2-nistp256@openssh.com` signature. OpenSSH for Windows does
//! the same through libfido2's Windows backend. So no webauthn-style
//! signature (and no server setting for it) is needed: servers see ordinary
//! sk signatures, for both key types.

#![cfg_attr(not(windows), allow(dead_code))]

use crate::security_key::{SkAssertion, SkError};

/// Authenticator data flag: attested credential data follows (not part of an
/// assertion).
const FLAG_ATTESTED_CREDENTIAL: u8 = 0x40;
/// Authenticator data flag: extension data follows the counter.
const FLAG_EXTENSION_DATA: u8 = 0x80;

/// The flags byte and the counter of authenticator data
/// (`rpIdHash(32) || flags(1) || counter(4)`). An sk signature has no room
/// for anything after the counter, so extension data is refused: the signature
/// would not verify on the server.
pub fn parse_authenticator_data(data: &[u8]) -> Option<(u8, u32)> {
    if data.len() != 37 {
        return None;
    }
    let flags = data[32];
    if flags & (FLAG_ATTESTED_CREDENTIAL | FLAG_EXTENSION_DATA) != 0 {
        return None;
    }
    Some((flags, u32::from_be_bytes(data[33..37].try_into().ok()?)))
}

/// An assertion from what the Windows WebAuthn API returned. `signature` is
/// the raw signature (Ed25519) or the ASN.1 DER one (ECDSA), as in CTAP2.
pub fn assertion_from_windows(
    authenticator_data: &[u8],
    signature: &[u8],
) -> Result<SkAssertion, SkError> {
    let (flags, counter) = parse_authenticator_data(authenticator_data).ok_or_else(|| {
        SkError::Device("Windows WebAuthn returned unusable authenticator data".into())
    })?;
    if signature.is_empty() {
        return Err(SkError::Device(
            "Windows WebAuthn returned no signature".into(),
        ));
    }
    Ok(SkAssertion {
        signature: signature.to_vec(),
        flags,
        counter,
    })
}

/// Maps the `HRESULT` of `WebAuthNAuthenticatorGetAssertion` to what the
/// caller can act on. The Windows dialog handles PINs and retries itself, so
/// only a few outcomes matter.
pub fn classify_hresult(hresult: i32) -> SkError {
    const NTE_NOT_FOUND: u32 = 0x8009_0011;
    const NTE_USER_CANCELLED: u32 = 0x8009_0036;
    const NTE_NOT_SUPPORTED: u32 = 0x8009_0029;
    // HRESULT_FROM_WIN32 of ERROR_CANCELLED and ERROR_TIMEOUT.
    const CANCELLED: u32 = 0x8007_04C7;
    const TIMEOUT: u32 = 0x8007_05B4;
    match hresult as u32 {
        NTE_NOT_FOUND => SkError::NoCredential,
        NTE_USER_CANCELLED | CANCELLED | TIMEOUT => SkError::NotTouched,
        NTE_NOT_SUPPORTED => SkError::Device(
            "Windows WebAuthn does not support this security key or credential".into(),
        ),
        other => SkError::Device(format!("Windows WebAuthn error 0x{other:08X}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security_key::{encode_signature, SkAlgorithm, SkKeyInfo};
    use russh::keys::signature::Verifier;
    use russh::keys::ssh_key::public::KeyData;
    use russh::keys::ssh_key::{PublicKey, Signature};
    use sha2::{Digest, Sha256};

    const APPLICATION: &str = "ssh:";

    fn put(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    }

    fn authenticator_data(flags: u8, counter: u32) -> Vec<u8> {
        let mut data = Sha256::digest(APPLICATION.as_bytes()).to_vec();
        data.push(flags);
        data.extend_from_slice(&counter.to_be_bytes());
        data
    }

    /// What the Windows API makes of `clientDataJSON = message`: the device
    /// signs `authenticatorData || SHA256(message)`.
    fn signed_blob(auth_data: &[u8], message: &[u8]) -> Vec<u8> {
        let mut signed = auth_data.to_vec();
        signed.extend_from_slice(&Sha256::digest(message));
        signed
    }

    fn info(algorithm: SkAlgorithm) -> SkKeyInfo {
        SkKeyInfo {
            algorithm,
            application: APPLICATION.into(),
            key_handle: vec![1, 2, 3],
            flags: 0x01,
        }
    }

    fn ecdsa_public(key: &p256::ecdsa::SigningKey) -> PublicKey {
        let point = key.verifying_key().to_sec1_point(false);
        let mut blob = Vec::new();
        for part in [
            SkAlgorithm::EcdsaP256.name().as_bytes(),
            b"nistp256",
            point.as_bytes(),
            APPLICATION.as_bytes(),
        ] {
            put(&mut blob, part);
        }
        PublicKey::from_bytes(&blob).unwrap()
    }

    fn ed25519_public(key: &ed25519_dalek::SigningKey) -> PublicKey {
        let mut blob = Vec::new();
        for part in [
            SkAlgorithm::Ed25519.name().as_bytes(),
            key.verifying_key().as_bytes(),
            APPLICATION.as_bytes(),
        ] {
            put(&mut blob, part);
        }
        PublicKey::from_bytes(&blob).unwrap()
    }

    /// Windows answers for an ECDSA credential; the assembled SSH signature
    /// verifies with `ssh-key` over the plain message, with several keys so
    /// that DER integers of both lengths (high bit set or not) occur.
    #[test]
    fn ecdsa_answers_become_verifying_sk_signatures() {
        use p256::ecdsa::signature::Signer;
        let message = b"session id and userauth request";
        for seed in 1u8..=12 {
            let key = p256::ecdsa::SigningKey::from_slice(&[seed; 32]).unwrap();
            let auth_data = authenticator_data(0x05, 41 + seed as u32);
            let der: p256::ecdsa::Signature = key.sign(&signed_blob(&auth_data, message));
            let assertion = assertion_from_windows(&auth_data, der.to_der().as_bytes()).unwrap();
            assert_eq!(
                (assertion.flags, assertion.counter),
                (0x05, 41 + seed as u32)
            );
            let wire = encode_signature(&info(SkAlgorithm::EcdsaP256), &assertion).unwrap();
            let signature = Signature::try_from(wire.as_slice()).unwrap();
            let public = ecdsa_public(&key);
            assert!(matches!(public.key_data(), KeyData::SkEcdsaSha2NistP256(_)));
            Verifier::verify(public.key_data(), message, &signature).unwrap();
            assert!(Verifier::verify(public.key_data(), b"another", &signature).is_err());
        }
    }

    #[test]
    fn ed25519_answers_become_verifying_sk_signatures() {
        use ed25519_dalek::Signer;
        let message = b"session id and userauth request";
        let key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
        let auth_data = authenticator_data(0x01, 3);
        let raw = key.sign(&signed_blob(&auth_data, message)).to_bytes();
        let assertion = assertion_from_windows(&auth_data, &raw).unwrap();
        let wire = encode_signature(&info(SkAlgorithm::Ed25519), &assertion).unwrap();
        let signature = Signature::try_from(wire.as_slice()).unwrap();
        let public = ed25519_public(&key);
        assert!(matches!(public.key_data(), KeyData::SkEd25519(_)));
        Verifier::verify(public.key_data(), message, &signature).unwrap();
        assert!(Verifier::verify(public.key_data(), b"another", &signature).is_err());
    }

    #[test]
    fn authenticator_data_is_split_and_checked() {
        assert_eq!(
            parse_authenticator_data(&authenticator_data(0x05, 9)),
            Some((0x05, 9))
        );
        let good = authenticator_data(0x01, 1);
        // Too short, extension data, attested credential data, trailing data.
        assert!(parse_authenticator_data(&good[..36]).is_none());
        assert!(parse_authenticator_data(&authenticator_data(0x81, 1)).is_none());
        assert!(parse_authenticator_data(&authenticator_data(0x41, 1)).is_none());
        let mut longer = good;
        longer.push(0xa0);
        assert!(parse_authenticator_data(&longer).is_none());
    }

    #[test]
    fn unusable_windows_answers_are_refused() {
        assert!(assertion_from_windows(&[0; 10], &[1]).is_err());
        assert!(assertion_from_windows(&authenticator_data(0x01, 1), &[]).is_err());
    }

    #[test]
    fn windows_errors_map_to_actionable_ones() {
        assert_eq!(
            classify_hresult(0x8009_0011_u32 as i32),
            SkError::NoCredential
        );
        assert_eq!(
            classify_hresult(0x8009_0036_u32 as i32),
            SkError::NotTouched
        );
        assert_eq!(
            classify_hresult(0x8007_05B4_u32 as i32),
            SkError::NotTouched
        );
        assert!(matches!(
            classify_hresult(0x8000_4005_u32 as i32),
            SkError::Device(text) if text.contains("0x80004005")
        ));
    }
}
