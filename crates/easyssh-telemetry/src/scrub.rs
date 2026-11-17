//! The one scrubber. Everything that leaves the machine (panic messages,
//! frontend error messages, stack frames) goes through [`scrub`] first, and
//! reports read back from the spool are scrubbed again before they are shown or
//! sent, so a report written by an older build cannot leak what a newer
//! scrubber knows about.
//!
//! What it removes, each replaced by a fixed placeholder:
//!
//! | looks like | placeholder |
//! |---|---|
//! | `scheme://...` URL | `<url>` |
//! | `name@domain.tld` | `<email>` |
//! | `user@host` (no dot) | `<account>` |
//! | `C:\Users\name\...`, any drive path, UNC share | `<path>` |
//! | `/home/x/..`, `/Users/x/..`, `/root/..`, `~/..`, any absolute Unix path with two or more segments | `<path>` |
//! | MAC address | `<mac>` |
//! | 16+ hex digits, 20+ base64-like characters with a digit | `<token>` |
//! | IPv4 and IPv6 addresses | `<ip>` |
//! | dotted host names (`a.b.tld`) | `<host>` |
//!
//! Known limit: a single-label name with no dot (`prod-db`) cannot be told
//! from an ordinary word. Nothing in a message is trusted because of that:
//! reports carry only the panic message and symbol names, never arguments,
//! environment, command lines or terminal text.

use regex::{Captures, Regex};
use std::net::Ipv6Addr;
use std::sync::LazyLock;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("scrubber pattern")
}

static URL: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)\b[a-z][a-z0-9+.\-]*://[^\s"'<>)\]]+"#));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r"[A-Za-z0-9._%+~-]+@[A-Za-z0-9_-]+(?:\.[A-Za-z0-9_-]+)*"));
static WIN_USER: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)[A-Za-z]:[\\/]+Users[\\/]+[^\\/\r\n"'<>|]+(?:[\\/][^\s"'<>|]*)?"#));
static WIN_PATH: LazyLock<Regex> = LazyLock::new(|| re(r#"[A-Za-z]:[\\/][^\s"'<>|*?]*"#));
static UNC: LazyLock<Regex> = LazyLock::new(|| re(r#"\\\\[A-Za-z0-9._$-]+\\[^\s"'<>|]*"#));
static UNIX_HOME: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(^|[^A-Za-z0-9_])(/(?:home|Users)/[^/\r\n"'<>]+(?:/[^\s"'<>]*)?|/root(?:/[^\s"'<>]*)?|~[A-Za-z0-9._-]*/[^\s"'<>]*)"#,
    )
});
static UNIX_PATH: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(^|[^A-Za-z0-9_.)\]>/:])(/[A-Za-z0-9._@+%~=-]+(?:/[A-Za-z0-9._@+%~=-]*)+)")
});
static MAC: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:[0-9A-Fa-f]{2}[:-]){5}[0-9A-Fa-f]{2}\b"));
static HEX: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:0[xX])?[0-9A-Fa-f]{16,}\b"));
static BASE64: LazyLock<Regex> = LazyLock::new(|| re(r"[A-Za-z0-9+/_-]{20,}={0,2}"));
static IPV4: LazyLock<Regex> = LazyLock::new(|| re(r"\b(?:\d{1,3}\.){3}\d{1,3}\b"));
static HOST: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\b(?:[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?\.)+(?:[A-Za-z]{2,63}|xn--[A-Za-z0-9-]{1,59})\b",
    )
});

/// Final labels that are file extensions, not top-level domains anyone puts a
/// host under: `main.rs`, `index.js`. Kept short on purpose.
const FILE_EXTENSIONS: &[&str] = &[
    "rs", "toml", "json", "lock", "md", "js", "ts", "tsx", "jsx", "mjs", "css", "html", "log",
    "so", "dll", "exe", "dylib", "yml", "yaml", "txt", "sql", "plist",
];

/// Removes everything in the table of the module documentation. Idempotent:
/// `scrub(&scrub(x)) == scrub(x)`.
pub fn scrub(input: &str) -> String {
    let s = URL.replace_all(input, "<url>");
    let s = EMAIL.replace_all(&s, |c: &Captures| {
        if c[0].rsplit('@').next().is_some_and(|d| d.contains('.')) {
            "<email>"
        } else {
            "<account>"
        }
    });
    let s = WIN_USER.replace_all(&s, "<path>");
    let s = WIN_PATH.replace_all(&s, "<path>");
    let s = UNC.replace_all(&s, "<path>");
    let s = UNIX_HOME.replace_all(&s, "${1}<path>");
    let s = UNIX_PATH.replace_all(&s, "${1}<path>");
    let s = MAC.replace_all(&s, "<mac>");
    let s = HEX.replace_all(&s, "<token>");
    let s = BASE64.replace_all(&s, |c: &Captures| {
        let t = &c[0];
        if t.bytes().any(|b| b.is_ascii_digit()) && t.bytes().any(|b| b.is_ascii_alphabetic()) {
            "<token>".to_owned()
        } else {
            t.to_owned()
        }
    });
    let s = scrub_ipv6(&s);
    let s = IPV4.replace_all(&s, "<ip>");
    let s = HOST.replace_all(&s, |c: &Captures| {
        let tld = c[0].rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        if FILE_EXTENSIONS.contains(&tld.as_str()) {
            c[0].to_owned()
        } else {
            "<host>".to_owned()
        }
    });
    s.into_owned()
}

/// [`scrub`], then cut to at most `max` characters (on a character boundary).
pub fn scrub_truncated(input: &str, max: usize) -> String {
    let s = scrub(input);
    match s.char_indices().nth(max) {
        Some((end, _)) => s[..end].to_owned(),
        None => s,
    }
}

/// IPv6 addresses are found as maximal runs of `[0-9A-Fa-f:.]` that are not
/// glued to a letter or digit (so `std::io` is not `d::`), and kept only when
/// the standard parser accepts them.
fn scrub_ipv6(s: &str) -> String {
    let is_run = |c: char| c.is_ascii_hexdigit() || c == ':' || c == '.';
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut chars = s.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if !is_run(c) {
            chars.next();
            continue;
        }
        let mut end = start;
        while let Some(&(i, c)) = chars.peek() {
            if !is_run(c) {
                break;
            }
            end = i + c.len_utf8();
            chars.next();
        }
        let glued = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        if glued(s[..start].chars().next_back()) || glued(s[end..].chars().next()) {
            continue;
        }
        let mut run = &s[start..end];
        if run.matches(':').count() < 2 || !run.bytes().any(|b| b.is_ascii_digit()) {
            continue;
        }
        loop {
            if run.parse::<Ipv6Addr>().is_ok() {
                break;
            }
            match run.strip_suffix(['.', ':']) {
                Some(shorter) => run = shorter,
                None => {
                    run = "";
                    break;
                }
            }
        }
        if run.is_empty() {
            continue;
        }
        out.push_str(&s[last..start]);
        out.push_str("<ip>");
        last = start + run.len();
    }
    out.push_str(&s[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inputs that must come out with none of the listed fragments.
    const LEAKS: &[(&str, &[&str])] = &[
        (
            "connect to prod-db.example.com failed",
            &["prod-db", "example.com"],
        ),
        ("dns: lookup db1.internal.corp.lan", &["db1", "corp", "lan"]),
        ("host xn--80ak6aa92e.xn--p1ai", &["xn--"]),
        ("bad host: Server.EXAMPLE.ORG.", &["EXAMPLE", "Server"]),
        ("connect 10.0.0.5:22 refused", &["10.0.0.5"]),
        ("peer 192.168.1.20", &["192.168"]),
        ("peer 8.8.8.8, 1.1.1.1", &["8.8.8.8", "1.1.1.1"]),
        ("to 999.1.1.1", &["999.1"]),
        ("bind [::1]:2222 failed", &["::1"]),
        ("addr 2001:db8::ff00:42:8329", &["2001", "8329"]),
        ("addr fe80::1%eth0 lost", &["fe80"]),
        ("mapped ::ffff:192.168.0.1 gone", &["192.168", "ffff"]),
        (
            "full 2001:0db8:0000:0000:0000:ff00:0042:8329.",
            &["2001", "0db8"],
        ),
        ("mail admin@example.com bounced", &["admin", "example.com"]),
        (
            "user+tag.name@sub.mail.example.co.uk",
            &["user", "sub.", "co.uk"],
        ),
        ("ssh root@prod-db failed", &["root@", "prod-db"]),
        ("ssh deploy@10.1.2.3 failed", &["deploy", "10.1.2.3"]),
        (
            "git@github.com:org/repo.git",
            &["git@", "github", "org/repo"],
        ),
        ("open /home/alice/.ssh/id_ed25519", &["alice", "id_ed25519"]),
        ("open /Users/Bob Smith/Library/x.db", &["Bob", "Smith"]),
        ("open /root/.config/EasySSH", &["root", "EasySSH"]),
        ("open ~/.ssh/config", &[".ssh", "config"]),
        ("open ~carol/.ssh/config", &["carol"]),
        (
            r"open C:\Users\Dave\AppData\Roaming\x.json",
            &["Dave", "AppData"],
        ),
        (
            r"open c:\users\Dave Jones\Documents\a b.txt",
            &["Dave", "Jones", "Documents"],
        ),
        ("open C:/Users/erin/work/key.pem", &["erin", "key.pem"]),
        (r"open D:\keys\prod.ppk", &["prod.ppk", "keys"]),
        (
            r"share \\fileserver\team\keys\a.pem",
            &["fileserver", "team"],
        ),
        (r"verbatim \\?\C:\Users\frank\x", &["frank"]),
        (
            "read /etc/ssh/ssh_config.d/50-corp.conf",
            &["corp", "ssh_config"],
        ),
        ("read /var/lib/secret-vault/data", &["secret-vault"]),
        ("tmp (/tmp/ssh-XXaa/agent.1234)", &["agent.1234"]),
        (
            "key 0123456789abcdef0123456789abcdef",
            &["0123456789abcdef"],
        ),
        ("fp 0xDEADBEEFDEADBEEF", &["DEADBEEF"]),
        (
            "sha256:9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            &["9f86d081"],
        ),
        ("tok QWxhZGRpbjpvcGVuIHNlc2FtZQ12345==", &["QWxhZGRp"]),
        (
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abc",
            &["eyJhbGci", "eyJzdWIi"],
        ),
        ("mac aa:bb:cc:dd:ee:ff up", &["aa:bb"]),
        (
            "fetch https://user:pw@git.example.com/a/b?token=1",
            &["user", "pw", "example"],
        ),
        (
            "tauri://localhost/assets/index-abc123.js:1:2345",
            &["localhost", "assets"],
        ),
    ];

    #[test]
    fn removes_what_it_must() {
        for (input, banned) in LEAKS {
            let out = scrub(input);
            for fragment in *banned {
                assert!(
                    !out.contains(fragment),
                    "{input:?} -> {out:?} still contains {fragment:?}"
                );
            }
        }
    }

    #[test]
    fn exact_outputs() {
        let cases = [
            ("ssh root@prod-db failed", "ssh <account> failed"),
            ("mail a.b@example.com", "mail <email>"),
            ("peer 10.0.0.5:22", "peer <ip>:22"),
            ("bind [::1]:22", "bind [<ip>]:22"),
            ("x db1.example.com y", "x <host> y"),
            ("open /home/alice/a/b.txt now", "open <path> now"),
            ("open ~/x/y now", "open <path> now"),
            (r"C:\Users\Dave\x.txt", "<path>"),
            ("see https://example.com/x for more", "see <url> for more"),
            ("mac aa:bb:cc:dd:ee:ff", "mac <mac>"),
            ("0123456789abcdef", "<token>"),
            ("a /etc/hosts.", "a <path>"),
        ];
        for (input, want) in cases {
            assert_eq!(scrub(input), want, "{input:?}");
        }
    }

    #[test]
    fn leaves_ordinary_text_alone() {
        for text in [
            "called `Option::unwrap()` on a `None` value",
            "index out of bounds: the len is 3 but the index is 7",
            "attempt to subtract with overflow",
            "std::io::Error",
            "alloc::vec::Vec<T>::push",
            "core::panicking::panic_fmt",
            "anixops_ssh::sessions::open_in_process::{{closure}}",
            "assertion failed: left == right",
            "failed at 12:34:56 after 3 tries",
            "version 1.0.2 and 2.0",
            "and/or either/or 50/50",
            "src/main.rs and Cargo.toml and index.tsx",
            "TypeError: undefined is not a function",
            "state = { code: 2, kind: NotFound }",
            "a::b and d::ef",
            "snake_case_identifier_that_is_quite_long_indeed",
            "SomeVeryLongCamelCaseIdentifierWithoutDigits",
            "e.g. this, i.e. that",
            "",
        ] {
            assert_eq!(scrub(text), text, "{text:?}");
        }
    }

    #[test]
    fn is_idempotent() {
        for (input, _) in LEAKS {
            let once = scrub(input);
            assert_eq!(scrub(&once), once, "{input:?}");
        }
    }

    #[test]
    fn placeholders_do_not_collide_with_patterns() {
        let all = "<url> <email> <account> <path> <mac> <token> <ip> <host>";
        assert_eq!(scrub(all), all);
    }

    #[test]
    fn handles_multiple_secrets_in_one_message() {
        let out =
            scrub("user@h.example.org from 10.0.0.1 opened /home/u/.ssh/id_rsa via [fe80::1]");
        assert_eq!(out, "<email> from <ip> opened <path> via [<ip>]");
    }

    #[test]
    fn handles_multiline_and_unicode() {
        let out = scrub("错误: 无法连接 db.example.com\n at /home/张三/x\n done");
        assert!(!out.contains("example"));
        assert!(!out.contains("张三"));
        assert!(out.contains("错误"));
        assert!(out.contains('\n'));
    }

    #[test]
    fn stack_frames() {
        let frames = [
            "at connect (tauri://localhost/assets/index-9f3a1c.js:1:2345)",
            "at https://example.com/app.js:10:5",
            "anixops_ssh::hosts::load_hosts::h0123456789abcdef",
        ];
        for f in frames {
            let out = scrub(f);
            assert!(
                !out.contains("example") && !out.contains("localhost"),
                "{out}"
            );
        }
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        assert_eq!(scrub_truncated("abcdef", 3), "abc");
        assert_eq!(scrub_truncated("错误错误", 3), "错误错");
        assert_eq!(scrub_truncated("ab", 10), "ab");
        // Scrubbed first, so a long path cannot survive by being cut mid-way.
        assert_eq!(scrub_truncated("/home/alice/secret/file", 100), "<path>");
    }

    #[test]
    fn ipv6_runs_glued_to_words_are_not_addresses() {
        assert_eq!(scrub("std::io::Error"), "std::io::Error");
        assert_eq!(scrub("beef::dead"), "beef::dead"); // no digit: ordinary words
        assert_eq!(scrub("v::1x"), "v::1x");
    }
}
