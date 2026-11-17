//! Envelope verification and rollback protection.

use crate::error::VerifyError;
use crate::wire::Envelope;
use easyssh_crypto::{verify_envelope, CryptoError, DeviceCert, Ordering, VersionVector};
use ed25519_dalek::VerifyingKey;
use std::collections::{BTreeMap, HashMap};

/// Newest version vector seen per `(vault, record)`: the join of every
/// vector this client has accepted or pushed. Persist it with
/// [`HighWater::snapshot`] so rollback protection survives restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HighWater {
    seen: HashMap<(String, String), VersionVector>,
}

/// Serializable form of [`HighWater`].
pub type HighWaterSnapshot = Vec<(String, String, BTreeMap<String, u64>)>;

impl HighWater {
    pub fn get(&self, vault_id: &str, record_id: &str) -> Option<&VersionVector> {
        self.seen.get(&(vault_id.to_owned(), record_id.to_owned()))
    }

    /// Fold `vector` into the mark (element-wise maximum).
    pub fn record(&mut self, vault_id: &str, record_id: &str, vector: &VersionVector) {
        self.seen
            .entry((vault_id.to_owned(), record_id.to_owned()))
            .or_default()
            .merge(vector);
    }

    pub fn snapshot(&self) -> HighWaterSnapshot {
        let mut out: Vec<_> = self
            .seen
            .iter()
            .map(|((v, r), vec)| {
                (
                    v.clone(),
                    r.clone(),
                    vec.iter().map(|(d, c)| (d.to_owned(), c)).collect(),
                )
            })
            .collect();
        out.sort();
        out
    }

    pub fn from_snapshot(snapshot: HighWaterSnapshot) -> Self {
        let mut seen = HashMap::new();
        for (vault, record, entries) in snapshot {
            seen.insert(
                (vault, record),
                VersionVector::from_entries(entries.iter().map(|(d, c)| (d.as_str(), *c))),
            );
        }
        Self { seen }
    }
}

/// Checks every envelope this client accepts.
///
/// An envelope is accepted only if (1) its certificate is signed by one of
/// the trusted account keys (the client's own, plus the accounts of the
/// vault's other members, learned from the member list), (2) the certificate
/// is for the signing device, (3) the signature covers every routing field
/// and the ciphertext, and (4) its version vector is not older than one
/// already seen for that record.
#[derive(Debug, Clone)]
pub struct Verifier {
    accounts: Vec<VerifyingKey>,
    high_water: HighWater,
}

impl Verifier {
    pub fn new(own_account: VerifyingKey) -> Self {
        Self {
            accounts: vec![own_account],
            high_water: HighWater::default(),
        }
    }

    pub fn with_high_water(mut self, high_water: HighWater) -> Self {
        self.high_water = high_water;
        self
    }

    /// Trust another account's signing key (a member of a shared vault).
    pub fn trust_account(&mut self, account: VerifyingKey) {
        if !self.accounts.contains(&account) {
            self.accounts.push(account);
        }
    }

    pub fn high_water(&self) -> &HighWater {
        &self.high_water
    }

    /// Note a version this client created and pushed itself.
    pub fn record_local(&mut self, vault_id: &str, record_id: &str, vector: &VersionVector) {
        self.high_water.record(vault_id, record_id, vector);
    }

    /// Signature, certificate and signer checks only.
    pub fn verify_signature(&self, envelope: &Envelope) -> Result<(), VerifyError> {
        if envelope.cert.is_empty() {
            return Err(VerifyError::Malformed("missing certificate"));
        }
        let (signed, cert) = envelope
            .to_signed()
            .map_err(|e| VerifyError::Malformed(e.0))?;
        self.check_signed(&signed, &cert)
    }

    fn check_signed(
        &self,
        signed: &easyssh_crypto::Envelope,
        cert: &DeviceCert,
    ) -> Result<(), VerifyError> {
        let mut last = VerifyError::UntrustedCertificate;
        for account in &self.accounts {
            match verify_envelope(signed, cert, account) {
                Ok(()) => return Ok(()),
                Err(CryptoError::BadCertificate) => {}
                Err(CryptoError::SignerMismatch) => last = VerifyError::SignerMismatch,
                Err(_) => last = VerifyError::BadSignature,
            }
            // A certificate that verifies under this account but fails later
            // is not retried against other accounts' keys.
            if cert.verify(account).is_ok() {
                return Err(last);
            }
        }
        Err(VerifyError::UntrustedCertificate)
    }

    /// Full check for an envelope arriving from the server, then record its
    /// vector in the high-water mark. `Equal` to what was seen is fine
    /// (re-delivery); strictly older is a rollback.
    pub fn verify_and_record(&mut self, envelope: &Envelope) -> Result<(), VerifyError> {
        self.verify_signature(envelope)?;
        let vector = envelope.vector();
        if let Some(seen) = self.high_water.get(&envelope.vault_id, &envelope.record_id) {
            if vector.compare(seen) == Ordering::Before {
                return Err(VerifyError::Rollback);
            }
        }
        self.high_water
            .record(&envelope.vault_id, &envelope.record_id, &vector);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{seal_record, DeviceIdentity};
    use crate::record::RecordVersion;
    use crate::wire::device_id_for;
    use easyssh_crypto::{certify_device, AccountKeyPair, DeviceSigningKey, VaultKey};
    use serde_json::json;

    pub(crate) fn identity(account: &AccountKeyPair, seed: u8) -> DeviceIdentity {
        let key = DeviceSigningKey::from_seed([seed; 32]);
        let cert = certify_device(&account.signing, &key.public(), "dev", 1_700_000_000);
        DeviceIdentity {
            device_id: device_id_for(&key.public()),
            signing_key: key,
            cert,
        }
    }

    fn envelope(identity: &DeviceIdentity, edits: usize) -> Envelope {
        let key = VaultKey::from_bytes([9; 32]);
        let mut rec = RecordVersion::new("hosts");
        for i in 0..edits {
            rec.edit(&identity.device_id, [("n", json!(i))]);
        }
        seal_record(identity, &key, "v", "r", &rec).unwrap()
    }

    #[test]
    fn accepts_valid_envelope() {
        let account = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let id = identity(&account, 3);
        let mut v = Verifier::new(account.signing.public());
        v.verify_and_record(&envelope(&id, 1)).unwrap();
    }

    #[test]
    fn rejects_tampering_in_every_routed_field() {
        let account = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let id = identity(&account, 3);
        let v = Verifier::new(account.signing.public());
        let good = envelope(&id, 1);
        assert!(v.verify_signature(&good).is_ok());

        let mut e = good.clone();
        e.ciphertext[30] ^= 1;
        assert_eq!(v.verify_signature(&e), Err(VerifyError::BadSignature));
        let mut e = good.clone();
        e.record_id = "other".into();
        assert_eq!(v.verify_signature(&e), Err(VerifyError::BadSignature));
        let mut e = good.clone();
        e.vault_id = "other".into();
        assert_eq!(v.verify_signature(&e), Err(VerifyError::BadSignature));
        let mut e = good.clone();
        e.deleted = true;
        assert_eq!(v.verify_signature(&e), Err(VerifyError::BadSignature));
        let mut e = good.clone();
        e.version_vector.insert("x".into(), 5);
        assert_eq!(v.verify_signature(&e), Err(VerifyError::BadSignature));
        let mut e = good;
        e.signature[0] ^= 1;
        assert_eq!(v.verify_signature(&e), Err(VerifyError::BadSignature));
    }

    #[test]
    fn rejects_uncertified_device() {
        let account = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let mut rogue = identity(&account, 3);
        // The device signs with a key the account never certified.
        let other = DeviceSigningKey::from_seed([77; 32]);
        rogue.signing_key = other;
        let v = Verifier::new(account.signing.public());
        assert_eq!(
            v.verify_signature(&envelope(&rogue, 1)),
            Err(VerifyError::SignerMismatch)
        );
        // A self-issued certificate is not signed by the account.
        let imposter = AccountKeyPair::from_seeds([5; 32], [6; 32]);
        let fake = identity(&imposter, 3);
        assert_eq!(
            v.verify_signature(&envelope(&fake, 1)),
            Err(VerifyError::UntrustedCertificate)
        );
    }

    #[test]
    fn rejects_cert_from_another_account_but_accepts_after_trusting_it() {
        let mine = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let theirs = AccountKeyPair::from_seeds([3; 32], [4; 32]);
        let their_device = identity(&theirs, 8);
        let mut v = Verifier::new(mine.signing.public());
        let env = envelope(&their_device, 1);
        assert_eq!(
            v.verify_signature(&env),
            Err(VerifyError::UntrustedCertificate)
        );
        v.trust_account(theirs.signing.public());
        assert!(v.verify_signature(&env).is_ok());
    }

    #[test]
    fn rejects_missing_certificate() {
        let account = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let id = identity(&account, 3);
        let v = Verifier::new(account.signing.public());
        let mut e = envelope(&id, 1);
        e.cert.clear();
        assert!(matches!(
            v.verify_signature(&e),
            Err(VerifyError::Malformed(_))
        ));
    }

    #[test]
    fn rollback_is_rejected_but_redelivery_and_newer_are_fine() {
        let account = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let id = identity(&account, 3);
        let mut v = Verifier::new(account.signing.public());
        let old = envelope(&id, 1);
        let new = envelope(&id, 2);
        v.verify_and_record(&old).unwrap();
        v.verify_and_record(&new).unwrap();
        v.verify_and_record(&new).unwrap();
        assert_eq!(v.verify_and_record(&old), Err(VerifyError::Rollback));
    }

    #[test]
    fn high_water_snapshot_round_trips() {
        let account = AccountKeyPair::from_seeds([1; 32], [2; 32]);
        let id = identity(&account, 3);
        let mut v = Verifier::new(account.signing.public());
        v.verify_and_record(&envelope(&id, 2)).unwrap();
        let restored = HighWater::from_snapshot(v.high_water().snapshot());
        assert_eq!(&restored, v.high_water());
        let mut v2 = Verifier::new(account.signing.public()).with_high_water(restored);
        assert_eq!(
            v2.verify_and_record(&envelope(&id, 1)),
            Err(VerifyError::Rollback)
        );
    }
}
