//! What the FIDO2 backends share.

use easyssh_crypto::DeviceKey;
use easyssh_vault::FidoError;

/// The relying party id of the vault's credentials. It is not a web origin;
/// authenticators and the Windows API only need a stable string.
pub(crate) const RP_ID: &str = "vault.anixops-ssh";

/// 32 random bytes for a challenge or user id.
pub(crate) fn random_bytes() -> Result<[u8; 32], FidoError> {
    DeviceKey::generate()
        .map(|key| *key.expose())
        .map_err(|e| FidoError::Device(format!("no random numbers: {e}")))
}

/// Maps the `HRESULT` of a `webauthn.dll` call to what the vault can act on.
/// Windows shows its own dialog for the touch and the PIN and handles
/// retries, so only a few outcomes matter. Kept here, not in the Windows
/// file, so the table is tested on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn classify_hresult(hresult: i32) -> FidoError {
    const NTE_NOT_FOUND: u32 = 0x8009_0011;
    const NTE_USER_CANCELLED: u32 = 0x8009_0036;
    const NTE_NOT_SUPPORTED: u32 = 0x8009_0029;
    // HRESULT_FROM_WIN32 of ERROR_CANCELLED and ERROR_TIMEOUT.
    const CANCELLED: u32 = 0x8007_04C7;
    const TIMEOUT: u32 = 0x8007_05B4;
    match hresult as u32 {
        NTE_NOT_FOUND => FidoError::NoCredential,
        NTE_USER_CANCELLED | CANCELLED | TIMEOUT => FidoError::NotTouched,
        NTE_NOT_SUPPORTED => FidoError::Unsupported,
        other => FidoError::Device(format!("Windows WebAuthn error 0x{other:08X}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_webauthn_errors_map_to_what_the_user_can_do() {
        assert_eq!(
            classify_hresult(0x8009_0011_u32 as i32),
            FidoError::NoCredential
        );
        assert_eq!(
            classify_hresult(0x8009_0036_u32 as i32),
            FidoError::NotTouched
        );
        assert_eq!(
            classify_hresult(0x8007_04C7_u32 as i32),
            FidoError::NotTouched
        );
        assert_eq!(
            classify_hresult(0x8009_0029_u32 as i32),
            FidoError::Unsupported
        );
        assert!(matches!(
            classify_hresult(0x8000_4005_u32 as i32),
            FidoError::Device(text) if text.contains("0x80004005")
        ));
    }

    #[test]
    fn challenges_are_random() {
        assert_ne!(random_bytes().unwrap(), random_bytes().unwrap());
    }
}
