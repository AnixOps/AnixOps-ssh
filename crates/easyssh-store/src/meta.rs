//! Sync metadata carried by every syncable row, and the `bump` helper.

use crate::error::{Result, StoreError};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::Row;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The Lamport clock and device that last wrote one field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldStamp {
    pub lamport: u64,
    pub device: String,
}

/// Routing and merge metadata of one record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub id: String,
    pub vault_id: String,
    pub updated_at: DateTime<Utc>,
    /// Device id to the number of writes that device has made to this record.
    pub version_vector: BTreeMap<String, u64>,
    /// Field name to the stamp of its last write.
    pub field_stamps: BTreeMap<String, FieldStamp>,
    pub deleted: bool,
}

impl Meta {
    pub fn new(id: impl Into<String>, vault_id: impl Into<String>, now: DateTime<Utc>) -> Self {
        let now = trunc_us(now);
        Self {
            id: id.into(),
            vault_id: vault_id.into(),
            updated_at: now,
            version_vector: BTreeMap::new(),
            field_stamps: BTreeMap::new(),
            deleted: false,
        }
    }

    /// Records one write by `device_id` that changed `changed_fields`: the
    /// device's counter in the version vector grows by one and every changed
    /// field gets a stamp whose Lamport value is above everything this record
    /// has seen. Returns that Lamport value.
    pub fn bump(&mut self, changed_fields: &[&str], device_id: &str, now: DateTime<Utc>) -> u64 {
        let now = trunc_us(now);
        let lamport = self
            .field_stamps
            .values()
            .map(|stamp| stamp.lamport)
            .max()
            .unwrap_or(0)
            + 1;
        *self.version_vector.entry(device_id.to_owned()).or_insert(0) += 1;
        for field in changed_fields {
            self.field_stamps.insert(
                (*field).to_owned(),
                FieldStamp {
                    lamport,
                    device: device_id.to_owned(),
                },
            );
        }
        self.updated_at = now;
        lamport
    }

    pub(crate) fn from_row(row: &Row<'_>) -> Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            vault_id: row.get("vault_id")?,
            updated_at: parse_ts(&row.get::<_, String>("updated_at")?)?,
            version_vector: serde_json::from_str(&row.get::<_, String>("version_vector")?)?,
            field_stamps: serde_json::from_str(&row.get::<_, String>("field_stamps")?)?,
            deleted: row.get::<_, i64>("deleted")? != 0,
        })
    }
}

/// Storage keeps microseconds; truncating up front makes what a write returns
/// equal to what a later read returns.
pub(crate) fn trunc_us(value: DateTime<Utc>) -> DateTime<Utc> {
    DateTime::from_timestamp_micros(value.timestamp_micros()).unwrap_or(value)
}

pub(crate) fn fmt_ts(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub(crate) fn parse_ts(value: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|error| StoreError::Invalid(format!("bad timestamp {value:?}: {error}")))
}

/// One field overwritten by a write, as `Store::bump` needs it.
#[derive(Debug, Clone)]
pub struct FieldChange<'a> {
    pub field: &'a str,
    /// The value being overwritten (`None` when the field had none).
    pub old: Option<serde_json::Value>,
    /// Secret values are encrypted before they enter the history table.
    pub secret: bool,
}

/// One restorable overwritten value.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub seq: i64,
    pub record_table: String,
    pub record_id: String,
    pub field: String,
    pub old_value: Option<serde_json::Value>,
    pub changed_at: DateTime<Utc>,
    pub device: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_advances_vector_and_stamps() {
        let now = Utc::now();
        let mut meta = Meta::new("r", "v", now);
        assert_eq!(meta.bump(&["name"], "dev-a", now), 1);
        assert_eq!(meta.bump(&["color", "name"], "dev-b", now), 2);
        assert_eq!(meta.bump(&["color"], "dev-a", now), 3);
        assert_eq!(meta.version_vector["dev-a"], 2);
        assert_eq!(meta.version_vector["dev-b"], 1);
        assert_eq!(meta.field_stamps["name"].lamport, 2);
        assert_eq!(meta.field_stamps["name"].device, "dev-b");
        assert_eq!(meta.field_stamps["color"].lamport, 3);
    }
}
