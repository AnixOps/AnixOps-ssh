//! Field-level merge of two versions of one record.
//!
//! Rules (documented in the crate README):
//!
//! 1. Compare version vectors. `Equal`: nothing to do. `Before`/`After`: the
//!    causally newer side wins as a whole (no field-level work).
//! 2. `Concurrent`: every field is taken from the side with the larger
//!    [`Stamp`] `(lamport, device)`. Fields edited on only one side survive.
//!    The value that loses a same-field conflict is reported as
//!    [`Overwritten`] so the store writes it to history. A local value is
//!    always reported when replaced; a remote value only if the local side
//!    never saw the write that produced it (otherwise it is merely stale).
//! 3. The tombstone is the larger of the two tombstones. The merged record is
//!    deleted only if that tombstone is strictly newer than every field stamp
//!    of the merged record; an edit stamped later than the delete keeps the
//!    record alive.

use crate::error::MergeError;
use crate::record::{RecordVersion, Stamp};
use easyssh_crypto::Ordering;
use serde_json::Value;

/// Which input a lost value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Local,
    Remote,
}

/// A value a merge did not keep. The store records it in `record_history`
/// so a user can restore it.
#[derive(Debug, Clone, PartialEq)]
pub struct Overwritten {
    pub field: String,
    pub value: Value,
    /// Stamp the lost value carried (who wrote it and when).
    pub stamp: Option<Stamp>,
    pub side: Side,
    /// Stamp of the value that replaced it.
    pub replaced_by: Option<Stamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Same version; nothing changed.
    Equal,
    /// Local is causally newer; keep it (it may still need pushing).
    KeepLocal,
    /// Remote is causally newer; adopt it.
    TakeRemote,
    /// Concurrent versions were combined; the result needs a new version.
    Merged,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub outcome: Outcome,
    pub version: RecordVersion,
    pub overwritten: Vec<Overwritten>,
}

/// Merge `remote` into `local`. Pure and deterministic: swapping the two
/// arguments gives the same fields, stamps, tombstone and vector.
pub fn merge(local: &RecordVersion, remote: &RecordVersion) -> Result<Merged, MergeError> {
    if local.table != remote.table {
        return Err(MergeError::TableMismatch {
            local: local.table.clone(),
            remote: remote.table.clone(),
        });
    }
    match local.version_vector.compare(&remote.version_vector) {
        Ordering::Equal if local == remote => Ok(Merged {
            outcome: Outcome::Equal,
            version: local.clone(),
            overwritten: Vec::new(),
        }),
        Ordering::After => Ok(Merged {
            outcome: Outcome::KeepLocal,
            version: local.clone(),
            overwritten: Vec::new(),
        }),
        Ordering::Before => Ok(Merged {
            outcome: Outcome::TakeRemote,
            version: remote.clone(),
            overwritten: replaced_by_newer(local, remote),
        }),
        // Concurrent, or equal vectors with different content (a device that
        // reused a counter): combine field by field.
        Ordering::Concurrent | Ordering::Equal => Ok(combine(local, remote)),
    }
}

/// Local values the causally newer remote version replaced.
fn replaced_by_newer(local: &RecordVersion, remote: &RecordVersion) -> Vec<Overwritten> {
    let mut out = Vec::new();
    for (field, value) in &local.fields {
        if value.is_null() || remote.fields.get(field) == Some(value) {
            continue;
        }
        out.push(Overwritten {
            field: field.clone(),
            value: value.clone(),
            stamp: local.stamps.get(field).cloned(),
            side: Side::Local,
            replaced_by: remote.stamps.get(field).cloned(),
        });
    }
    out
}

fn combine(local: &RecordVersion, remote: &RecordVersion) -> Merged {
    let mut version = RecordVersion::new(local.table.clone());
    version.version_vector = local.version_vector.clone();
    version.version_vector.merge(&remote.version_vector);
    let mut overwritten = Vec::new();

    let names: std::collections::BTreeSet<&String> = local
        .stamps
        .keys()
        .chain(remote.stamps.keys())
        .chain(local.fields.keys())
        .chain(remote.fields.keys())
        .collect();

    for name in names {
        let (lv, ls) = (local.fields.get(name), local.stamps.get(name));
        let (rv, rs) = (remote.fields.get(name), remote.stamps.get(name));
        // `None < Some(_)`; ties on the stamp (which only a misbehaving
        // device can produce) are broken by the serialized value so the
        // result does not depend on argument order.
        let local_wins = match ls.cmp(&rs) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => false,
            std::cmp::Ordering::Equal => lv.map(|v| v.to_string()) >= rv.map(|v| v.to_string()),
        };
        let (win_v, win_s, lose_v, lose_s, lose_side) = if local_wins {
            (lv, ls, rv, rs, Side::Remote)
        } else {
            (rv, rs, lv, ls, Side::Local)
        };
        if let Some(value) = win_v {
            version.fields.insert(name.clone(), value.clone());
        }
        if let Some(stamp) = win_s {
            version.stamps.insert(name.clone(), stamp.clone());
        }
        if let Some(lost) = lose_v {
            // A remote value is only news if this device never saw the write
            // that produced it; otherwise it is an old value already
            // superseded here (and already in history).
            let unseen = lose_side == Side::Local
                || lose_s.is_none_or(|s| {
                    remote.version_vector.get(&s.device) > local.version_vector.get(&s.device)
                });
            if unseen && !lost.is_null() && win_v != Some(lost) {
                overwritten.push(Overwritten {
                    field: name.clone(),
                    value: lost.clone(),
                    stamp: lose_s.cloned(),
                    side: lose_side,
                    replaced_by: win_s.cloned(),
                });
            }
        }
    }

    version.tombstone = match (&local.tombstone, &remote.tombstone) {
        (Some(a), Some(b)) => Some(a.max(b).clone()),
        (Some(a), None) | (None, Some(a)) => Some(a.clone()),
        (None, None) => None,
    };

    Merged {
        outcome: Outcome::Merged,
        version,
        overwritten,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base() -> RecordVersion {
        let mut r = RecordVersion::new("hosts");
        r.edit(
            "a",
            [
                ("name", json!("web")),
                ("host", json!("10.0.0.1")),
                ("port", json!(22)),
            ],
        );
        r
    }

    #[test]
    fn equal() {
        let r = base();
        let m = merge(&r, &r.clone()).unwrap();
        assert_eq!(m.outcome, Outcome::Equal);
        assert_eq!(m.version, r);
        assert!(m.overwritten.is_empty());
    }

    #[test]
    fn after_keeps_local() {
        let old = base();
        let mut new = old.clone();
        new.edit("a", [("name", json!("web2"))]);
        let m = merge(&new, &old).unwrap();
        assert_eq!(m.outcome, Outcome::KeepLocal);
        assert_eq!(m.version, new);
        assert!(m.overwritten.is_empty());
    }

    #[test]
    fn before_takes_remote_and_reports_replaced_local_values() {
        let old = base();
        let mut new = old.clone();
        new.edit("b", [("name", json!("web2"))]);
        let m = merge(&old, &new).unwrap();
        assert_eq!(m.outcome, Outcome::TakeRemote);
        assert_eq!(m.version, new);
        assert_eq!(m.overwritten.len(), 1);
        assert_eq!(m.overwritten[0].field, "name");
        assert_eq!(m.overwritten[0].value, json!("web"));
        assert_eq!(m.overwritten[0].side, Side::Local);
    }

    #[test]
    fn concurrent_different_fields_merge_without_loss() {
        let root = base();
        let mut a = root.clone();
        let mut b = root;
        a.edit("a", [("name", json!("web-a"))]);
        b.edit("b", [("port", json!(2222))]);
        let m = merge(&a, &b).unwrap();
        assert_eq!(m.outcome, Outcome::Merged);
        assert_eq!(m.version.fields["name"], json!("web-a"));
        assert_eq!(m.version.fields["port"], json!(2222));
        assert_eq!(m.version.fields["host"], json!("10.0.0.1"));
        // No conflict: the only overwritten value is the superseded local
        // `port` (it was replaced causally, not lost). Nothing from `b`
        // that `a` never saw is dropped.
        assert_eq!(m.overwritten.len(), 1);
        assert_eq!(m.overwritten[0].field, "port");
        assert_eq!(m.overwritten[0].value, json!(22));
        assert_eq!(m.version.version_vector.get("a"), 2);
        assert_eq!(m.version.version_vector.get("b"), 1);
    }

    #[test]
    fn concurrent_same_field_lww_by_lamport_then_device() {
        let root = base();
        let mut a = root.clone();
        let mut b = root;
        a.edit("a", [("name", json!("from-a"))]);
        b.edit("b", [("name", json!("from-b"))]);
        // Same Lamport value: the larger device id wins.
        let m = merge(&a, &b).unwrap();
        assert_eq!(m.version.fields["name"], json!("from-b"));
        assert_eq!(m.overwritten.len(), 1);
        assert_eq!(m.overwritten[0].value, json!("from-a"));
        assert_eq!(m.overwritten[0].side, Side::Local);
        // Merge is symmetric; the loser is now reported as remote.
        let m2 = merge(&b, &a).unwrap();
        assert_eq!(m2.version, m.version);
        assert_eq!(m2.overwritten[0].side, Side::Remote);

        // A higher Lamport beats a larger device id.
        a.edit("a", [("name", json!("a-again"))]);
        let m3 = merge(&a, &b).unwrap();
        assert_eq!(m3.version.fields["name"], json!("a-again"));
        assert_eq!(m3.overwritten[0].value, json!("from-b"));
    }

    #[test]
    fn merge_is_commutative_and_idempotent() {
        let root = base();
        let mut a = root.clone();
        let mut b = root;
        a.edit("a", [("name", json!("1")), ("port", json!(1))]);
        b.edit("b", [("name", json!("2")), ("host", json!("h"))]);
        b.delete("b");
        let ab = merge(&a, &b).unwrap().version;
        let ba = merge(&b, &a).unwrap().version;
        assert_eq!(ab, ba);
        let again = merge(&ab, &a).unwrap();
        assert_eq!(again.outcome, Outcome::KeepLocal);
        assert_eq!(again.version, ab);
    }

    #[test]
    fn tombstone_wins_only_when_newer_than_every_edited_field() {
        let root = base();

        // Delete (lamport 3) vs edit (lamport 2): the delete is newer.
        let mut editor = root.clone();
        let mut deleter = root.clone();
        editor.edit("a", [("name", json!("renamed"))]);
        deleter.edit("b", [("port", json!(1))]);
        deleter.delete("b");
        let m = merge(&editor, &deleter).unwrap();
        assert!(m.version.is_deleted());
        assert_eq!(m.version.fields["name"], json!("renamed"), "data is kept");

        // Edit (lamport 3) vs delete (lamport 2): the edit is newer.
        let mut editor = root.clone();
        let mut deleter = root;
        editor.edit("a", [("port", json!(1))]);
        editor.edit("a", [("name", json!("renamed"))]);
        deleter.delete("b");
        let m = merge(&editor, &deleter).unwrap();
        assert!(!m.version.is_deleted());
        assert!(m.version.tombstone.is_some());
        assert_eq!(m.version.fields["name"], json!("renamed"));
    }

    #[test]
    fn tombstone_ties_are_not_wins_for_the_older_stamp() {
        // Equal lamport: ordering falls back to the device id, deterministically.
        let root = base();
        let mut editor = root.clone();
        let mut deleter = root;
        editor.edit("z", [("name", json!("n"))]);
        deleter.delete("b");
        let m = merge(&editor, &deleter).unwrap();
        // field stamp (2, "z") > tombstone (2, "b")
        assert!(!m.version.is_deleted());
        let m = merge(&deleter, &editor).unwrap();
        assert!(!m.version.is_deleted());
    }

    #[test]
    fn delete_after_causal_edit_is_a_plain_delete() {
        let root = base();
        let mut next = root.clone();
        next.delete("a");
        let m = merge(&root, &next).unwrap();
        assert_eq!(m.outcome, Outcome::TakeRemote);
        assert!(m.version.is_deleted());
    }

    #[test]
    fn table_mismatch_is_rejected() {
        let a = RecordVersion::new("hosts");
        let b = RecordVersion::new("keys");
        assert!(matches!(
            merge(&a, &b),
            Err(MergeError::TableMismatch { .. })
        ));
    }

    #[test]
    fn same_vector_different_content_is_combined() {
        let mut a = RecordVersion::new("t");
        let mut b = RecordVersion::new("t");
        a.edit("d", [("x", json!(1))]);
        b.edit("d", [("y", json!(2))]);
        let m = merge(&a, &b).unwrap();
        assert_eq!(m.outcome, Outcome::Merged);
        assert_eq!(m.version.fields["x"], json!(1));
        assert_eq!(m.version.fields["y"], json!(2));
    }
}
