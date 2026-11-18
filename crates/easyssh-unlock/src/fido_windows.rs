//! FIDO2 `hmac-secret` through the Windows WebAuthn API (`webauthn.dll`).
//! It is the route that works for processes that are not elevated (only
//! elevated processes may open FIDO HID devices on Windows).
//!
//! * Enrolment: `WebAuthNAuthenticatorMakeCredential` with the
//!   `hmac-secret` extension.
//! * Unlock: `WebAuthNAuthenticatorGetAssertion` with an
//!   `WEBAUTHN_HMAC_SECRET_SALT_VALUES`, which needs WebAuthn API version 4
//!   (Windows 11); the secret comes back in `pHmacSecret`.
//!
//! Windows shows its own dialog for the touch and the PIN, so the vault asks
//! for neither ([`HmacAuthenticator::handles_user_verification`]). User
//! verification is always required, so the hmac-secret output is the same one
//! at enrolment and unlock.
//!
//! `webauthn.dll` is loaded at run time from System32, so the app still
//! starts on a Windows without it; then [`WebAuthnAuthenticator::available`]
//! is false and the option is shown as unavailable. A real prompt needs a
//! person at a Windows desktop with a key, so CI only builds this file.

use crate::fido_common::{classify_hresult, random_bytes, RP_ID};
use easyssh_vault::{FidoError, HmacAuthenticator};
use std::sync::OnceLock;
use windows::core::{s, w, BOOL, HRESULT, PCWSTR};
use windows::Win32::Foundation::HWND;
use windows::Win32::Networking::WindowsWebServices::{
    WEBAUTHN_ASSERTION, WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE,
    WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY, WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_6,
    WEBAUTHN_AUTHENTICATOR_HMAC_SECRET_VALUES_FLAG, WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS,
    WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_1, WEBAUTHN_CLIENT_DATA,
    WEBAUTHN_CLIENT_DATA_CURRENT_VERSION, WEBAUTHN_COSE_ALGORITHM_ECDSA_P256_WITH_SHA256,
    WEBAUTHN_COSE_CREDENTIAL_PARAMETER, WEBAUTHN_COSE_CREDENTIAL_PARAMETERS, WEBAUTHN_CREDENTIAL,
    WEBAUTHN_CREDENTIALS, WEBAUTHN_CREDENTIAL_ATTESTATION, WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
    WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY, WEBAUTHN_EXTENSION, WEBAUTHN_EXTENSIONS,
    WEBAUTHN_EXTENSIONS_IDENTIFIER_HMAC_SECRET, WEBAUTHN_HASH_ALGORITHM_SHA_256,
    WEBAUTHN_HMAC_SECRET_SALT, WEBAUTHN_HMAC_SECRET_SALT_VALUES, WEBAUTHN_RP_ENTITY_INFORMATION,
    WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION, WEBAUTHN_USER_ENTITY_INFORMATION,
    WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
    WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
};
use windows::Win32::System::LibraryLoader::{
    GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use windows::Win32::UI::WindowsAndMessaging::{GetDesktopWindow, GetForegroundWindow};
use zeroize::Zeroizing;

/// The oldest API version with hmac-secret salt values in the assertion
/// options and `pHmacSecret` in the assertion.
const MIN_API_VERSION: u32 = 4;
/// How long Windows waits for the user, in milliseconds.
const TIMEOUT_MS: u32 = 120_000;
/// `WEBAUTHN_ASSERTION` version that carries `pHmacSecret`.
const ASSERTION_WITH_HMAC_SECRET: u32 = 3;

type GetApiVersion = unsafe extern "system" fn() -> u32;
type MakeCredential = unsafe extern "system" fn(
    HWND,
    *const WEBAUTHN_RP_ENTITY_INFORMATION,
    *const WEBAUTHN_USER_ENTITY_INFORMATION,
    *const WEBAUTHN_COSE_CREDENTIAL_PARAMETERS,
    *const WEBAUTHN_CLIENT_DATA,
    *const WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS,
    *mut *mut WEBAUTHN_CREDENTIAL_ATTESTATION,
) -> HRESULT;
type FreeAttestation = unsafe extern "system" fn(*const WEBAUTHN_CREDENTIAL_ATTESTATION);
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
    make_credential: MakeCredential,
    free_attestation: FreeAttestation,
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
        let make = GetProcAddress(module, s!("WebAuthNAuthenticatorMakeCredential"))?;
        let free_attestation = GetProcAddress(module, s!("WebAuthNFreeCredentialAttestation"))?;
        let get = GetProcAddress(module, s!("WebAuthNAuthenticatorGetAssertion"))?;
        let free_assertion = GetProcAddress(module, s!("WebAuthNFreeAssertion"))?;
        let version =
            std::mem::transmute::<unsafe extern "system" fn() -> isize, GetApiVersion>(version)();
        Some(Api {
            version,
            make_credential: std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                MakeCredential,
            >(make),
            free_attestation: std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                FreeAttestation,
            >(free_attestation),
            get_assertion: std::mem::transmute::<unsafe extern "system" fn() -> isize, GetAssertion>(
                get,
            ),
            free_assertion: std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                FreeAssertion,
            >(free_assertion),
        })
    }
}

fn usable_api() -> Result<&'static Api, FidoError> {
    api()
        .filter(|api| api.version >= MIN_API_VERSION)
        .ok_or(FidoError::Unsupported)
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn window() -> HWND {
    // SAFETY: both calls take no arguments and return a handle or null.
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            GetDesktopWindow()
        } else {
            hwnd
        }
    }
}

/// Security keys through the Windows WebAuthn API.
#[derive(Debug, Clone, Copy, Default)]
pub struct WebAuthnAuthenticator;

impl HmacAuthenticator for WebAuthnAuthenticator {
    fn available(&self) -> bool {
        api().is_some_and(|api| api.version >= MIN_API_VERSION)
    }

    fn handles_user_verification(&self) -> bool {
        true
    }

    fn make_credential(&self, _pin: Option<&str>) -> Result<Vec<u8>, FidoError> {
        let api = usable_api()?;
        let mut challenge = random_bytes()?;
        let mut user_id = random_bytes()?;
        let rp_id = wide(RP_ID);
        let rp_name = wide("AnixOps SSH vault");
        let user_name = wide("vault");
        let user_display = wide("AnixOps SSH vault");

        let rp = WEBAUTHN_RP_ENTITY_INFORMATION {
            dwVersion: WEBAUTHN_RP_ENTITY_INFORMATION_CURRENT_VERSION,
            pwszId: PCWSTR(rp_id.as_ptr()),
            pwszName: PCWSTR(rp_name.as_ptr()),
            pwszIcon: PCWSTR::null(),
        };
        let user = WEBAUTHN_USER_ENTITY_INFORMATION {
            dwVersion: WEBAUTHN_USER_ENTITY_INFORMATION_CURRENT_VERSION,
            cbId: user_id.len() as u32,
            pbId: user_id.as_mut_ptr(),
            pwszName: PCWSTR(user_name.as_ptr()),
            pwszIcon: PCWSTR::null(),
            pwszDisplayName: PCWSTR(user_display.as_ptr()),
        };
        let mut algorithm = WEBAUTHN_COSE_CREDENTIAL_PARAMETER {
            dwVersion: 1,
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
            lAlg: WEBAUTHN_COSE_ALGORITHM_ECDSA_P256_WITH_SHA256,
        };
        let parameters = WEBAUTHN_COSE_CREDENTIAL_PARAMETERS {
            cCredentialParameters: 1,
            pCredentialParameters: &mut algorithm,
        };
        let client = WEBAUTHN_CLIENT_DATA {
            dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cbClientDataJSON: challenge.len() as u32,
            pbClientDataJSON: challenge.as_mut_ptr(),
            pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
        };
        // The hmac-secret extension is a single BOOL: "create the secret".
        let mut enable = BOOL(1);
        let mut extension = WEBAUTHN_EXTENSION {
            pwszExtensionIdentifier: WEBAUTHN_EXTENSIONS_IDENTIFIER_HMAC_SECRET,
            cbExtension: std::mem::size_of::<BOOL>() as u32,
            pvExtension: (&mut enable as *mut BOOL).cast(),
        };
        // Version 1 of the options is a prefix of every later one, so it runs
        // on every API version.
        let options = WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS {
            dwVersion: WEBAUTHN_AUTHENTICATOR_MAKE_CREDENTIAL_OPTIONS_VERSION_1,
            dwTimeoutMilliseconds: TIMEOUT_MS,
            Extensions: WEBAUTHN_EXTENSIONS {
                cExtensions: 1,
                pExtensions: &mut extension,
            },
            dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
            dwUserVerificationRequirement: WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
            dwAttestationConveyancePreference: WEBAUTHN_ATTESTATION_CONVEYANCE_PREFERENCE_NONE,
            ..Default::default()
        };

        // SAFETY: every pointer refers to a local that outlives the call, the
        // structures have the versions they claim, and the returned
        // attestation is read only before it is freed.
        unsafe {
            let mut attestation: *mut WEBAUTHN_CREDENTIAL_ATTESTATION = std::ptr::null_mut();
            let hresult = (api.make_credential)(
                window(),
                &rp,
                &user,
                &parameters,
                &client,
                &options,
                &mut attestation,
            );
            if hresult.is_err() {
                return Err(classify_hresult(hresult.0));
            }
            if attestation.is_null() {
                return Err(FidoError::Device(
                    "Windows WebAuthn returned no credential".into(),
                ));
            }
            let answer = &*attestation;
            let id = if answer.pbCredentialId.is_null() || answer.cbCredentialId == 0 {
                Err(FidoError::Device(
                    "Windows WebAuthn returned no credential id".into(),
                ))
            } else {
                Ok(std::slice::from_raw_parts(
                    answer.pbCredentialId,
                    answer.cbCredentialId as usize,
                )
                .to_vec())
            };
            (api.free_attestation)(attestation);
            id
        }
    }

    fn hmac_secret(
        &self,
        credential_id: &[u8],
        salt: &[u8; 32],
        _pin: Option<&str>,
    ) -> Result<Zeroizing<[u8; 32]>, FidoError> {
        let api = usable_api()?;
        let mut challenge = random_bytes()?;
        let rp_id = wide(RP_ID);
        let mut handle = credential_id.to_vec();
        let mut salt = Zeroizing::new(*salt);

        let mut credential = WEBAUTHN_CREDENTIAL {
            dwVersion: WEBAUTHN_CREDENTIAL_CURRENT_VERSION,
            cbId: handle.len() as u32,
            pbId: handle.as_mut_ptr(),
            pwszCredentialType: WEBAUTHN_CREDENTIAL_TYPE_PUBLIC_KEY,
        };
        let client = WEBAUTHN_CLIENT_DATA {
            dwVersion: WEBAUTHN_CLIENT_DATA_CURRENT_VERSION,
            cbClientDataJSON: challenge.len() as u32,
            pbClientDataJSON: challenge.as_mut_ptr(),
            pwszHashAlgId: WEBAUTHN_HASH_ALGORITHM_SHA_256,
        };
        let mut global_salt = WEBAUTHN_HMAC_SECRET_SALT {
            cbFirst: 32,
            pbFirst: salt.as_mut_ptr(),
            cbSecond: 0,
            pbSecond: std::ptr::null_mut(),
        };
        let mut salts = WEBAUTHN_HMAC_SECRET_SALT_VALUES {
            pGlobalHmacSalt: &mut global_salt,
            cCredWithHmacSecretSaltList: 0,
            pCredWithHmacSecretSaltList: std::ptr::null_mut(),
        };
        let mut options = WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS {
            dwVersion: WEBAUTHN_AUTHENTICATOR_GET_ASSERTION_OPTIONS_VERSION_6,
            dwTimeoutMilliseconds: TIMEOUT_MS,
            dwAuthenticatorAttachment: WEBAUTHN_AUTHENTICATOR_ATTACHMENT_ANY,
            dwUserVerificationRequirement: WEBAUTHN_USER_VERIFICATION_REQUIREMENT_REQUIRED,
            dwFlags: WEBAUTHN_AUTHENTICATOR_HMAC_SECRET_VALUES_FLAG,
            pHmacSecretSaltValues: &mut salts,
            ..Default::default()
        };
        options.CredentialList = WEBAUTHN_CREDENTIALS {
            cCredentials: 1,
            pCredentials: &mut credential,
        };

        // SAFETY: every pointer refers to a local that outlives the call, the
        // structures have the versions they claim, and the returned assertion
        // is read only before it is freed.
        unsafe {
            let mut assertion: *mut WEBAUTHN_ASSERTION = std::ptr::null_mut();
            let hresult = (api.get_assertion)(
                window(),
                PCWSTR(rp_id.as_ptr()),
                &client,
                &options,
                &mut assertion,
            );
            if hresult.is_err() {
                return Err(classify_hresult(hresult.0));
            }
            if assertion.is_null() {
                return Err(FidoError::Device(
                    "Windows WebAuthn returned no assertion".into(),
                ));
            }
            let answer = &*assertion;
            let secret = if answer.dwVersion < ASSERTION_WITH_HMAC_SECRET
                || answer.pHmacSecret.is_null()
                || (*answer.pHmacSecret).pbFirst.is_null()
                || (*answer.pHmacSecret).cbFirst != 32
            {
                Err(FidoError::Unsupported)
            } else {
                let mut out = Zeroizing::new([0u8; 32]);
                out.copy_from_slice(std::slice::from_raw_parts(
                    (*answer.pHmacSecret).pbFirst,
                    32,
                ));
                Ok(out)
            };
            (api.free_assertion)(assertion);
            secret
        }
    }
}
