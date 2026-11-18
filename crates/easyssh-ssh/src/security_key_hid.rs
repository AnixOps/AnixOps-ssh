//! The real authenticator: FIDO2 security keys over USB HID (CTAP2), through
//! the pure-Rust `ctap-hid-fido2` crate. It needs no system libfido2; on
//! Linux it uses hidraw (so the user needs access to the device node, which
//! the usual udev rules for FIDO keys grant), on macOS IOKit.
//!
//! Not built on Windows: Windows only lets elevated processes open FIDO HID
//! devices; there `security_key_winhello.rs` goes through `webauthn.dll`.

use crate::security_key::{
    SecurityKey, SkAssertion, SkError, SkKeyInfo, SkSignRequest, FLAG_USER_PRESENCE,
};
use async_trait::async_trait;
use ctap_hid_fido2::fidokey::get_assertion::get_assertion_params::GetAssertionArgsBuilder;
use ctap_hid_fido2::{get_fidokey_devices, FidoKeyHidFactory, LibCfg};
use zeroize::Zeroizing;

/// A security key on USB.
#[derive(Debug, Clone, Copy, Default)]
pub struct HidSecurityKey;

struct Job {
    info: SkKeyInfo,
    message: Vec<u8>,
    pin: Option<Zeroizing<String>>,
}

#[async_trait]
impl SecurityKey for HidSecurityKey {
    async fn sign(
        &self,
        request: &SkSignRequest<'_>,
        pin: Option<&str>,
    ) -> Result<SkAssertion, SkError> {
        let job = Job {
            info: request.info.clone(),
            message: request.message.to_vec(),
            pin: pin.map(|p| Zeroizing::new(p.to_owned())),
        };
        tokio::task::spawn_blocking(move || sign_blocking(&job))
            .await
            .map_err(|e| SkError::Device(format!("the security key task failed: {e}")))?
    }
}

/// Maps the text of a CTAP2 status error to what the caller can act on. The
/// library reports statuses only as text (`0x36 CTAP2_ERR_PIN_REQUIRED ...`).
pub(crate) fn classify(error: &str) -> SkError {
    let upper = error.to_ascii_uppercase();
    let has = |needle: &str| upper.contains(needle);
    if has("PIN_REQUIRED") || has("PUAT_REQUIRED") {
        SkError::PinRequired
    } else if has("PIN_INVALID") || has("PIN_AUTH_INVALID") {
        SkError::PinInvalid { retries: None }
    } else if has("PIN_BLOCKED") || has("PIN_AUTH_BLOCKED") {
        SkError::PinBlocked
    } else if has("NO_CREDENTIALS") {
        SkError::NoCredential
    } else if has("USER_ACTION_TIMEOUT")
        || has("OPERATION_DENIED")
        || has("KEEPALIVE_CANCEL")
        || has("ACTION_TIMEOUT")
    {
        SkError::NotTouched
    } else {
        SkError::Device(error.to_owned())
    }
}

/// The flags byte and the counter of raw CTAP2 authenticator data
/// (`rpIdHash(32) || flags(1) || counter(4) || ...`).
pub(crate) fn flags_and_counter(auth_data: &[u8]) -> Option<(u8, u32)> {
    let flags = *auth_data.get(32)?;
    let counter = auth_data.get(33..37)?;
    Some((flags, u32::from_be_bytes(counter.try_into().ok()?)))
}

fn sign_blocking(job: &Job) -> Result<SkAssertion, SkError> {
    let mut cfg = LibCfg::init();
    // The library prints "touch the sensor" to stdout; the app asks the user
    // itself.
    cfg.enable_keep_alive_msg = false;
    cfg.keep_alive_msg = String::new();

    let devices = get_fidokey_devices();
    if devices.is_empty() {
        return Err(SkError::NoDevice);
    }
    let mut last = SkError::NoCredential;
    for device in devices {
        let key = match FidoKeyHidFactory::create_by_params(&[device.param], &cfg) {
            Ok(key) => key,
            Err(e) => {
                last = SkError::Device(format!("cannot open the security key: {e}"));
                continue;
            }
        };
        let mut builder = GetAssertionArgsBuilder::new(&job.info.application, &job.message)
            .credential_id(&job.info.key_handle);
        builder = match &job.pin {
            Some(pin) => builder.pin(pin.as_str()),
            None => builder.without_pin_and_uv(),
        };
        if job.info.flags & FLAG_USER_PRESENCE == 0 {
            builder = builder.without_up();
        }
        match key.get_assertion_with_args(&builder.build()) {
            Ok(assertions) => {
                let assertion = assertions
                    .into_iter()
                    .next()
                    .ok_or_else(|| SkError::Device("the security key returned nothing".into()))?;
                let (flags, counter) =
                    flags_and_counter(&assertion.auth_data).ok_or_else(|| {
                        SkError::Device("the security key returned short authenticator data".into())
                    })?;
                return Ok(SkAssertion {
                    signature: assertion.signature,
                    flags,
                    counter,
                    webauthn: None,
                });
            }
            Err(e) => match classify(&e.to_string()) {
                // Another key may hold the credential.
                SkError::NoCredential => last = SkError::NoCredential,
                other => return Err(other),
            },
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctap_status_text_maps_to_actionable_errors() {
        assert_eq!(
            classify("0x36 CTAP2_ERR_PIN_REQUIRED  PIN is required for the selected operation."),
            SkError::PinRequired
        );
        assert_eq!(
            classify("0x31 CTAP2_ERR_PIN_INVALID   PIN Invalid."),
            SkError::PinInvalid { retries: None }
        );
        assert_eq!(classify("0x32 CTAP2_ERR_PIN_BLOCKED"), SkError::PinBlocked);
        assert_eq!(
            classify("0x2E CTAP2_ERR_NO_CREDENTIALS    No valid credentials provided."),
            SkError::NoCredential
        );
        assert_eq!(
            classify("0x27 CTAP2_ERR_OPERATION_DENIED"),
            SkError::NotTouched
        );
        assert!(matches!(classify("something else"), SkError::Device(_)));
    }

    #[test]
    fn authenticator_data_gives_flags_and_counter() {
        let mut data = vec![0u8; 32];
        data.push(0x05);
        data.extend_from_slice(&7u32.to_be_bytes());
        assert_eq!(flags_and_counter(&data), Some((0x05, 7)));
        assert_eq!(flags_and_counter(&data[..36]), None);
    }
}
