//! Removes obvious secrets from terminal context before it is shown for
//! review and sent.
//!
//! This is deliberately not the telemetry scrubber: that one also removes
//! host names, addresses and paths, which the model needs to give a useful
//! answer, and the user is shown the result anyway. This one removes what
//! should never leave the machine whatever the question is: private keys,
//! access tokens, passwords given as `name=value` or as command-line flags,
//! and passwords inside URLs. Each removal leaves `[REDACTED:kind]` in the
//! text and is counted, so the review step can say what was taken out.
//!
//! It is a safety net for the obvious, not a guarantee: the user reviews the
//! exact text before sending.

use regex::{Captures, Regex};
use std::collections::BTreeMap;
use std::sync::LazyLock;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("scrubber pattern")
}

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----|\z)",
    )
});
static TOKEN_SHAPES: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        ("anthropic-key", re(r"\bsk-ant-[A-Za-z0-9_-]{10,}")),
        ("api-key", re(r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}")),
        (
            "github-token",
            re(r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})"),
        ),
        ("gitlab-token", re(r"\bglpat-[A-Za-z0-9_-]{16,}")),
        ("aws-key", re(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b")),
        ("google-key", re(r"\bAIza[0-9A-Za-z_-]{35}\b")),
        ("slack-token", re(r"\bxox[abprs]-[A-Za-z0-9-]{10,}")),
        (
            "jwt",
            re(r"\beyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}"),
        ),
    ]
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{12,}"));
static URL_PASSWORD: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+):([^\s/@]+)@"));
static FLAG: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?i)(--(?:password|passwd|pass|token|secret|api-?key|access-?key|auth-?token)(?:=|\s+))("[^"\n]*"|'[^'\n]*'|[^\s"']+)"#,
    )
});
static SSHPASS: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(\bsshpass\s+-p\s*)("[^"\n]*"|'[^'\n]*'|[^\s"']+)"#));
static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r#"(?i)\b([A-Za-z0-9_.-]*(?:password|passwd|pwd|secret|token|api[_-]?key|apikey|access[_-]?key|private[_-]?key|credential)[A-Za-z0-9_.-]*)(\s*[=:]\s*)("[^"\n]*"|'[^'\n]*'|[^\s"',;]+)"#,
    )
});

const MARK: &str = "[REDACTED";

/// What was removed: a short kind and how many times.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Removed {
    pub kind: String,
    pub count: usize,
}

/// `text` without secrets, and what was removed.
pub fn scrub(text: &str) -> (String, Vec<Removed>) {
    let mut found: BTreeMap<String, usize> = BTreeMap::new();
    let mut note = |kind: &str| *found.entry(kind.to_owned()).or_insert(0) += 1;

    let mut out = PRIVATE_KEY
        .replace_all(text, |_: &Captures| {
            note("private-key");
            "[REDACTED:private-key]"
        })
        .into_owned();
    for (kind, pattern) in TOKEN_SHAPES.iter() {
        out = pattern
            .replace_all(&out, |_: &Captures| {
                note(kind);
                format!("[REDACTED:{kind}]")
            })
            .into_owned();
    }
    out = BEARER
        .replace_all(&out, |c: &Captures| {
            note("auth-header");
            format!("{} [REDACTED:auth-header]", &c[1])
        })
        .into_owned();
    out = URL_PASSWORD
        .replace_all(&out, |c: &Captures| {
            note("url-password");
            format!("{}:[REDACTED:url-password]@", &c[1])
        })
        .into_owned();
    out = FLAG
        .replace_all(&out, |c: &Captures| {
            if c[2].starts_with(MARK) {
                return c[0].to_owned();
            }
            note("password-flag");
            format!("{}[REDACTED:password-flag]", &c[1])
        })
        .into_owned();
    out = SSHPASS
        .replace_all(&out, |c: &Captures| {
            if c[2].starts_with(MARK) {
                return c[0].to_owned();
            }
            note("password-flag");
            format!("{}[REDACTED:password-flag]", &c[1])
        })
        .into_owned();
    out = ASSIGNMENT
        .replace_all(&out, |c: &Captures| {
            let value = c[3].trim_matches(['"', '\'']);
            if value.starts_with(MARK) || value.is_empty() {
                return c[0].to_owned();
            }
            note("secret-value");
            format!("{}{}[REDACTED:secret-value]", &c[1], &c[2])
        })
        .into_owned();

    let removed = found
        .into_iter()
        .map(|(kind, count)| Removed { kind, count })
        .collect();
    (out, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(removed: &[Removed]) -> Vec<&str> {
        removed.iter().map(|r| r.kind.as_str()).collect()
    }

    #[test]
    fn private_keys_go_whole() {
        let text = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nafter";
        let (out, removed) = scrub(text);
        assert_eq!(out, "before\n[REDACTED:private-key]\nafter");
        assert_eq!(kinds(&removed), ["private-key"]);
        // A key cut off by the end of the selection goes too.
        let (out, _) = scrub("x -----BEGIN RSA PRIVATE KEY-----\nMIIE");
        assert_eq!(out, "x [REDACTED:private-key]");
        // A public key is not secret.
        let (out, removed) = scrub("-----BEGIN PUBLIC KEY-----\nabc\n-----END PUBLIC KEY-----");
        assert!(removed.is_empty(), "{out}");
    }

    #[test]
    fn token_shapes() {
        let cases = [
            ("sk-ant-api03-abcdefghij0123456789", "anthropic-key"),
            ("sk-proj-abcdefghij0123456789abcd", "api-key"),
            ("ghp_0123456789abcdefghijABCDEFGHIJ012345", "github-token"),
            ("AKIAIOSFODNN7EXAMPLE", "aws-key"),
            ("xoxb-1234567890-abcdefghij", "slack-token"),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
                "jwt",
            ),
            ("glpat-abcdefghij0123456789", "gitlab-token"),
        ];
        for (secret, kind) in cases {
            let (out, removed) = scrub(&format!("export X is {secret} ok"));
            assert!(!out.contains(secret), "{kind}: {out}");
            assert_eq!(kinds(&removed), [kind], "{out}");
        }
    }

    #[test]
    fn authorization_headers_and_url_passwords() {
        let (out, removed) = scrub(
            "curl -H 'Authorization: Bearer abcdef0123456789xyz' https://u:hunter2@example.com/x",
        );
        assert!(!out.contains("abcdef0123456789xyz"), "{out}");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("https://u:[REDACTED:url-password]@example.com/x"));
        assert_eq!(kinds(&removed), ["auth-header", "url-password"]);
    }

    #[test]
    fn assignments_and_flags() {
        let (out, removed) = scrub(
            "DB_PASSWORD=hunter2 API_KEY: \"abc 123\" mysql --password=hunter3 --token 'a b' sshpass -p secret ssh x",
        );
        for secret in ["hunter2", "abc 123", "hunter3", "a b", "secret ssh"] {
            assert!(!out.contains(secret), "{secret}: {out}");
        }
        assert!(out.contains("DB_PASSWORD=[REDACTED:secret-value]"), "{out}");
        assert!(out.contains("ssh x"), "{out}");
        assert_eq!(removed.iter().map(|r| r.count).sum::<usize>(), 5);
    }

    #[test]
    fn ordinary_terminal_text_is_left_alone() {
        let text = "user@web-1:~$ ls -la /var/log\ntotal 12\n-rw-r--r-- 1 root root 0 Jan 1 auth.log\nsystemctl status nginx.service\nEnter passphrase for key:\n";
        let (out, removed) = scrub(text);
        assert!(removed.is_empty(), "{removed:?}");
        assert_eq!(out, text);
    }

    #[test]
    fn scrubbing_twice_changes_nothing() {
        let (once, _) = scrub(
            "PASSWORD=x TOKEN=sk-ant-abcdefghijkl0123 ghp_0123456789abcdefghijABCDEFGHIJ012345",
        );
        let (twice, removed) = scrub(&once);
        assert_eq!(once, twice);
        assert!(removed.is_empty(), "{removed:?}");
    }
}
