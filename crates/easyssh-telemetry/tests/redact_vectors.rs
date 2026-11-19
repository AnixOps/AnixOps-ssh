//! The shared AnixOps redaction vectors (anix-kit `redact/testdata/vectors.json`)
//! must come out of `scrub` exactly as the rule set says.

use easyssh_telemetry::scrub::scrub;

const VECTORS: &str = include_str!("data/anixops-redact-vectors.json");

#[test]
fn shared_vectors_pass_exactly() {
    let doc: serde_json::Value = serde_json::from_str(VECTORS).expect("vectors parse");
    let cases = doc["redact"].as_array().expect("redact array");
    assert!(!cases.is_empty());
    for case in cases {
        let input = case["in"].as_str().expect("in");
        let want = case["out"].as_str().expect("out");
        assert_eq!(scrub(input), want, "input: {input:?}");
    }
}
