//! The `anixops-appcast-v1` feed of AnixOps (anix-kit, package `update`),
//! verified exactly as the kit's Go verifier does and failing closed on
//! everything else.
//!
//! The feed URL serves an envelope `{"format","payload","signature"}`. The
//! payload is the feed JSON as base64; the signature is Ed25519 over
//! [`FEED_PREFIX`] plus the payload bytes, so no canonical JSON is needed.
//! Each item carries its own signature over [`archive_message`].

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use std::io::Read;

pub const FORMAT: &str = "anixops-appcast-v1";
pub const SCHEMA: i64 = 1;
pub const FEED_PREFIX: &str = "anixops-appcast-v1\n";
pub const ARCHIVE_PREFIX: &str = "anixops-update-archive-v1\n";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("not an Ed25519 key in the AnixOps format")]
    Key,
    #[error("malformed feed")]
    Malformed,
    #[error("feed signature does not verify")]
    FeedSignature,
    #[error("feed is for another product")]
    WrongProduct,
    #[error("feed is older than one already accepted")]
    Stale,
    #[error("malformed item")]
    Item,
    #[error("item URL is not HTTPS")]
    InsecureUrl,
    #[error("archive does not match its item")]
    Archive,
    #[error("archive signature does not verify")]
    ArchiveSigning,
    #[error("could not read the archive")]
    Read,
}

/// One signed release.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub version: String,
    /// Orders releases; an update needs a higher build than the running one.
    pub build: i64,
    pub minimum_system_version: String,
    pub url: String,
    pub length: i64,
    /// 64 hex digits.
    pub sha256: String,
    pub signature: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
}

/// The signed payload.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Feed {
    pub schema: i64,
    pub product: String,
    #[serde(default)]
    pub channel: String,
    pub sequence: i64,
    pub generated_at: DateTime<Utc>,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Deserialize)]
struct Envelope {
    format: String,
    payload: String,
    signature: String,
}

pub fn feed_message(payload: &[u8]) -> Vec<u8> {
    [FEED_PREFIX.as_bytes(), payload].concat()
}

pub fn archive_message(product: &str, build: i64, length: i64, sha256_hex: &str) -> Vec<u8> {
    format!(
        "{ARCHIVE_PREFIX}{product}\n{build}\n{length}\n{}",
        sha256_hex.to_ascii_lowercase()
    )
    .into_bytes()
}

/// The key's id: 16 hex characters of SHA-256 of the raw public key.
pub fn key_id(key: &VerifyingKey) -> String {
    hex(&Sha256::digest(key.as_bytes())[..8])
}

/// Parses a public key in the AnixOps format: base64 of the 32 raw bytes.
/// (The kit also reads a PEM key; a build variable holds the base64 form.)
pub fn parse_public_key(text: &str) -> Result<VerifyingKey, Error> {
    let raw = STANDARD.decode(text.trim()).map_err(|_| Error::Key)?;
    let raw: [u8; 32] = raw.try_into().map_err(|_| Error::Key)?;
    VerifyingKey::from_bytes(&raw).map_err(|_| Error::Key)
}

/// Checks feeds and archives of one product against the keys the program
/// ships. More than one key lets a new key roll out before the old one is
/// retired.
#[derive(Debug, Clone)]
pub struct Verifier {
    product: String,
    keys: Vec<VerifyingKey>,
}

impl Verifier {
    /// At least one key is required, and every one must parse.
    pub fn new(product: &str, public_keys: &[&str]) -> Result<Verifier, Error> {
        if public_keys.is_empty() {
            return Err(Error::Key);
        }
        let keys = public_keys
            .iter()
            .map(|text| parse_public_key(text))
            .collect::<Result<_, _>>()?;
        Ok(Verifier {
            product: product.to_owned(),
            keys,
        })
    }

    /// Verifies a downloaded envelope and returns its feed. `last_accepted`
    /// is the highest sequence accepted before (0 when none): an older feed
    /// is refused, the same one is accepted again.
    pub fn verify_feed(&self, data: &[u8], last_accepted: i64) -> Result<Feed, Error> {
        let envelope: Envelope = serde_json::from_slice(data).map_err(|_| Error::Malformed)?;
        if envelope.format != FORMAT {
            return Err(Error::Malformed);
        }
        let payload = STANDARD
            .decode(&envelope.payload)
            .map_err(|_| Error::Malformed)?;
        if !self.verify(&feed_message(&payload), &envelope.signature) {
            return Err(Error::FeedSignature);
        }
        // Only signed bytes are parsed from here on.
        let feed: Feed = serde_json::from_slice(&payload).map_err(|_| Error::Malformed)?;
        if feed.schema != SCHEMA {
            return Err(Error::Malformed);
        }
        if feed.product != self.product {
            return Err(Error::WrongProduct);
        }
        if feed.sequence < last_accepted {
            return Err(Error::Stale);
        }
        for item in &feed.items {
            validate(item)?;
        }
        Ok(feed)
    }

    /// Reads the downloaded archive and checks its size, digest and
    /// signature against `item`. For the later installer step: run it on the
    /// file that will be opened, after the download finished.
    pub fn verify_archive(&self, mut archive: impl Read, item: &Item) -> Result<(), Error> {
        validate(item)?;
        let mut hasher = Sha256::new();
        let mut length: i64 = 0;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let n = archive.read(&mut buffer).map_err(|_| Error::Read)?;
            if n == 0 {
                break;
            }
            hasher.update(&buffer[..n]);
            length += n as i64;
        }
        let digest = hex(&hasher.finalize());
        if length != item.length || digest != item.sha256.to_ascii_lowercase() {
            return Err(Error::Archive);
        }
        let message = archive_message(&self.product, item.build, length, &digest);
        if !self.verify(&message, &item.signature) {
            return Err(Error::ArchiveSigning);
        }
        Ok(())
    }

    fn verify(&self, message: &[u8], signature: &str) -> bool {
        // Whitespace inside a signature is ignored, as the kit does.
        let compact: String = signature.split_whitespace().collect();
        let Ok(raw) = STANDARD.decode(compact) else {
            return false;
        };
        let Ok(signature) = Signature::from_slice(&raw) else {
            return false;
        };
        self.keys
            .iter()
            .any(|key| key.verify_strict(message, &signature).is_ok())
    }
}

/// The item with the highest build above `current_build` that runs on
/// `system_version`, or `None`.
pub fn newest<'a>(feed: &'a Feed, current_build: i64, system_version: &str) -> Option<&'a Item> {
    let system = parse_system_version(system_version)?;
    feed.items
        .iter()
        .filter(|item| {
            item.build > current_build
                && parse_system_version(&item.minimum_system_version)
                    .is_some_and(|minimum| system >= minimum)
        })
        .fold(None, |best: Option<&Item>, item| match best {
            Some(b) if b.build >= item.build => Some(b),
            _ => Some(item),
        })
}

/// Checks an item's fields.
pub fn validate(item: &Item) -> Result<(), Error> {
    if !is_https(&item.url) {
        return Err(Error::InsecureUrl);
    }
    let signature = STANDARD.decode(item.signature.replace(['\r', '\n'], ""));
    let digest_ok = item.sha256.len() == 64 && item.sha256.bytes().all(|b| b.is_ascii_hexdigit());
    if item.build <= 0
        || item.length <= 0
        || !signature.is_ok_and(|s| s.len() == 64)
        || !digest_ok
        || parse_system_version(&item.minimum_system_version).is_none()
    {
        return Err(Error::Item);
    }
    Ok(())
}

/// `https://host/...` with a host and no credentials.
pub fn is_https(url: &str) -> bool {
    let Some(rest) = url
        .get(..8)
        .filter(|s| s.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty() && !authority.contains('@') && !url.chars().any(char::is_whitespace)
}

/// "14", "14.2" or "14.2.1" (digits only, three parts at most).
fn parse_system_version(text: &str) -> Option<[u64; 3]> {
    let mut out = [0u64; 3];
    for (count, part) in text.split('.').enumerate() {
        if count == 3 || part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out[count] = part.parse().ok()?;
    }
    Some(out)
}

/// The build number of an app version `major.minor.patch`:
/// `major * 1_000_000 + minor * 1_000 + patch`, the number the mobile
/// releases already use as versionCode. A suffix after `-` or `+` is ignored.
pub fn build_from_version(version: &str) -> Option<i64> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = |max: i64| -> Option<i64> {
        let n: i64 = parts.next()?.parse().ok()?;
        (0..max).contains(&n).then_some(n)
    };
    let (major, minor, patch) = (next(2_000)?, next(1_000)?, next(1_000)?);
    parts
        .next()
        .is_none()
        .then_some(major * 1_000_000 + minor * 1_000 + patch)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    pub const PRODUCT: &str = "anixops-ssh-linux";

    pub fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    pub fn public(key: &SigningKey) -> String {
        STANDARD.encode(key.verifying_key().as_bytes())
    }

    pub fn sign(key: &SigningKey, message: &[u8]) -> String {
        STANDARD.encode(key.sign(message).to_bytes())
    }

    pub fn item(key: &SigningKey, build: i64, archive: &[u8]) -> serde_json::Value {
        let digest = hex(&Sha256::digest(archive));
        let length = archive.len() as i64;
        serde_json::json!({
            "version": format!("2.{build}.0"),
            "build": build,
            "minimumSystemVersion": "0",
            "url": "https://github.com/AnixOps/EasySSH/releases/download/v2/AnixOps-SSH.AppImage",
            "length": length,
            "sha256": digest,
            "signature": sign(key, &archive_message(PRODUCT, build, length, &digest)),
            "notes": "Fixes."
        })
    }

    /// An envelope as the kit's `SignFeed` writes it.
    pub fn feed(
        key: &SigningKey,
        product: &str,
        sequence: i64,
        items: Vec<serde_json::Value>,
    ) -> Vec<u8> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "product": product,
            "channel": "stable",
            "sequence": sequence,
            "generatedAt": "2027-01-15T08:00:00Z",
            "items": items,
        }))
        .unwrap();
        serde_json::to_vec_pretty(&serde_json::json!({
            "format": FORMAT,
            "payload": STANDARD.encode(&payload),
            "signature": sign(key, &feed_message(&payload)),
        }))
        .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn pair() -> (ed25519_dalek::SigningKey, Verifier) {
        let k = key(1);
        let v = Verifier::new(PRODUCT, &[&public(&k)]).unwrap();
        (k, v)
    }

    #[test]
    fn round_trip() {
        let (k, v) = pair();
        let archive = b"installer package";
        let data = feed(&k, PRODUCT, 5, vec![item(&k, 7, archive)]);
        let parsed = v.verify_feed(&data, 5).unwrap();
        assert_eq!(parsed.sequence, 5);
        assert_eq!(parsed.items[0].build, 7);
        v.verify_archive(&archive[..], &parsed.items[0]).unwrap();
    }

    #[test]
    fn feed_refusals() {
        let (k, v) = pair();
        let stranger = key(2);
        let good = feed(&k, PRODUCT, 5, vec![item(&k, 7, b"x")]);

        let env: serde_json::Value = serde_json::from_slice(&good).unwrap();
        let payload = STANDARD.decode(env["payload"].as_str().unwrap()).unwrap();
        let evil = String::from_utf8(payload)
            .unwrap()
            .replace("AnixOps/EasySSH", "AnixOps/EvilSSH~");
        let mut tampered = env.clone();
        tampered["payload"] = STANDARD.encode(evil).into();

        let mut insecure = item(&k, 7, b"x");
        insecure["url"] = "http://github.com/a".into();
        let mut bad_digest = item(&k, 7, b"x");
        bad_digest["sha256"] = "zz".into();

        let cases: Vec<(&str, Vec<u8>, i64, Error)> = vec![
            ("not json", b"<html>".to_vec(), 0, Error::Malformed),
            (
                "tampered payload",
                serde_json::to_vec(&tampered).unwrap(),
                0,
                Error::FeedSignature,
            ),
            (
                "another key",
                feed(&stranger, PRODUCT, 5, vec![]),
                0,
                Error::FeedSignature,
            ),
            ("replayed old feed", good.clone(), 6, Error::Stale),
            (
                "another product",
                feed(&k, "anixops-ssh-windows", 5, vec![]),
                0,
                Error::WrongProduct,
            ),
            (
                "plain http item",
                feed(&k, PRODUCT, 5, vec![insecure]),
                0,
                Error::InsecureUrl,
            ),
            (
                "malformed item",
                feed(&k, PRODUCT, 5, vec![bad_digest]),
                0,
                Error::Item,
            ),
        ];
        for (name, data, last, want) in cases {
            assert_eq!(v.verify_feed(&data, last), Err(want), "{name}");
        }
    }

    #[test]
    fn wrong_format_and_schema_are_refused() {
        let (k, v) = pair();
        let mut env: serde_json::Value =
            serde_json::from_slice(&feed(&k, PRODUCT, 1, vec![])).unwrap();
        env["format"] = "anixops-appcast-v2".into();
        assert_eq!(
            v.verify_feed(&serde_json::to_vec(&env).unwrap(), 0),
            Err(Error::Malformed)
        );
        let payload = serde_json::to_vec(&serde_json::json!({
            "schema": 2, "product": PRODUCT, "sequence": 1,
            "generatedAt": "2027-01-15T08:00:00Z", "items": []
        }))
        .unwrap();
        let signed = serde_json::to_vec(&serde_json::json!({
            "format": FORMAT,
            "payload": STANDARD.encode(&payload),
            "signature": sign(&k, &feed_message(&payload)),
        }))
        .unwrap();
        assert_eq!(v.verify_feed(&signed, 0), Err(Error::Malformed));
    }

    #[test]
    fn archive_refusals() {
        let (k, v) = pair();
        let stranger = key(2);
        let archive = b"installer package";
        let parse = |it: serde_json::Value| serde_json::from_value::<Item>(it).unwrap();
        let it = parse(item(&k, 7, archive));

        assert_eq!(
            v.verify_archive(&b"installer pack"[..], &it),
            Err(Error::Archive)
        );
        assert_eq!(
            v.verify_archive(&b"installer packagE"[..], &it),
            Err(Error::Archive)
        );
        let mut moved = it.clone();
        moved.build = 8; // a signed archive of build 7 presented as build 8
        assert_eq!(
            v.verify_archive(&archive[..], &moved),
            Err(Error::ArchiveSigning)
        );
        let foreign = parse(item(&stranger, 7, archive));
        assert_eq!(
            v.verify_archive(&archive[..], &foreign),
            Err(Error::ArchiveSigning)
        );
        // A feed signature must not pass as an archive signature.
        let mut swapped = it.clone();
        swapped.signature = sign(&k, &feed_message(b"x"));
        assert_eq!(
            v.verify_archive(&archive[..], &swapped),
            Err(Error::ArchiveSigning)
        );
    }

    #[test]
    fn newest_picks_highest_applicable_build() {
        let k = key(1);
        let parse = |it: serde_json::Value| serde_json::from_value::<Item>(it).unwrap();
        let (a, b, mut c) = (
            parse(item(&k, 5, b"a")),
            parse(item(&k, 7, b"b")),
            parse(item(&k, 9, b"c")),
        );
        c.minimum_system_version = "15.1".into();
        let feed = Feed {
            schema: 1,
            product: PRODUCT.into(),
            channel: String::new(),
            sequence: 1,
            generated_at: Utc::now(),
            items: vec![a, c, b],
        };
        assert_eq!(newest(&feed, 4, "14.6.1").map(|i| i.build), Some(7));
        assert_eq!(newest(&feed, 4, "15.1").map(|i| i.build), Some(9));
        assert_eq!(newest(&feed, 9, "15.1"), None, "no downgrade or same build");
        assert_eq!(newest(&feed, 4, "garbage"), None);
    }

    #[test]
    fn system_versions() {
        assert_eq!(parse_system_version("14"), Some([14, 0, 0]));
        assert_eq!(parse_system_version("14.2.1"), Some([14, 2, 1]));
        for bad in ["", "1.2.3.4", "a", "1..2", "-1", "+1", "1.2."] {
            assert_eq!(parse_system_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn builds_follow_the_mobile_version_code() {
        assert_eq!(build_from_version("2.1.0"), Some(2_001_000));
        assert_eq!(build_from_version("2.10.3-rc1"), Some(2_010_003));
        for bad in ["2.1", "2.1.0.1", "x.y.z", "", "2.1000.0"] {
            assert_eq!(build_from_version(bad), None, "{bad}");
        }
    }

    #[test]
    fn https_only() {
        assert!(is_https("https://github.com/a/b"));
        assert!(is_https("HTTPS://github.com"));
        for bad in [
            "http://github.com",
            "https://",
            "https:///x",
            "ftp://github.com",
            "https://user@github.com/",
            "https://github.com/a b",
            "",
        ] {
            assert!(!is_https(bad), "{bad}");
        }
    }

    #[test]
    fn keys_and_ids() {
        assert_eq!(Verifier::new(PRODUCT, &[]).unwrap_err(), Error::Key);
        assert_eq!(
            Verifier::new(PRODUCT, &["not base64!"]).unwrap_err(),
            Error::Key
        );
        assert_eq!(Verifier::new(PRODUCT, &["AAAA"]).unwrap_err(), Error::Key);
        // Two keys: either verifies (key rollover).
        let (old, new) = (key(1), key(2));
        let v = Verifier::new(PRODUCT, &[&public(&old), &public(&new)]).unwrap();
        assert!(v.verify_feed(&feed(&new, PRODUCT, 1, vec![]), 0).is_ok());
        assert_eq!(key_id(&old.verifying_key()).len(), 16);
    }
}
