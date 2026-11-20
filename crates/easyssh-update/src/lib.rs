//! Update check for AnixOps SSH: check and notify, never install.
//!
//! The program fetches the signed `anixops-appcast-v1` feed of its platform
//! from the GitHub release ([`feed_url`]), verifies it against the public
//! keys compiled in from `EASYSSH_UPDATE_PUBLIC_KEYS` ([`feed`]) and says
//! whether a newer build exists. The user opens the release in the browser.
//! No key compiled in means everything here is inert ([`configured`]).
//!
//! The format is the one every AnixOps program reads; its source is
//! `anix-kit/update`. Every check fails closed.

pub mod feed;
pub mod fetch;

pub use feed::{build_from_version, Error, Feed, Item, Verifier};
pub use fetch::{Fetch, Http};

use serde::Serialize;

/// The trusted public keys compiled in (comma separated base64), if any.
pub const BUILD_KEYS: Option<&str> = option_env!("EASYSSH_UPDATE_PUBLIC_KEYS");

/// The feed product of the platform this build runs on; `None` on phones,
/// where the stores update the app.
pub fn product() -> Option<&'static str> {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        None
    } else if cfg!(target_os = "windows") {
        Some("anixops-ssh-windows")
    } else if cfg!(target_os = "macos") {
        Some("anixops-ssh-macos")
    } else if cfg!(target_os = "linux") {
        Some("anixops-ssh-linux")
    } else {
        None
    }
}

/// Where the latest release serves the feed of `product`.
pub fn feed_url(product: &str) -> String {
    format!("https://github.com/AnixOps/EasySSH/releases/latest/download/{product}.appcast.json")
}

/// The keys from a comma separated list. A key that does not parse is
/// dropped, never trusted.
pub fn parse_keys(list: &str) -> Vec<&str> {
    list.split(',')
        .map(str::trim)
        .filter(|key| feed::parse_public_key(key).is_ok())
        .collect()
}

/// A verifier for this platform with the compiled-in keys.
pub fn build_verifier() -> Option<Verifier> {
    verifier(product()?, BUILD_KEYS?)
}

fn verifier(product: &str, keys: &str) -> Option<Verifier> {
    Verifier::new(product, &parse_keys(keys)).ok()
}

/// This build can check for updates: a platform with a feed and a key.
pub fn configured() -> bool {
    build_verifier().is_some()
}

/// What the page shows about a newer release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Update {
    pub version: String,
    pub build: i64,
    /// The release asset; opened in the browser, never downloaded here.
    pub url: String,
    pub notes: String,
    pub published_at: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Checked {
    /// The sequence of the feed that verified; the caller persists the highest.
    pub sequence: i64,
    pub update: Option<Update>,
}

/// Fetches, verifies and compares. `last_sequence` is the highest sequence
/// accepted before; `system_version` is what the item's minimum is compared to.
pub fn check(
    fetch: &dyn Fetch,
    verifier: &Verifier,
    url: &str,
    current_build: i64,
    system_version: &str,
    last_sequence: i64,
) -> Result<Checked, String> {
    let body = fetch.get(url)?;
    let feed = verifier
        .verify_feed(&body, last_sequence)
        .map_err(|e| e.to_string())?;
    let update = feed::newest(&feed, current_build, system_version).map(|item| Update {
        version: item.version.clone(),
        build: item.build,
        url: item.url.clone(),
        notes: item.notes.clone(),
        published_at: item.published_at.map(|t| t.to_rfc3339()),
    });
    Ok(Checked {
        sequence: feed.sequence,
        update,
    })
}

/// The OS version items are compared to. Items for Windows and Linux carry
/// minimum "0"; macOS items carry the app's minimum.
pub fn system_version() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (!text.is_empty()).then_some(text)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some("0".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::feed::testing::*;
    use super::*;

    struct Fake(Result<Vec<u8>, String>);
    impl Fetch for Fake {
        fn get(&self, _: &str) -> Result<Vec<u8>, String> {
            self.0.clone()
        }
    }

    #[test]
    fn feed_urls_name_the_platform() {
        assert_eq!(
            feed_url("anixops-ssh-linux"),
            "https://github.com/AnixOps/EasySSH/releases/latest/download/anixops-ssh-linux.appcast.json"
        );
        assert!(fetch::allowed_host(&feed_url("anixops-ssh-macos")).is_some());
    }

    #[test]
    fn no_valid_key_means_inert() {
        assert!(verifier(PRODUCT, "").is_none());
        assert!(verifier(PRODUCT, "nonsense, ,AAAA").is_none());
        let k = public(&key(1));
        let v = verifier(PRODUCT, &format!("nonsense, {k} ,")).unwrap();
        assert!(v.verify_feed(&feed(&key(1), PRODUCT, 1, vec![]), 0).is_ok());
    }

    #[test]
    fn a_newer_verified_item_is_reported() {
        let k = key(1);
        let v = verifier(PRODUCT, &public(&k)).unwrap();
        let data = feed(&k, PRODUCT, 9, vec![item(&k, 2_002_000, b"new")]);
        let got = check(&Fake(Ok(data.clone())), &v, "u", 2_001_000, "0", 3).unwrap();
        assert_eq!(got.sequence, 9);
        let update = got.update.unwrap();
        assert_eq!(update.build, 2_002_000);
        assert_eq!(update.notes, "Fixes.");
        // Already current, or newer: nothing to say.
        assert_eq!(
            check(&Fake(Ok(data.clone())), &v, "u", 2_002_000, "0", 3)
                .unwrap()
                .update,
            None
        );
        // A replayed older feed is an error, not "no update".
        assert!(check(&Fake(Ok(data)), &v, "u", 2_001_000, "0", 10).is_err());
    }

    #[test]
    fn every_failure_is_an_error() {
        let k = key(1);
        let v = verifier(PRODUCT, &public(&k)).unwrap();
        assert!(check(&Fake(Err("offline".into())), &v, "u", 1, "0", 0).is_err());
        assert!(check(&Fake(Ok(b"{}".to_vec())), &v, "u", 1, "0", 0).is_err());
        let other = feed(&key(2), PRODUCT, 1, vec![item(&key(2), 5, b"x")]);
        assert!(check(&Fake(Ok(other)), &v, "u", 1, "0", 0).is_err());
    }
}
