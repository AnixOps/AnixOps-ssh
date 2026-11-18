//! FIDO2 `hmac-secret` over USB HID (Linux and macOS), through the pure-Rust
//! `ctap-hid-fido2` crate. Like the SSH security key backend it needs no
//! system library; on Linux the user needs access to the hidraw node, which
//! the usual udev rules for FIDO keys grant.
//!
//! Not used on Windows, where only elevated processes may open FIDO HID
//! devices; there `fido_windows.rs` goes through `webauthn.dll`.

use crate::fido_common::{random_bytes, RP_ID};
use ctap_hid_fido2::fidokey::get_assertion::get_assertion_params::{
    Extension as GetExt, GetAssertionArgsBuilder,
};
use ctap_hid_fido2::fidokey::get_info::InfoParam;
use ctap_hid_fido2::fidokey::make_credential::{Extension as MakeExt, MakeCredentialArgsBuilder};
use ctap_hid_fido2::{get_fidokey_devices, FidoKeyHid, FidoKeyHidFactory, LibCfg};
use easyssh_vault::{FidoError, HmacAuthenticator};
use zeroize::Zeroizing;

/// Security keys on USB.
#[derive(Debug, Clone, Copy, Default)]
pub struct HidAuthenticator;

fn config() -> LibCfg {
    let mut cfg = LibCfg::init();
    // The library prints "touch the sensor" to stdout; the app asks itself.
    cfg.enable_keep_alive_msg = false;
    cfg.keep_alive_msg = String::new();
    cfg
}

/// Maps the text of a CTAP2 status error to what the caller can act on. The
/// library reports statuses only as text (`0x36 CTAP2_ERR_PIN_REQUIRED ...`).
pub(crate) fn classify(error: &str) -> FidoError {
    let upper = error.to_ascii_uppercase();
    let has = |needle: &str| upper.contains(needle);
    if has("PIN_REQUIRED") || has("PUAT_REQUIRED") {
        FidoError::PinRequired
    } else if has("PIN_INVALID") || has("PIN_AUTH_INVALID") {
        FidoError::PinInvalid { retries: None }
    } else if has("PIN_BLOCKED") || has("PIN_AUTH_BLOCKED") {
        FidoError::PinBlocked
    } else if has("NO_CREDENTIALS") {
        FidoError::NoCredential
    } else if has("UNSUPPORTED_EXTENSION") || has("INVALID_OPTION") || has("UNSUPPORTED_OPTION") {
        FidoError::Unsupported
    } else if has("USER_ACTION_TIMEOUT")
        || has("OPERATION_DENIED")
        || has("KEEPALIVE_CANCEL")
        || has("ACTION_TIMEOUT")
    {
        FidoError::NotTouched
    } else {
        FidoError::Device(error.to_owned())
    }
}

fn open_all() -> Result<Vec<FidoKeyHid>, FidoError> {
    let devices = get_fidokey_devices();
    if devices.is_empty() {
        return Err(FidoError::NoDevice);
    }
    let cfg = config();
    let mut keys = Vec::new();
    let mut last = FidoError::NoDevice;
    for device in devices {
        match FidoKeyHidFactory::create_by_params(&[device.param], &cfg) {
            Ok(key) => keys.push(key),
            Err(e) => last = FidoError::Device(format!("cannot open the security key: {e}")),
        }
    }
    if keys.is_empty() {
        Err(last)
    } else {
        Ok(keys)
    }
}

fn make_blocking(pin: Option<&str>) -> Result<Vec<u8>, FidoError> {
    let challenge = random_bytes()?;
    let mut last = FidoError::NoDevice;
    for key in open_all()? {
        // Look before touching: a key without the extension would otherwise
        // make a credential that can never answer.
        match key.enable_info_param(&InfoParam::ExtensionsHmacSecret) {
            Ok(true) => {}
            Ok(false) => {
                last = FidoError::Unsupported;
                continue;
            }
            Err(e) => {
                last = classify(&e.to_string());
                continue;
            }
        }
        let mut builder = MakeCredentialArgsBuilder::new(RP_ID, &challenge)
            .extensions(&[MakeExt::HmacSecret(Some(true))]);
        builder = match pin {
            Some(pin) => builder.pin(pin),
            None => builder.without_pin_and_uv(),
        };
        match key.make_credential_with_args(&builder.build()) {
            Ok(attestation) => {
                let id = attestation.credential_descriptor.id;
                if id.is_empty() {
                    return Err(FidoError::Device(
                        "the security key returned no credential id".into(),
                    ));
                }
                return Ok(id);
            }
            Err(e) => return Err(classify(&e.to_string())),
        }
    }
    Err(last)
}

fn hmac_blocking(
    credential_id: &[u8],
    salt: &[u8; 32],
    pin: Option<&str>,
) -> Result<Zeroizing<[u8; 32]>, FidoError> {
    let challenge = random_bytes()?;
    let mut last = FidoError::NoCredential;
    for key in open_all()? {
        let mut builder = GetAssertionArgsBuilder::new(RP_ID, &challenge)
            .credential_id(credential_id)
            .extensions(&[GetExt::HmacSecret(Some(*salt))]);
        builder = match pin {
            Some(pin) => builder.pin(pin),
            None => builder.without_pin_and_uv(),
        };
        match key.get_assertion_with_args(&builder.build()) {
            Ok(assertions) => {
                for assertion in assertions {
                    for extension in assertion.extensions {
                        if let GetExt::HmacSecret(Some(output)) = extension {
                            return Ok(Zeroizing::new(output));
                        }
                    }
                }
                return Err(FidoError::Unsupported);
            }
            Err(e) => match classify(&e.to_string()) {
                // Another key may hold the credential.
                FidoError::NoCredential => last = FidoError::NoCredential,
                other => return Err(other),
            },
        }
    }
    Err(last)
}

impl HmacAuthenticator for HidAuthenticator {
    fn available(&self) -> bool {
        true
    }

    fn make_credential(&self, pin: Option<&str>) -> Result<Vec<u8>, FidoError> {
        make_blocking(pin)
    }

    fn hmac_secret(
        &self,
        credential_id: &[u8],
        salt: &[u8; 32],
        pin: Option<&str>,
    ) -> Result<Zeroizing<[u8; 32]>, FidoError> {
        hmac_blocking(credential_id, salt, pin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctap_status_text_maps_to_actionable_errors() {
        assert_eq!(
            classify("0x36 CTAP2_ERR_PIN_REQUIRED  PIN is required for the selected operation."),
            FidoError::PinRequired
        );
        assert_eq!(
            classify("0x31 CTAP2_ERR_PIN_INVALID   PIN Invalid."),
            FidoError::PinInvalid { retries: None }
        );
        assert_eq!(
            classify("0x32 CTAP2_ERR_PIN_BLOCKED"),
            FidoError::PinBlocked
        );
        assert_eq!(
            classify("0x2E CTAP2_ERR_NO_CREDENTIALS    No valid credentials provided."),
            FidoError::NoCredential
        );
        assert_eq!(
            classify("0x27 CTAP2_ERR_OPERATION_DENIED"),
            FidoError::NotTouched
        );
        assert_eq!(
            classify("0x2B CTAP2_ERR_UNSUPPORTED_EXTENSION"),
            FidoError::Unsupported
        );
        assert!(matches!(classify("something else"), FidoError::Device(_)));
    }
}
