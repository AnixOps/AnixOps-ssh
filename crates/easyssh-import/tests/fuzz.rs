//! Fuzz-ish robustness: no parser may panic, whatever the input.
//! Deterministic (fixed seed) so failures reproduce.

use easyssh_import::*;
use std::path::Path;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

type Parser = (&'static str, fn(&[u8]) -> ImportReport);

fn parsers() -> Vec<Parser> {
    vec![
        ("openssh", openssh::parse),
        ("putty", putty::parse),
        ("securecrt", securecrt::parse),
        ("mobaxterm", mobaxterm::parse),
        ("xshell", xshell::parse),
        ("finalshell", finalshell::parse),
        ("termius", termius::parse),
        ("csv", csv_format::parse),
        ("auto", import_auto),
    ]
}

fn detectors() -> Vec<fn(&[u8]) -> bool> {
    vec![
        openssh::detect,
        putty::detect,
        securecrt::detect,
        mobaxterm::detect,
        xshell::detect,
        finalshell::detect,
        termius::detect,
        csv_format::detect,
    ]
}

fn run_all(input: &[u8]) {
    for (_, parse) in parsers() {
        let report = parse(input);
        // Invariants that hold for any input.
        for host in &report.hosts {
            assert!(host.port != 0);
            assert!(!host.hostname.is_empty());
            assert!(!host.hostname.starts_with('-'));
        }
        let _ = report.to_entities();
    }
    for detect in detectors() {
        let _ = detect(input);
    }
    let _ = detect_format(input);
}

fn fixtures() -> Vec<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut out = Vec::new();
    for name in [
        "openssh/config",
        "openssh/config_bad",
        "putty.reg",
        "securecrt.xml",
        "mobaxterm.mxtsessions",
        "xshell.xsh",
        "finalshell.json",
        "termius.csv",
        "termius.json",
        "hosts.csv",
    ] {
        out.push(std::fs::read(dir.join(name)).unwrap());
    }
    out
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..400 {
        let len = rng.below(2048);
        let data: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
        run_all(&data);
    }
}

#[test]
fn structured_noise_never_panics() {
    let alphabet: &[&str] = &[
        "Host ",
        "HostName ",
        "Include ",
        "Match ",
        "\n",
        "=",
        "\"",
        "[",
        "]",
        "<key name=\"",
        "</key>",
        "<string name=\"Hostname\">",
        "</string>",
        "{",
        "}",
        ",",
        ":",
        "%",
        "#",
        "dword:",
        "\\",
        "PK\x03\x04",
        "\u{FEFF}",
        "label,hostname\n",
        "[CONNECTION]",
        "Sessions",
        "1",
        "99999",
        "\u{0}",
        "é",
        "-",
    ];
    let mut rng = Rng(42);
    for _ in 0..600 {
        let mut text = String::new();
        for _ in 0..rng.below(60) {
            text.push_str(alphabet[rng.below(alphabet.len())]);
        }
        run_all(text.as_bytes());
    }
}

#[test]
fn mutated_fixtures_never_panic() {
    let mut rng = Rng(7);
    for original in fixtures() {
        run_all(&original);
        // Every truncation point on a coarse grid.
        for cut in (0..original.len()).step_by(7) {
            run_all(&original[..cut]);
        }
        // Random byte flips, insertions and deletions.
        for _ in 0..60 {
            let mut data = original.clone();
            for _ in 0..1 + rng.below(8) {
                if data.is_empty() {
                    break;
                }
                let at = rng.below(data.len());
                match rng.below(3) {
                    0 => data[at] = rng.next() as u8,
                    1 => data.insert(at, rng.next() as u8),
                    _ => {
                        data.remove(at);
                    }
                }
            }
            run_all(&data);
        }
    }
}

#[test]
fn hostile_archives_do_not_panic() {
    // Truncated headers, bad central directories and zip-looking garbage.
    let mut rng = Rng(1234);
    for _ in 0..200 {
        let mut data = b"PK\x03\x04".to_vec();
        data.extend((0..rng.below(300)).map(|_| rng.next() as u8));
        run_all(&data);
    }
    run_all(b"PK\x05\x06\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00");
}

#[test]
fn deeply_nested_input_is_bounded() {
    let xml = "<key name=\"a\">".repeat(5000);
    run_all(xml.as_bytes());
    let json = "[".repeat(5000);
    run_all(json.as_bytes());
    let include = "Include x\n".repeat(5000);
    run_all(include.as_bytes());
    let hosts = "Host h\n".repeat(20000);
    run_all(hosts.as_bytes());
}
