//! A crash report: what is known, already scrubbed.

use crate::scrub::{scrub, scrub_truncated};
use serde::{Deserialize, Serialize};

pub const MESSAGE_MAX: usize = 500;
pub const FRAME_MAX: usize = 160;
pub const FRAMES_MAX: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Report {
    /// 32 hexadecimal digits, as Sentry wants it.
    pub id: String,
    /// Unix seconds.
    pub timestamp: i64,
    pub version: String,
    pub os: String,
    pub arch: String,
    /// `app`, `daemon` or `ui`.
    pub component: String,
    /// `panic`, `error` or `unhandledrejection`.
    pub kind: String,
    pub message: String,
    /// Symbol names only, innermost first.
    pub frames: Vec<String>,
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl Report {
    /// Builds a report; message and frames are scrubbed and cut here, so there
    /// is no way to construct one that was not.
    pub fn new(
        component: &str,
        kind: &str,
        version: &str,
        message: &str,
        frames: impl IntoIterator<Item = String>,
    ) -> Report {
        Report {
            id: uuid::Uuid::new_v4().simple().to_string(),
            timestamp: now(),
            version: version.to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            component: component.to_owned(),
            kind: kind.to_owned(),
            message: message.to_owned(),
            frames: frames.into_iter().collect(),
        }
        .scrubbed()
    }

    /// Scrubs and bounds every free-text field again. Applied to whatever is
    /// read back from the spool.
    pub fn scrubbed(mut self) -> Report {
        self.message = scrub_truncated(&self.message, MESSAGE_MAX);
        self.frames = self
            .frames
            .iter()
            .map(|f| scrub_truncated(&clean_symbol(f), FRAME_MAX))
            .filter(|f| !f.is_empty())
            .take(FRAMES_MAX)
            .collect();
        for field in [&mut self.version, &mut self.component, &mut self.kind] {
            *field = scrub_truncated(field, 64);
        }
        self.os = scrub(&self.os);
        self.arch = scrub(&self.arch);
        self
    }
}

/// Removes the compiler's hash and LLVM suffixes from a symbol.
pub fn clean_symbol(symbol: &str) -> String {
    let mut s = symbol.trim();
    if let Some(i) = s.find(".llvm.") {
        s = &s[..i];
    }
    if let Some(i) = s.rfind("::h") {
        let hash = &s[i + 3..];
        if hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            s = &s[..i];
        }
    }
    s.to_owned()
}

/// Symbol names from the text of a `std::backtrace::Backtrace`: only the
/// `  12: name` lines; the `at file:line` lines (home directories) are dropped.
pub fn parse_backtrace(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim_start();
            let (index, rest) = line.split_once(':')?;
            if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let name = clean_symbol(rest);
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// Function names from a JavaScript stack (`at f (url:1:2)`, `f@url:1:2`).
/// URLs, file names and positions are never kept.
pub fn parse_js_stack(stack: &str) -> Vec<String> {
    stack
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("at ") {
                let name = rest.split(" (").next().unwrap_or("");
                // `at url:1:2` has no function name.
                return Some(if rest.contains(" (") && !name.contains("://") {
                    name.to_owned()
                } else {
                    "<anonymous>".to_owned()
                });
            }
            let (name, _) = line.split_once('@')?;
            Some(if name.is_empty() {
                "<anonymous>".to_owned()
            } else {
                name.to_owned()
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn construction_scrubs() {
        let r = Report::new(
            "app",
            "panic",
            "1.0.2",
            "failed for root@10.0.0.1 in /home/alice/x",
            [
                "a::b::h0123456789abcdef".to_owned(),
                "at /home/bob/y".to_owned(),
            ],
        );
        assert_eq!(r.message, "failed for <email> in <path>");
        assert_eq!(r.frames[0], "a::b");
        assert_eq!(r.frames[1], "at <path>");
        assert_eq!(r.id.len(), 32);
    }

    #[test]
    fn bounds() {
        let r = Report::new(
            "app",
            "panic",
            "1",
            &"x y ".repeat(1000),
            (0..200).map(|i| format!("frame_{i}")),
        );
        assert!(r.message.chars().count() <= MESSAGE_MAX);
        assert_eq!(r.frames.len(), FRAMES_MAX);
    }

    #[test]
    fn rescrubbing_a_stale_report() {
        let mut r = Report::new("app", "panic", "1", "ok", []);
        r.message = "leak db.example.com".into();
        assert_eq!(r.scrubbed().message, "leak <host>");
    }

    #[test]
    fn backtrace_symbols_only() {
        let text = "   0: std::backtrace::Backtrace::force_capture\n             at /rustc/abc/library/std/src/backtrace.rs:1:2\n   1: anixops_ssh::main::h0123456789abcdef\n             at /home/alice/EasySSH/src-tauri/src/main.rs:5:1\n  10: core::ops::function::FnOnce::call_once{{vtable.shim}}\nstack backtrace:\n";
        assert_eq!(
            parse_backtrace(text),
            [
                "std::backtrace::Backtrace::force_capture",
                "anixops_ssh::main",
                "core::ops::function::FnOnce::call_once{{vtable.shim}}"
            ]
        );
    }

    #[test]
    fn llvm_suffix() {
        assert_eq!(clean_symbol("foo::bar.llvm.1234567890"), "foo::bar");
    }

    #[test]
    fn js_stacks_keep_function_names_only() {
        let chrome = "Error: x\n    at connect (tauri://localhost/assets/a.js:1:2)\n    at https://example.com/b.js:3:4";
        assert_eq!(parse_js_stack(chrome), ["connect", "<anonymous>"]);
        let webkit = "connect@tauri://localhost/a.js:1:2\n@https://example.com/b.js:3:4";
        assert_eq!(parse_js_stack(webkit), ["connect", "<anonymous>"]);
    }
}
