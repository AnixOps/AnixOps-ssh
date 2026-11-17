//! OPAQUE (RFC 9807) building blocks shared by client and server.
//!
//! The "password" fed to OPAQUE is the 32-byte `AuthSecret` that
//! `easyssh-crypto` derives from the passphrase with Argon2id, so the
//! passphrase itself never reaches OPAQUE (or the network) and the memory-hard
//! stretching has already happened: the OPAQUE key-stretching function is the
//! identity. The server never learns the passphrase, the `AuthSecret` or the
//! account key; a database leak only allows an offline attack that has to pay
//! for Argon2id and for OPAQUE's OPRF per guess.

use crate::error::AuthError;
use easyssh_crypto::AuthSecret;
use opaque_ke::ksf::Identity;
use opaque_ke::{
    CipherSuite, ClientLogin, ClientLoginFinishParameters, ClientRegistration,
    ClientRegistrationFinishParameters, CredentialResponse, RegistrationResponse, Ristretto255,
    TripleDh,
};
use rand_core::OsRng;

/// Ristretto255 OPRF and 3DH key exchange with SHA-512.
pub struct OpaqueSuite;

impl CipherSuite for OpaqueSuite {
    type OprfCs = Ristretto255;
    type KeyExchange = TripleDh<Ristretto255, sha2::Sha512>;
    type Ksf = Identity;
}

pub struct RegistrationStart {
    state: ClientRegistration<OpaqueSuite>,
    /// Send to `register/start`.
    pub request: Vec<u8>,
}

pub fn registration_start(secret: &AuthSecret) -> Result<RegistrationStart, AuthError> {
    let result = ClientRegistration::<OpaqueSuite>::start(&mut OsRng, secret.expose())
        .map_err(|_| AuthError::Opaque)?;
    Ok(RegistrationStart {
        request: result.message.serialize().to_vec(),
        state: result.state,
    })
}

impl RegistrationStart {
    /// Returns the upload to send to `register/finish`.
    pub fn finish(self, secret: &AuthSecret, response: &[u8]) -> Result<Vec<u8>, AuthError> {
        let response =
            RegistrationResponse::deserialize(response).map_err(|_| AuthError::Opaque)?;
        let result = self
            .state
            .finish(
                &mut OsRng,
                secret.expose(),
                response,
                ClientRegistrationFinishParameters::default(),
            )
            .map_err(|_| AuthError::Opaque)?;
        Ok(result.message.serialize().to_vec())
    }
}

pub struct LoginStart {
    state: ClientLogin<OpaqueSuite>,
    /// Send to `login/start`.
    pub request: Vec<u8>,
}

pub fn login_start(secret: &AuthSecret) -> Result<LoginStart, AuthError> {
    let result = ClientLogin::<OpaqueSuite>::start(&mut OsRng, secret.expose())
        .map_err(|_| AuthError::Opaque)?;
    Ok(LoginStart {
        request: result.message.serialize().to_vec(),
        state: result.state,
    })
}

impl LoginStart {
    /// Returns the finalization to send to `login/finish`. Fails when the
    /// passphrase (hence `secret`) is wrong, before anything is revealed.
    pub fn finish(self, secret: &AuthSecret, response: &[u8]) -> Result<Vec<u8>, AuthError> {
        let response = CredentialResponse::deserialize(response).map_err(|_| AuthError::Opaque)?;
        let result = self
            .state
            .finish(
                &mut OsRng,
                secret.expose(),
                response,
                ClientLoginFinishParameters::default(),
            )
            .map_err(|_| AuthError::Opaque)?;
        Ok(result.message.serialize().to_vec())
    }
}
