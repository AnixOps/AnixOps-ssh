//! Version vectors: one monotonically increasing counter per device.

use crate::{CryptoError, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// How two version vectors relate. `a.compare(&b)` reads "a is `Before` b".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ordering {
    Before,
    After,
    Equal,
    Concurrent,
}

/// Device id -> counter. A missing device counts as 0, and zero entries are
/// never stored, so equal vectors always serialize identically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionVector {
    counters: BTreeMap<String, u64>,
}

impl VersionVector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build from `(device, counter)` pairs; zero counters are dropped and
    /// repeated devices keep the maximum.
    pub fn from_entries<'a>(entries: impl IntoIterator<Item = (&'a str, u64)>) -> Self {
        let mut v = Self::new();
        for (d, c) in entries {
            if c > 0 {
                let e = v.counters.entry(d.to_owned()).or_insert(0);
                *e = (*e).max(c);
            }
        }
        v
    }

    pub fn get(&self, device: &str) -> u64 {
        self.counters.get(device).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    /// Entries in canonical (byte-wise sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.counters.iter().map(|(k, v)| (k.as_str(), *v))
    }

    /// Record a local event on `device`; returns the new counter.
    pub fn increment(&mut self, device: &str) -> u64 {
        let c = self.counters.entry(device.to_owned()).or_insert(0);
        *c = c.saturating_add(1);
        *c
    }

    /// Element-wise maximum.
    pub fn merge(&mut self, other: &VersionVector) {
        for (d, c) in &other.counters {
            let e = self.counters.entry(d.clone()).or_insert(0);
            *e = (*e).max(*c);
        }
    }

    pub fn compare(&self, other: &VersionVector) -> Ordering {
        let mut less = false;
        let mut greater = false;
        for d in self.counters.keys().chain(other.counters.keys()) {
            let (a, b) = (self.get(d), other.get(d));
            less |= a < b;
            greater |= a > b;
        }
        match (less, greater) {
            (false, false) => Ordering::Equal,
            (true, false) => Ordering::Before,
            (false, true) => Ordering::After,
            (true, true) => Ordering::Concurrent,
        }
    }

    /// Canonical encoding: `count(u32 BE)` then, for each entry in ascending
    /// byte order of the device id, `len(u32 BE) || device_utf8 || counter(u64 BE)`.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.counters.len() * 24);
        out.extend_from_slice(&(self.counters.len() as u32).to_be_bytes());
        for (d, c) in &self.counters {
            crate::put_lp(&mut out, d.as_bytes());
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    /// Strict inverse of [`to_canonical_bytes`](Self::to_canonical_bytes):
    /// rejects unsorted or duplicate devices, zero counters and trailing bytes.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let bad = CryptoError::Malformed("version vector");
        let mut pos = 0usize;
        let mut take = |n: usize| -> Result<&[u8]> {
            let end = pos.checked_add(n).filter(|e| *e <= bytes.len());
            let end = end.ok_or(CryptoError::Malformed("version vector"))?;
            let s = &bytes[pos..end];
            pos = end;
            Ok(s)
        };
        let count = u32::from_be_bytes(take(4)?.try_into().expect("4"));
        let mut counters = BTreeMap::new();
        let mut prev: Option<String> = None;
        for _ in 0..count {
            let len = u32::from_be_bytes(take(4)?.try_into().expect("4")) as usize;
            let name = std::str::from_utf8(take(len)?)
                .map_err(|_| CryptoError::Malformed("version vector"))?
                .to_owned();
            let c = u64::from_be_bytes(take(8)?.try_into().expect("8"));
            if c == 0
                || prev
                    .as_ref()
                    .is_some_and(|p| p.as_bytes() >= name.as_bytes())
            {
                return Err(bad);
            }
            prev = Some(name.clone());
            counters.insert(name, c);
        }
        if pos != bytes.len() {
            return Err(bad);
        }
        Ok(Self { counters })
    }

    /// SHA-256 of the canonical encoding; goes into the record AAD.
    pub fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.to_canonical_bytes()).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vv(e: &[(&str, u64)]) -> VersionVector {
        VersionVector::from_entries(e.iter().copied())
    }

    #[test]
    fn increment_and_get() {
        let mut v = VersionVector::new();
        assert_eq!(v.get("a"), 0);
        assert_eq!(v.increment("a"), 1);
        assert_eq!(v.increment("a"), 2);
        assert_eq!(v.get("a"), 2);
    }

    #[test]
    fn ordering() {
        let a = vv(&[("a", 1)]);
        let b = vv(&[("a", 2)]);
        assert_eq!(a.compare(&b), Ordering::Before);
        assert_eq!(b.compare(&a), Ordering::After);
        assert_eq!(a.compare(&a.clone()), Ordering::Equal);
        assert_eq!(VersionVector::new().compare(&a), Ordering::Before);
        assert_eq!(
            VersionVector::new().compare(&VersionVector::new()),
            Ordering::Equal
        );
        let c = vv(&[("a", 1), ("b", 1)]);
        assert_eq!(a.compare(&c), Ordering::Before);
        let x = vv(&[("a", 2), ("b", 1)]);
        let y = vv(&[("a", 1), ("b", 2)]);
        assert_eq!(x.compare(&y), Ordering::Concurrent);
        assert_eq!(y.compare(&x), Ordering::Concurrent);
        assert_eq!(
            vv(&[("a", 1)]).compare(&vv(&[("b", 1)])),
            Ordering::Concurrent
        );
    }

    #[test]
    fn merge_is_max_and_dominates() {
        let x = vv(&[("a", 2), ("b", 1)]);
        let y = vv(&[("a", 1), ("b", 2), ("c", 5)]);
        let mut m = x.clone();
        m.merge(&y);
        assert_eq!(m, vv(&[("a", 2), ("b", 2), ("c", 5)]));
        assert_eq!(x.compare(&m), Ordering::Before);
        assert_eq!(y.compare(&m), Ordering::Before);
        let mut m2 = y.clone();
        m2.merge(&x);
        assert_eq!(m, m2);
    }

    #[test]
    fn canonical_is_order_independent_and_round_trips() {
        let a = vv(&[("b", 2), ("a", 1), ("zz", 9)]);
        let mut b = VersionVector::new();
        b.merge(&vv(&[("zz", 9)]));
        b.merge(&vv(&[("a", 1)]));
        b.merge(&vv(&[("b", 2)]));
        assert_eq!(a.to_canonical_bytes(), b.to_canonical_bytes());
        assert_eq!(a.digest(), b.digest());
        assert_eq!(
            VersionVector::from_canonical_bytes(&a.to_canonical_bytes()).unwrap(),
            a
        );
        // zero entries are not part of the vector
        assert_eq!(vv(&[("a", 0)]), VersionVector::new());
    }

    #[test]
    fn canonical_parse_is_strict() {
        let good = vv(&[("a", 1), ("b", 2)]).to_canonical_bytes();
        let mut trailing = good.clone();
        trailing.push(0);
        assert!(VersionVector::from_canonical_bytes(&trailing).is_err());
        assert!(VersionVector::from_canonical_bytes(&good[..good.len() - 1]).is_err());
        // unsorted: build b then a by hand
        let mut bad = vec![0, 0, 0, 2];
        for (d, c) in [("b", 1u64), ("a", 1)] {
            bad.extend_from_slice(&1u32.to_be_bytes());
            bad.extend_from_slice(d.as_bytes());
            bad.extend_from_slice(&c.to_be_bytes());
        }
        assert!(VersionVector::from_canonical_bytes(&bad).is_err());
        // huge count must not allocate or panic
        assert!(VersionVector::from_canonical_bytes(&[0xff, 0xff, 0xff, 0xff]).is_err());
    }

    #[test]
    fn digest_changes_with_content() {
        assert_ne!(vv(&[("a", 1)]).digest(), vv(&[("a", 2)]).digest());
        assert_ne!(vv(&[("a", 1)]).digest(), vv(&[("b", 1)]).digest());
    }
}
