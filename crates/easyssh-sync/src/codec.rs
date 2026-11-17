//! Turning a [`RecordVersion`] into a signed wire [`Envelope`] and back.

use crate::error::SyncError;
use crate::record::{RecordVersion, PAYLOAD_VERSION};
use crate::wire::Envelope;
use easyssh_crypto::{
    decrypt_record, encrypt_record, sign_envelope, Aad, DeviceCert, DeviceSigningKey,
    EnvelopeFields, VaultKey,
};

/// A device's signing identity: id, key and the account's certificate for it.
pub struct DeviceIdentity {
    /// Used in version vectors and field stamps; by convention
    /// [`crate::wire::device_id_for`] of the public key.
    pub device_id: String,
    pub signing_key: DeviceSigningKey,
    pub cert: DeviceCert,
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceIdentity")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// Encrypt and sign `version` as an envelope for `record_id` in `vault_id`.
pub fn seal_record(
    identity: &DeviceIdentity,
    vault_key: &VaultKey,
    vault_id: &str,
    record_id: &str,
    version: &RecordVersion,
) -> Result<Envelope, SyncError> {
    let aad = Aad {
        record_id,
        vault_id,
        schema_version: PAYLOAD_VERSION,
        version_vector_digest: version.version_vector.digest(),
    };
    let ciphertext = encrypt_record(vault_key, &version.to_plaintext(), &aad)?;
    let signed = sign_envelope(
        EnvelopeFields {
            record_id: record_id.to_owned(),
            vault_id: vault_id.to_owned(),
            version_vector: version.version_vector.clone(),
            deleted: version.is_deleted(),
            ciphertext,
        },
        &identity.signing_key,
    );
    Ok(Envelope::from_signed(
        signed,
        &version.table,
        &identity.cert,
    ))
}

/// Decrypt an envelope that has already been verified. Checks that the
/// table and the delete flag in the routing metadata match the ciphertext.
pub fn open_record(vault_key: &VaultKey, envelope: &Envelope) -> Result<RecordVersion, SyncError> {
    let vector = envelope.vector();
    let aad = Aad {
        record_id: &envelope.record_id,
        vault_id: &envelope.vault_id,
        schema_version: PAYLOAD_VERSION,
        version_vector_digest: vector.digest(),
    };
    let plaintext = decrypt_record(vault_key, &envelope.ciphertext, &aad)?;
    let version =
        RecordVersion::from_plaintext(&plaintext, vector).map_err(SyncError::BadRecord)?;
    if version.table != envelope.table {
        return Err(SyncError::BadRecord("table does not match the ciphertext"));
    }
    if version.is_deleted() != envelope.deleted {
        return Err(SyncError::BadRecord(
            "deleted flag does not match the ciphertext",
        ));
    }
    Ok(version)
}
