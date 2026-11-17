//! Safety numbers for account public keys.
//!
//! Before an owner shares a vault with someone, both sides should see the
//! same number for that person's account and compare them over another
//! channel (a call, in person). The number covers both public keys of the
//! account, the X25519 key vault keys are sealed to and the Ed25519 key that
//! certifies devices, so a server that substitutes either one changes it.
//!
//! ```text
//! h      = SHA-256("easyssh/v1/account-fingerprint" || x25519_pub(32) || ed25519_pub(32))
//! group  = SHA-256(h || i as u8)[0..5] read as a big-endian integer, mod 100000, i = 0..12
//! ```
//!
//! Twelve groups of five digits (about 199 bits shown) separated by spaces.

use sha2::{Digest, Sha256};

pub const FINGERPRINT_CONTEXT: &[u8] = b"easyssh/v1/account-fingerprint";
/// Number of five-digit groups.
pub const FINGERPRINT_GROUPS: usize = 12;

/// The safety number of an account's public keys, as 12 space-separated
/// groups of 5 digits.
pub fn account_fingerprint(seal_public: &[u8; 32], signing_public: &[u8; 32]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_CONTEXT);
    hasher.update(seal_public);
    hasher.update(signing_public);
    let h = hasher.finalize();
    let mut groups = Vec::with_capacity(FINGERPRINT_GROUPS);
    for i in 0..FINGERPRINT_GROUPS {
        let mut block = Sha256::new();
        block.update(h);
        block.update([i as u8]);
        let digest = block.finalize();
        let mut n = 0u64;
        for byte in &digest[..5] {
            n = (n << 8) | u64::from(*byte);
        }
        groups.push(format!("{:05}", n % 100_000));
    }
    groups.join(" ")
}

/// Compares two fingerprints the way a person would: digits only, so spacing
/// and punctuation typed or pasted by the user do not matter.
pub fn fingerprints_match(a: &str, b: &str) -> bool {
    let digits = |s: &str| s.chars().filter(char::is_ascii_digit).collect::<String>();
    let (a, b) = (digits(a), digits(b));
    !a.is_empty() && a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_and_stability() {
        let f = account_fingerprint(&[1; 32], &[2; 32]);
        let groups: Vec<&str> = f.split(' ').collect();
        assert_eq!(groups.len(), FINGERPRINT_GROUPS);
        assert!(groups
            .iter()
            .all(|g| g.len() == 5 && g.chars().all(|c| c.is_ascii_digit())));
        assert_eq!(f, account_fingerprint(&[1; 32], &[2; 32]));
    }

    #[test]
    fn either_key_changes_it() {
        let base = account_fingerprint(&[1; 32], &[2; 32]);
        assert_ne!(base, account_fingerprint(&[3; 32], &[2; 32]));
        assert_ne!(base, account_fingerprint(&[1; 32], &[3; 32]));
        // The order of the two keys matters.
        assert_ne!(base, account_fingerprint(&[2; 32], &[1; 32]));
    }

    #[test]
    fn matching_ignores_spacing() {
        let f = account_fingerprint(&[9; 32], &[8; 32]);
        assert!(fingerprints_match(&f, &f.replace(' ', "-")));
        assert!(!fingerprints_match(&f, "00000"));
        assert!(!fingerprints_match("", ""));
    }
}
