//! The decrypted form of one record version and how local edits change it.

use easyssh_crypto::VersionVector;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Schema version of the plaintext payload (also the AAD schema number).
pub const PAYLOAD_VERSION: u32 = 1;

/// Which device wrote a field and when, as a Lamport clock. The derived
/// order is `(lamport, device)`: the larger stamp wins a same-field conflict.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stamp {
    pub lamport: u64,
    pub device: String,
}

/// One record as clients see it after decryption: field values, one stamp
/// per field and the delete stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordVersion {
    pub table: String,
    pub version_vector: VersionVector,
    /// Field name to value. A removed field keeps its stamp with `null`.
    pub fields: BTreeMap<String, Value>,
    pub stamps: BTreeMap<String, Stamp>,
    /// Stamp of the newest delete. The record is deleted only while this is
    /// newer than every field stamp (see [`RecordVersion::is_deleted`]).
    pub tombstone: Option<Stamp>,
}

/// Exactly what is encrypted. The version vector lives in the envelope (and
/// in the AAD); the table is repeated here as a consistency check against
/// the (signed) routing metadata.
#[derive(Serialize, Deserialize)]
struct Payload {
    v: u32,
    table: String,
    fields: BTreeMap<String, Value>,
    stamps: BTreeMap<String, Stamp>,
    tombstone: Option<Stamp>,
}

impl RecordVersion {
    pub fn new(table: impl Into<String>) -> Self {
        Self {
            table: table.into(),
            version_vector: VersionVector::new(),
            fields: BTreeMap::new(),
            stamps: BTreeMap::new(),
            tombstone: None,
        }
    }

    /// Deleted iff a tombstone exists and its stamp is strictly newer than
    /// every field stamp. A later edit therefore brings a record back.
    pub fn is_deleted(&self) -> bool {
        match &self.tombstone {
            Some(tomb) => self.stamps.values().all(|stamp| tomb > stamp),
            None => false,
        }
    }

    /// Highest Lamport value this record has seen, including the tombstone.
    pub fn max_lamport(&self) -> u64 {
        self.stamps
            .values()
            .chain(self.tombstone.as_ref())
            .map(|stamp| stamp.lamport)
            .max()
            .unwrap_or(0)
    }

    /// Record a local write by `device`: the device counter grows and every
    /// changed field gets one new stamp above everything seen so far.
    /// Setting a field to `Value::Null` removes it. Returns the new Lamport.
    pub fn edit<I, K>(&mut self, device: &str, changes: I) -> u64
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        let lamport = self.max_lamport() + 1;
        self.version_vector.increment(device);
        for (field, value) in changes {
            let field = field.into();
            self.stamps.insert(
                field.clone(),
                Stamp {
                    lamport,
                    device: device.to_owned(),
                },
            );
            self.fields.insert(field, value);
        }
        lamport
    }

    /// Record a local delete. Field values stay in the payload so history
    /// can restore them.
    pub fn delete(&mut self, device: &str) -> u64 {
        let lamport = self.max_lamport() + 1;
        self.version_vector.increment(device);
        self.tombstone = Some(Stamp {
            lamport,
            device: device.to_owned(),
        });
        lamport
    }

    /// The bytes that get encrypted.
    pub fn to_plaintext(&self) -> Vec<u8> {
        serde_json::to_vec(&Payload {
            v: PAYLOAD_VERSION,
            table: self.table.clone(),
            fields: self.fields.clone(),
            stamps: self.stamps.clone(),
            tombstone: self.tombstone.clone(),
        })
        .expect("payload serializes")
    }

    /// Parse decrypted bytes; `version_vector` comes from the envelope.
    pub fn from_plaintext(
        bytes: &[u8],
        version_vector: VersionVector,
    ) -> Result<Self, &'static str> {
        let payload: Payload =
            serde_json::from_slice(bytes).map_err(|_| "payload is not valid JSON")?;
        if payload.v != PAYLOAD_VERSION {
            return Err("unsupported payload version");
        }
        if payload
            .fields
            .keys()
            .any(|f| !payload.stamps.contains_key(f))
        {
            return Err("field without a stamp");
        }
        Ok(Self {
            table: payload.table,
            version_vector,
            fields: payload.fields,
            stamps: payload.stamps,
            tombstone: payload.tombstone,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn edit_advances_vector_and_lamport() {
        let mut r = RecordVersion::new("hosts");
        assert_eq!(r.edit("a", [("name", json!("x"))]), 1);
        assert_eq!(r.edit("b", [("name", json!("y")), ("port", json!(22))]), 2);
        assert_eq!(r.version_vector.get("a"), 1);
        assert_eq!(r.version_vector.get("b"), 1);
        assert_eq!(r.stamps["port"].device, "b");
        assert!(!r.is_deleted());
    }

    #[test]
    fn delete_then_edit_revives() {
        let mut r = RecordVersion::new("hosts");
        r.edit("a", [("name", json!("x"))]);
        r.delete("a");
        assert!(r.is_deleted());
        r.edit("a", [("name", json!("z"))]);
        assert!(!r.is_deleted());
        assert!(r.tombstone.is_some());
    }

    #[test]
    fn plaintext_round_trip_and_validation() {
        let mut r = RecordVersion::new("hosts");
        r.edit("a", [("name", json!("x"))]);
        let bytes = r.to_plaintext();
        let back = RecordVersion::from_plaintext(&bytes, r.version_vector.clone()).unwrap();
        assert_eq!(back, r);
        assert!(RecordVersion::from_plaintext(b"{}", VersionVector::new()).is_err());
        let orphan = br#"{"v":1,"table":"t","fields":{"a":1},"stamps":{},"tombstone":null}"#;
        assert!(RecordVersion::from_plaintext(orphan, VersionVector::new()).is_err());
    }
}
