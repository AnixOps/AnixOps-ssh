//! The Windows authenticator: FIDO2 security keys through the Windows WebAuthn
//! API (`webauthn.dll`, `WebAuthNAuthenticatorGetAssertion`), the route that
//! works for processes that are not elevated (only elevated processes may open
//! FIDO HID devices on Windows) and the one OpenSSH for Windows uses.
//!
//! Windows shows its own dialog for the touch and the PIN, so this backend
//! asks for neither ([`SecurityKey::handles_user_verification`]); the engine's
//! touch event is a notice only. The API signs a client data JSON, not
//! `SHA256(message)`, so the signature is the
//! `webauthn-sk-ecdsa-sha2-nistp256@openssh.com` form (see
//! `security_key_webauthn.rs`): ECDSA keys work, Ed25519 keys do not.
//!
//! `webauthn.dll` is loaded at run time from System32 instead of linked, so
//! the app still starts on a Windows without it; then [`available`] is false
//! and hosts with `sk-*` keys run on the system OpenSSH.
//!
//! [`available`]: WebAuthnSecurityKey::available
//!
//! A real prompt needs a person at a Windows desktop with a key, so CI only
//! builds this file; the encoding it feeds is tested in
//! `security_key_webauthn.rs`.

use crate::security_key::{
    SecurityKey, SkAlgorithm, SkAssertion, SkError, SkKeyInfo, SkSignRequest, SkSignatureKind,
    FLAG_USER_VERIFICATION,
};
use crate::security_key_webauthn::{assertion_from_webauthn, classify_hresult, client_data};
use async_trait::async_trait;
use std::sync::OnceLock;
use windows::core::{s, w, HRESULT, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Networking::WindowsWebServices::{
    WEBAUTHN_API_VERSION_1, WEBAUTHN_ASSERTION, WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
    WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_1, WEBAUTHN_CLIENT_DATA,
    WEBAUTHN_CLIENT_DATA_CURRENT_VERSION, WEBAUTHN_CREDENTIAL, WEBAUTHN_CREDENTIALS,
    WEBAUTHN_CREDENTIAL_CURRENT_VERSION, WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
    WEBAUTHN_HASH_ALGORITHM_SHA_256, WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetForegroundWindow};

/// The oldest API version the backend runs on (Windows 10 1903).
const MIN_API_VERSION: u32 = WEBAUTHN_API_VERSION_1;
/// How long Windows waits for the user, in milliseconds.
const TIMEOUT_MS: u32 = 120_000;

type GetApiVersion = unsafe extern "system" fn() -> u32;
type GetAssertion = unsafe extern "system" fn(
    HWND,
    PCWSTR,
    *const WEBAUTHN_CLIENT_DATA,
    *const WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
    *mut *mut WEBAUTHN_ASSERTION,
) -> HRESULT;
type FreeAssertion = unsafe extern "system" fn(*const WEBAUTHN_ASSERTION);

/// The functions of `webauthn.dll` the backend calls.
struct Api {
    version: u32,
    get_assertion: GetAssertion,
    free_assertion: FreeAssertion,
}

fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(load_api).as_ref()
}

fn load_api() -> Option<Api> {
    // SAFETY: the names and signatures below are those of webauthn.h. The
    // library is never unloaded, so the pointers stay valid.
    unsafe {
        let module = LoadLibraryExW(w!("webauthn.dll"), None, LOAD_LIBRARY_SEARCH_SYSTEM32).ok()?;
        let version = GetProcAddress(module, s!("WebAuthNGetApiVersionNumber"))?;
        let get_assertion = GetProcAddress(module, s!("WebAuthNAuthenticatorGetAssertion"))?;
        let free_assertion = GetProcAddress(module, s!("WebAuthNFreeAssertion"))?;
        let version =
            std::mem::transmute::<unsafe extern "system" fn() -> isize, GetApiVersion>(version)();
        Some(Api {
            version,
            get_assertion: std::mem::transmute::<unsafe extern "system" fn() -> isize, GetAssertion>(
                get_assertion,
            ),
            free_assertion: std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                FreeAssertion,
            >(free_assertion),
        })
    }
}

/// A security key through the Windows WebAuthn API.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebAuthnSecurityKey;

impl WebAuthnSecurityKey {
    /// `webauthn.dll` is there and its API version is recent enough.
    pub fn available() -> bool {
        api().is_some_and(|api| api.version >= MIN_API_VERSION)
    }
}

#[async_trait]
impl SecurityKey for WebAuthnSecurityKey {
    async fn sign(
        &self,
        request: &SkSignRequest<'_>,
        _pin: Option<&str>,
    ) -> Result<SkAssertion, SkError> {
        let info = request.info.clone();
        let message = request.message.to_vec();
        tokio::task::spawn_blocking(move || sign_blocking(&info, &message))
            .await
            .map_err(|e| SkError::Device(format!("the security key task failed: {e}")))?
    }

    fn signature_kind(&self, _algorithm: SkAlgorithm) -> SkSignatureKind {
        SkSignatureKind::Webauthn
    }

    fn handles_user_verification(&self) -> bool {
        true
    }
}

fn sign_blocking(info: &SkKeyInfo, message: &[u8]) -> Result<SkAssertion, SkError> {
    if info.algorithm != SkAlgorithm::EcdsaP256 {
        return Err(SkError::Device(
            "Windows WebAuthn can only sign for sk-ecdsa-sha2-nistp256 keys; \
             use the system OpenSSH for this key"
                .into(),
        ));
    }
    let api = api()
        .filter(|api| api.version >= MIN_API_VERSION)
        .ok_or(SkError::NoDevice)?;
    let client_json = client_data(message, &info.application)?;
    let (Ok(client_len), Ok(handle_len)) = (
        u32::try_from(client_json.len()),
        u32::try_from(info.key_handle.len()),
    ) else {
        return Err(SkError::Device(
            "the security key request is too large".into(),
        ));
    };
    let mut rp_id: Vec<u16> = info.application.encode_utf16().collect();
    rp_id.push(0);
    let mut handle = info.key_handle.clone();

    let mut credential = WEBAUTHN_CREDENTIAL {
        dwVersion: WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
        cbId: handle_len,
        pbId: handle.as_mut_ptr(),
        pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
    };
    let client = WEBAUTHN_CLIENT_DATA {
        dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
        cbClientDataJSON: client_len,
        pbClientDataJSON: client_json.as_ptr().cast_mut(),
        pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
    };
    // Version 1 of the options is a prefix of every later one, so it runs on
    // every API version: the credential to use, and what Windows must check.
    let mut options = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS {
        dwVersion: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_1,
        dwTimeoutMilliseconds: TIMEOUT_MS,
        dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
        dwUserVerificationRequirement: if info.flags & FLAG_USER_VERIFICATION != 0 {
            WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED
        } else {
            WEBAUTHN_USER_VERIFICATION_REQUIREMENT_DISCOURAGED
        },
        ..Default::default()
    };
    options.CredentialList = WEBAUTHN_CREDENTIALS {
        cCredentials: 1,
        pCredentials: &mut credential,
    };

    // SAFETY: every pointer refers to a local that outlives the call, the
    // structures have the versions they claim, and the returned assertion is
    // read only before it is freed.
    unsafe {
        let mut hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            hwnd = GetDesktopWindow();
        }
        let mut assertion: *mut WEBAUTHN_ASSERTION = std::ptr::null_mut();
        let hresult = (api.get_assertion)(
            hwnd,
            PCWSTR(rp_id.as_ptr()),
            &client,
            &options,
            &mut assertion,
        );
        if hresult.is_err() {
            return Err(classify_hresult(hresult.0));
        }
        if assertion.is_null() {
            return Err(SkError::Device(
                "Windows WebAuthn returned no assertion".into(),
            ));
        }
        let answer = &*assertion;
        let result = if answer.pbAuthenticatorData.is_null() || answer.pbSignature.is_null() {
            Err(SkError::Device(
                "Windows WebAuthn returned an incomplete assertion".into(),
            ))
        } else {
            assertion_from_webauthn(
                &info.application,
                client_json,
                std::slice::from_raw_parts(
                    answer.pbAuthenticatorData,
                    answer.cbAuthenticatorData as usize,
                ),
                std::slice::from_raw_parts(answer.pbSignature, answer.cbSignature as usize),
            )
        };
        (api.free_assertion)(assertion);
        result
    }
}
