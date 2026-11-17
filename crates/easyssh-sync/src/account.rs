//! Account, device and vault flows on top of [`HttpClient`].

use crate::auth::{login_start, registration_start};
use crate::codec::DeviceIdentity;
use crate::error::{AuthError, TransportError};
use crate::http::HttpClient;
use crate::verify::Verifier;
use crate::wire::*;
use easyssh_crypto::{
    certify_device, derive_account_secrets, open_vault_key, seal_vault_key, unwrap_account_keys,
    wrap_account_keys, AccountKeyPair, DeviceCert, DeviceSigningKey, KdfHeader, KdfParams,
    SealPublicKey, VaultKey, CERT_LIFETIME_SECS,
};
use ed25519_dalek::VerifyingKey;

/// A signed-in account with its (unwrapped) key pairs.
#[derive(Debug)]
pub struct Account {
    pub account_id: String,
    pub username: String,
    pub keys: AccountKeyPair,
}

/// Create an account. The passphrase is stretched locally; the server gets
/// an OPAQUE registration record, the public KDF header, the wrapped key
/// pairs (opaque to it) and the two public keys.
pub async fn register(
    client: &HttpClient,
    username: &str,
    passphrase: &[u8],
    params: KdfParams,
) -> Result<Account, AuthError> {
    let header = KdfHeader::generate(params)?;
    let secrets = derive_account_secrets(passphrase, &header)?;
    let keys = AccountKeyPair::generate()?;
    let wrapped = wrap_account_keys(&secrets.account_key, &keys)?;

    let start = registration_start(&secrets.auth_secret)?;
    let response = client
        .register_start(&RegisterStartRequest {
            username: username.to_owned(),
            request: start.request.clone(),
        })
        .await?;
    let upload = start.finish(&secrets.auth_secret, &response.response)?;
    let finished = client
        .register_finish(&RegisterFinishRequest {
            username: username.to_owned(),
            upload,
            kdf_header: header.to_bytes().to_vec(),
            wrapped_account_keys: wrapped,
            seal_public: keys.sealing.public().0.to_vec(),
            signing_public: keys.signing.public().to_bytes().to_vec(),
        })
        .await?;
    Ok(Account {
        account_id: finished.account_id,
        username: username.to_owned(),
        keys,
    })
}

/// Sign in (OPAQUE), install the session token on `client` and unwrap the
/// account key pairs. A wrong passphrase fails on this side of the wire.
pub async fn login(
    client: &HttpClient,
    username: &str,
    passphrase: &[u8],
) -> Result<Account, AuthError> {
    let header = KdfHeader::from_bytes(&client.kdf_header(username).await?.kdf_header)?;
    let secrets = derive_account_secrets(passphrase, &header)?;
    let start = login_start(&secrets.auth_secret)?;
    let response = client
        .login_start(&LoginStartRequest {
            username: username.to_owned(),
            request: start.request.clone(),
        })
        .await?;
    let finalization = start.finish(&secrets.auth_secret, &response.response)?;
    let done = client
        .login_finish(&LoginFinishRequest {
            login_id: response.login_id,
            finalization,
        })
        .await?;
    let keys = unwrap_account_keys(&secrets.account_key, &done.wrapped_account_keys)?;
    if keys.signing.public().to_bytes().as_slice() != done.signing_public.as_slice()
        || keys.sealing.public().0.as_slice() != done.seal_public.as_slice()
    {
        return Err(AuthError::BadAccountData(
            "public keys do not match the wrapped key pairs",
        ));
    }
    Ok(Account {
        account_id: done.account_id,
        username: username.to_owned(),
        keys,
    })
}

/// Certify `key` with the account key (valid for [`CERT_LIFETIME_SECS`] from
/// `issued_at`) and register it with the server.
pub async fn enroll_device(
    client: &HttpClient,
    account: &Account,
    device_name: &str,
    key: DeviceSigningKey,
    issued_at: i64,
) -> Result<DeviceIdentity, TransportError> {
    let public = key.public();
    let cert = renew_device_cert(client, account, device_name, &public, issued_at).await?;
    Ok(DeviceIdentity {
        device_id: device_id_for(&public),
        signing_key: key,
        cert,
    })
}

/// Issue a new certificate for an already known device key and register it:
/// the server replaces the stored one. This is how a device stays valid
/// past the expiry of its certificate; it needs the account key and a
/// session, so it happens on sign-in and while a signed-in device syncs.
pub async fn renew_device_cert(
    client: &HttpClient,
    account: &Account,
    device_name: &str,
    device_public: &[u8; 32],
    now: i64,
) -> Result<DeviceCert, TransportError> {
    let cert = certify_device(
        &account.keys.signing,
        device_public,
        device_name,
        now,
        now + CERT_LIFETIME_SECS,
    );
    client
        .register_device(&RegisterDeviceRequest {
            device_public: device_public.to_vec(),
            name: device_name.to_owned(),
            cert: cert.to_bytes(),
        })
        .await?;
    Ok(cert)
}

/// Create a vault with a fresh key sealed to the creator; returns the key.
pub async fn create_vault(
    client: &HttpClient,
    account: &Account,
    vault_id: &str,
) -> Result<VaultKey, AuthError> {
    let key = VaultKey::generate()?;
    let sealed = seal_vault_key(&key, &account.keys.sealing.public())?;
    client
        .create_vault(&CreateVaultRequest {
            vault_id: vault_id.to_owned(),
            sealed_key: sealed,
        })
        .await?;
    Ok(key)
}

/// Fetch and open this account's copy of a vault's current key; also
/// returns its generation.
pub async fn open_vault(
    client: &HttpClient,
    account: &Account,
    vault_id: &str,
) -> Result<(VaultKey, u32), AuthError> {
    let vaults = client.list_vaults().await?;
    let vault = vaults
        .iter()
        .find(|v| v.vault_id == vault_id)
        .ok_or(AuthError::BadAccountData("vault is not shared with you"))?;
    Ok((
        open_vault_key(&vault.sealed_key, &account.keys.sealing)?,
        vault.key_generation,
    ))
}

/// Seal `key` to every member in `members` (for a key rotation).
pub fn seal_for_members(
    members: &[MemberInfo],
    key: &VaultKey,
) -> Result<Vec<MemberKey>, AuthError> {
    members
        .iter()
        .map(|member| {
            let public: [u8; 32] = member
                .seal_public
                .as_slice()
                .try_into()
                .map_err(|_| AuthError::BadAccountData("bad public key length"))?;
            Ok(MemberKey {
                account_id: member.account_id.clone(),
                sealed_key: seal_vault_key(key, &SealPublicKey(public))?,
            })
        })
        .collect()
}

/// Owner only: seal `vault_key` (generation `key_generation`) to the account
/// `username` and add it with `role`.
///
/// The recipient's public key comes from the server, so a malicious server
/// could substitute its own; compare account fingerprints out of band for
/// vaults that matter (not automated yet).
pub async fn share_vault(
    client: &HttpClient,
    vault_id: &str,
    vault_key: &VaultKey,
    key_generation: u32,
    username: &str,
    role: Role,
) -> Result<AccountInfo, AuthError> {
    let info = client.lookup_account(username).await?;
    let public: [u8; 32] = info
        .seal_public
        .as_slice()
        .try_into()
        .map_err(|_| AuthError::BadAccountData("bad public key length"))?;
    let sealed = seal_vault_key(vault_key, &SealPublicKey(public))?;
    client
        .put_member(
            vault_id,
            &info.account_id,
            &PutMemberRequest {
                role,
                sealed_key: Some(sealed),
                key_generation: Some(key_generation),
            },
        )
        .await?;
    Ok(info)
}

/// Trust the signing keys of every member of `vault_id`, so envelopes signed
/// by their devices verify. Like [`share_vault`], this relies on the server
/// reporting the right public keys.
pub async fn trust_members(
    client: &HttpClient,
    verifier: &mut Verifier,
    vault_id: &str,
) -> Result<(), AuthError> {
    for member in client.members(vault_id).await? {
        let bytes: [u8; 32] = member
            .signing_public
            .as_slice()
            .try_into()
            .map_err(|_| AuthError::BadAccountData("bad public key length"))?;
        let key = VerifyingKey::from_bytes(&bytes)
            .map_err(|_| AuthError::BadAccountData("bad signing key"))?;
        verifier.trust_account(key);
    }
    Ok(())
}

/// Tell `verifier` which devices of the vault's members were revoked, and
/// when, so it refuses what they sent after that.
pub async fn trust_revocations(
    client: &HttpClient,
    verifier: &mut Verifier,
    vault_id: &str,
) -> Result<(), AuthError> {
    for revoked in client.revoked_devices(vault_id).await? {
        if let Some(public) = device_public_from_id(&revoked.device_id) {
            verifier.revoke_device(public, revoked.revoked_at);
        }
    }
    Ok(())
}
