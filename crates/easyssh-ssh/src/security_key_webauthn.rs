//! The webauthn flavour of `sk-ecdsa-sha2-nistp256@openssh.com` signatures,
//! and the pure parts of the Windows backend (everything that does not call
//! `webauthn.dll`), so they are built and tested on every platform.
//!
//! The Windows WebAuthn API does not sign `SHA256(message)`. It takes a
//! *client data JSON*, hashes that itself and asks the authenticator to sign
//! `authenticatorData || SHA256(clientDataJSON)`. OpenSSH accepts such a
//! signature as `webauthn-sk-ecdsa-sha2-nistp256@openssh.com` (PROTOCOL.u2f,
//! ssh-ecdsa-sk.c). The client data has a fixed preamble the server checks
//! byte for byte:
//!
//! ```text
//! {"type":"webauthn.get","challenge":"<base64url(message)>","origin":"<origin>"...
//! ```
//!
//! and the server rebuilds the signed bytes as `SHA256(application) || flags ||
//! counter || extensions || SHA256(clientData)`, so the relying party id given
//! to Windows must be the key's application (`ssh:`). The userauth request
//! must also name `webauthn-sk-ecdsa-sha2-nistp256@openssh.com` as its
//! public key algorithm (the key blob stays the `sk-ecdsa` one), which is why
//! [`rewrite_userauth_algorithm`] exists. Ed25519 keys have no webauthn form in
//! OpenSSH, so Windows cannot sign for them.
//!
//! Only the Windows backend uses most of this; it is compiled everywhere so
//! the encoding is tested on every platform.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::security_key::{SkAssertion, SkError, WebauthnData};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

/// The public key algorithm and signature type of webauthn ECDSA signatures.
pub const WEBAUTHN_ECDSA_ALGORITHM: &str = "webauthn-sk-ecdsa-sha2-nistp256@openssh.com";

/// Authenticator data flag: extension data follows the counter.
const FLAG_EXTENSION_DATA: u8 = 0x80;
/// Authenticator data flag: attested credential data (not allowed in an
/// assertion, and refused by OpenSSH).
const FLAG_ATTESTED_CREDENTIAL: u8 = 0x40;

/// The client data JSON for `message`. The server compares it against the
/// same preamble; nothing else is added.
pub fn client_data(message: &[u8], origin: &str) -> Result<Vec<u8>, SkError> {
    // The server refuses a quote in the origin, and anything that needs JSON
    // escaping would make the text differ from what it expects.
    if origin
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_control())
    {
        return Err(SkError::Device(format!(
            "the key's application {origin:?} cannot be used with Windows WebAuthn"
        )));
    }
    let mut out = String::from(r#"{"type":"webauthn.get","challenge":""#);
    out.push_str(&URL_SAFE_NO_PAD.encode(message));
    out.push_str(r#"","origin":""#);
    out.push_str(origin);
    out.push_str("\"}");
    Ok(out.into_bytes())
}

/// The parts of CTAP2 authenticator data an SSH signature carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatorData<'a> {
    pub flags: u8,
    pub counter: u32,
    pub extensions: &'a [u8],
}

/// Splits `rpIdHash(32) || flags(1) || counter(4) || extensions`.
pub fn parse_authenticator_data(data: &[u8]) -> Option<AuthenticatorData<'_>> {
    let flags = *data.get(32)?;
    let counter = u32::from_be_bytes(data.get(33..37)?.try_into().ok()?);
    let extensions = &data[37..];
    // OpenSSH refuses attested credential data, and extension data that the
    // flag does not announce (or the other way round).
    if flags & FLAG_ATTESTED_CREDENTIAL != 0
        || (flags & FLAG_EXTENSION_DATA != 0) == extensions.is_empty()
    {
        return None;
    }
    Some(AuthenticatorData {
        flags,
        counter,
        extensions,
    })
}

/// An assertion from what the Windows WebAuthn API returned: the
/// authenticator data and the DER signature, made over `client_data`.
pub fn assertion_from_webauthn(
    origin: &str,
    client_data: Vec<u8>,
    authenticator_data: &[u8],
    signature: &[u8],
) -> Result<SkAssertion, SkError> {
    let parsed = parse_authenticator_data(authenticator_data).ok_or_else(|| {
        SkError::Device("Windows WebAuthn returned unusable authenticator data".into())
    })?;
    if signature.is_empty() {
        return Err(SkError::Device(
            "Windows WebAuthn returned no signature".into(),
        ));
    }
    Ok(SkAssertion {
        signature: signature.to_vec(),
        flags: parsed.flags,
        counter: parsed.counter,
        webauthn: Some(WebauthnData {
            origin: origin.to_owned(),
            client_data,
            extensions: parsed.extensions.to_vec(),
        }),
    })
}

fn take_string(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let len = u32::from_be_bytes(data.get(..4)?.try_into().ok()?) as usize;
    let end = len.checked_add(4)?;
    Some((data.get(4..end)?, data.get(end..)?))
}

fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// Replaces the public key algorithm name in the data russh asks to be
/// signed for a public key userauth request (`string session id`, the request
/// message with `byte 1`, `string algorithm`, `string key blob`). The server
/// signs over, and checks, the algorithm name it was sent, so the request the
/// signature is attached to must carry the new name too. `None` when the data
/// is not such a request or names another algorithm than `from`.
pub fn rewrite_userauth_algorithm(to_sign: &[u8], from: &str, to: &str) -> Option<Vec<u8>> {
    const USERAUTH_REQUEST: u8 = 50;
    let (session_id, rest) = take_string(to_sign)?;
    let (&message, rest) = rest.split_first()?;
    if message != USERAUTH_REQUEST {
        return None;
    }
    let (user, rest) = take_string(rest)?;
    let (service, rest) = take_string(rest)?;
    let (method, rest) = take_string(rest)?;
    if service != b"ssh-connection" || method != b"publickey" {
        return None;
    }
    let (&has_signature, rest) = rest.split_first()?;
    if has_signature != 1 {
        return None;
    }
    let (algorithm, rest) = take_string(rest)?;
    if algorithm != from.as_bytes() {
        return None;
    }
    let mut out = Vec::with_capacity(to_sign.len() + to.len());
    put_string(&mut out, session_id);
    out.push(USERAUTH_REQUEST);
    put_string(&mut out, user);
    put_string(&mut out, service);
    put_string(&mut out, method);
    out.push(1);
    put_string(&mut out, to.as_bytes());
    out.extend_from_slice(rest);
    Some(out)
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
    use p256::ecdsa::signature::Signer;
    use russh::keys::signature::Verifier;
    use russh::keys::ssh_key::public::KeyData;
    use russh::keys::ssh_key::{PublicKey, Signature};
    use sha2::{Digest, Sha256};

    const APPLICATION: &str = "ssh:";

    fn signing_key() -> p256::ecdsa::SigningKey {
        p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).unwrap()
    }

    fn sk_public_key(application: &str) -> PublicKey {
        let point = signing_key().verifying_key().to_sec1_point(false);
        let mut blob = Vec::new();
        for part in [
            SkAlgorithm::EcdsaP256.name().as_bytes(),
            b"nistp256",
            point.as_bytes(),
            application.as_bytes(),
        ] {
            put_string(&mut blob, part);
        }
        PublicKey::from_bytes(&blob).unwrap()
    }

    fn authenticator_data(flags: u8, counter: u32, extensions: &[u8]) -> Vec<u8> {
        let mut data = Sha256::digest(APPLICATION.as_bytes()).to_vec();
        data.push(flags);
        data.extend_from_slice(&counter.to_be_bytes());
        data.extend_from_slice(extensions);
        data
    }

    /// What the Windows API does: sign `authData || SHA256(clientData)`.
    fn windows_sign(auth_data: &[u8], client_data: &[u8]) -> Vec<u8> {
        let mut signed = auth_data.to_vec();
        signed.extend_from_slice(&Sha256::digest(client_data));
        let signature: p256::ecdsa::Signature = signing_key().sign(&signed);
        signature.to_der().as_bytes().to_vec()
    }

    fn info() -> SkKeyInfo {
        SkKeyInfo {
            algorithm: SkAlgorithm::EcdsaP256,
            application: APPLICATION.into(),
            key_handle: vec![1, 2, 3],
            flags: 0x01,
        }
    }

    #[test]
    fn client_data_has_the_preamble_openssh_checks() {
        let data = client_data(&[0xfb, 0xff, 0xfe, 0x01], "ssh:").unwrap();
        // 0xfbfffe01 is "+//+AQ==" in base64; url-safe and unpadded: "-__-AQ".
        assert_eq!(
            String::from_utf8(data).unwrap(),
            r#"{"type":"webauthn.get","challenge":"-__-AQ","origin":"ssh:"}"#
        );
    }

    #[test]
    fn applications_that_need_escaping_are_refused() {
        for origin in ["a\"b", "a\\b", "a\nb"] {
            assert!(matches!(client_data(b"x", origin), Err(SkError::Device(_))));
        }
    }

    #[test]
    fn authenticator_data_is_split_and_checked() {
        let data = authenticator_data(0x05, 9, &[]);
        assert_eq!(
            parse_authenticator_data(&data),
            Some(AuthenticatorData {
                flags: 0x05,
                counter: 9,
                extensions: &[]
            })
        );
        let with_extensions = authenticator_data(0x85, 9, &[0xa0]);
        assert_eq!(
            parse_authenticator_data(&with_extensions)
                .unwrap()
                .extensions,
            [0xa0]
        );
        // Too short, extension flag without data, data without flag,
        // attested credential data.
        assert!(parse_authenticator_data(&data[..36]).is_none());
        assert!(parse_authenticator_data(&authenticator_data(0x85, 1, &[])).is_none());
        assert!(parse_authenticator_data(&authenticator_data(0x05, 1, &[0xa0])).is_none());
        assert!(parse_authenticator_data(&authenticator_data(0x45, 1, &[])).is_none());
    }

    /// A webauthn signature made the way Windows makes it, encoded for the
    /// wire, has the layout sshd parses, and its r, s, flags and counter
    /// verify with `ssh-key` over the client data.
    #[test]
    fn webauthn_signature_assembles_and_verifies() {
        let message = b"session id and userauth request".to_vec();
        let client = client_data(&message, APPLICATION).unwrap();
        let auth_data = authenticator_data(0x05, 42, &[]);
        let der = windows_sign(&auth_data, &client);
        let assertion =
            assertion_from_webauthn(APPLICATION, client.clone(), &auth_data, &der).unwrap();
        let wire = encode_signature(&info(), &assertion).unwrap();

        // string name, string ecdsa_signature, byte flags, uint32 counter,
        // string origin, string clientData, string extensions.
        let (name, rest) = take_string(&wire).unwrap();
        assert_eq!(name, WEBAUTHN_ECDSA_ALGORITHM.as_bytes());
        let (rs, rest) = take_string(rest).unwrap();
        let (r, rest_rs) = take_string(rs).unwrap();
        let (s, rest_rs) = take_string(rest_rs).unwrap();
        assert!(rest_rs.is_empty() && !r.is_empty() && !s.is_empty());
        assert_eq!(rest[0], 0x05);
        assert_eq!(rest[1..5], 42u32.to_be_bytes());
        let (origin, rest) = take_string(&rest[5..]).unwrap();
        assert_eq!(origin, APPLICATION.as_bytes());
        let (wire_client, rest) = take_string(rest).unwrap();
        assert_eq!(wire_client, client);
        let (extensions, rest) = take_string(rest).unwrap();
        assert!(extensions.is_empty() && rest.is_empty());
        // The client data carries the challenge the server derives.
        assert!(wire_client.starts_with(
            format!(
                r#"{{"type":"webauthn.get","challenge":"{}","origin":"ssh:""#,
                URL_SAFE_NO_PAD.encode(&message)
            )
            .as_bytes()
        ));

        // The same r, s, flags and counter as a plain sk-ecdsa signature over
        // the client data: ssh-key verifies `SHA256(app) || flags || counter
        // || SHA256(data)`, which is what the device signed with
        // data = clientData.
        let plain = encode_signature(
            &info(),
            &SkAssertion {
                webauthn: None,
                ..assertion.clone()
            },
        )
        .unwrap();
        let signature = Signature::try_from(plain.as_slice()).unwrap();
        let public = sk_public_key(APPLICATION);
        assert!(matches!(public.key_data(), KeyData::SkEcdsaSha2NistP256(_)));
        Verifier::verify(public.key_data(), &client, &signature).unwrap();
        // The challenge is bound: other client data does not verify.
        let other = client_data(b"another message", APPLICATION).unwrap();
        assert!(Verifier::verify(public.key_data(), &other, &signature).is_err());
    }

    #[test]
    fn extensions_travel_with_the_signature() {
        let client = client_data(b"m", APPLICATION).unwrap();
        let auth_data = authenticator_data(0x85, 1, &[0xa0]);
        let der = windows_sign(&auth_data, &client);
        let assertion = assertion_from_webauthn(APPLICATION, client, &auth_data, &der).unwrap();
        let wire = encode_signature(&info(), &assertion).unwrap();
        assert!(wire.ends_with(&[0, 0, 0, 1, 0xa0]));
    }

    #[test]
    fn unusable_windows_answers_are_refused() {
        let client = client_data(b"m", APPLICATION).unwrap();
        assert!(assertion_from_webauthn(APPLICATION, client.clone(), &[0; 10], &[1]).is_err());
        let auth_data = authenticator_data(0x01, 1, &[]);
        assert!(assertion_from_webauthn(APPLICATION, client, &auth_data, &[]).is_err());
    }

    fn userauth(algorithm: &str) -> Vec<u8> {
        let mut out = Vec::new();
        put_string(&mut out, &[9u8; 32]);
        out.push(50);
        put_string(&mut out, b"alice");
        put_string(&mut out, b"ssh-connection");
        put_string(&mut out, b"publickey");
        out.push(1);
        put_string(&mut out, algorithm.as_bytes());
        put_string(&mut out, b"key blob");
        out
    }

    #[test]
    fn the_userauth_algorithm_is_renamed() {
        let from = SkAlgorithm::EcdsaP256.name();
        let renamed =
            rewrite_userauth_algorithm(&userauth(from), from, WEBAUTHN_ECDSA_ALGORITHM).unwrap();
        assert_eq!(renamed, userauth(WEBAUTHN_ECDSA_ALGORITHM));
    }

    #[test]
    fn other_requests_are_not_rewritten() {
        let from = SkAlgorithm::EcdsaP256.name();
        let to = WEBAUTHN_ECDSA_ALGORITHM;
        // Another algorithm, garbage and truncated data.
        assert!(rewrite_userauth_algorithm(&userauth("ssh-ed25519"), from, to).is_none());
        assert!(rewrite_userauth_algorithm(b"", from, to).is_none());
        let good = userauth(from);
        assert!(rewrite_userauth_algorithm(&good[..good.len() - 20], from, to).is_none());
        // Not a userauth request.
        let mut wrong = good.clone();
        wrong[4 + 32] = 51;
        assert!(rewrite_userauth_algorithm(&wrong, from, to).is_none());
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
