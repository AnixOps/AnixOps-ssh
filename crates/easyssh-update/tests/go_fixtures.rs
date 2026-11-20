//! Cross-checks against the kit's own Go implementation: a feed, item and
//! archive made by `anixsign keygen` and `anixupdate item|feed` (anix-kit
//! commit 73de268, see tests/fixtures/README), and the feed from the kit's
//! `TestForeignFeed` (RFC 8032 section 7.1 test 1 key).

use easyssh_update::feed::{newest, Error, Verifier};

const GO_KEY: &str = include_str!("fixtures/go-public-key.txt");
const GO_FEED: &[u8] = include_bytes!("fixtures/go-feed.json");
const GO_ARCHIVE: &[u8] = include_bytes!("fixtures/go-archive.bin");
const KIT_KEY: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
const KIT_FEED: &[u8] = include_bytes!("fixtures/kit-foreign-feed.json");

#[test]
fn a_feed_and_archive_made_by_the_go_tools_verify() {
    let v = Verifier::new("anixops-ssh-linux", &[GO_KEY]).unwrap();
    let feed = v.verify_feed(GO_FEED, 0).unwrap();
    assert_eq!(feed.sequence, 1_800_000_000);
    assert_eq!(feed.channel, "stable");
    let item = &feed.items[0];
    assert_eq!((item.version.as_str(), item.build), ("2.2.0", 2_002_000));
    assert_eq!(item.notes, "Cross-check fixture.");
    assert!(item.published_at.is_some());
    v.verify_archive(GO_ARCHIVE, item).unwrap();
    // Not the archive the item names.
    assert_eq!(
        v.verify_archive(&GO_ARCHIVE[1..], item),
        Err(Error::Archive)
    );
    assert_eq!(
        newest(&feed, 2_001_000, "0").map(|i| i.build),
        Some(2_002_000)
    );
    assert!(newest(&feed, 2_002_000, "0").is_none());
    // The same bytes for another product, or after a later feed: refused.
    let other = Verifier::new("anixops-ssh-windows", &[GO_KEY]).unwrap();
    assert_eq!(other.verify_feed(GO_FEED, 0), Err(Error::WrongProduct));
    assert_eq!(v.verify_feed(GO_FEED, 1_800_000_001), Err(Error::Stale));
}

#[test]
fn the_kit_foreign_feed_verifies() {
    let v = Verifier::new("anixops-net-macos", &[KIT_KEY]).unwrap();
    let feed = v.verify_feed(KIT_FEED, 3).unwrap();
    assert_eq!(feed.items[0].version, "0.3.1");
    assert_eq!(feed.generated_at.to_rfc3339(), "2026-11-12T00:00:00+00:00");
    v.verify_archive(&b"AnixOps Net test archive\n"[..], &feed.items[0])
        .unwrap();
}
