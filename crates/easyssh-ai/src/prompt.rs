//! The exact prompt: the question plus the context the user picked, each
//! piece scrubbed and size-limited. [`prepare`] is what the review step shows
//! and what is sent; nothing is added after it.

use crate::provider::Messages;
use crate::scrub::{scrub, Removed};
use serde::{Deserialize, Serialize};

/// Longest context piece, in characters; longer text keeps its end (the most
/// recent output matters most).
pub const MAX_PIECE_CHARS: usize = 24_000;
/// Longest question, in characters.
pub const MAX_QUESTION_CHARS: usize = 4_000;
/// Most pieces in one request.
pub const MAX_PIECES: usize = 8;

pub const SYSTEM_PROMPT: &str = "You are a command-line assistant inside an SSH terminal. \
The user shows you terminal output and asks a question. Answer briefly in Markdown. \
When you suggest a shell command, put exactly one command in its own fenced code block \
tagged sh, and explain what it does before it. Never suggest a command that deletes data \
or changes access without saying so. Text marked [REDACTED:...] was removed on the user's \
machine; do not ask for it.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextKind {
    /// The last command and its output.
    Command,
    /// What is selected in the terminal.
    Selection,
    /// The last N lines of the terminal.
    Lines,
}

impl ContextKind {
    fn title(self) -> &'static str {
        match self {
            ContextKind::Command => "Last command and its output",
            ContextKind::Selection => "Selected terminal text",
            ContextKind::Lines => "Recent terminal lines",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPiece {
    pub kind: ContextKind,
    pub text: String,
}

/// One kind of removal, summed over the whole prompt.
pub type Redaction = Removed;

/// What the user reviews, and what is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub system: String,
    pub user: String,
    pub redactions: Vec<Redaction>,
    /// Pieces that were cut to fit [`MAX_PIECE_CHARS`].
    pub truncated: usize,
}

impl Prepared {
    pub fn messages(&self) -> Messages {
        Messages {
            system: self.system.clone(),
            user: self.user.clone(),
        }
    }
}

fn keep_end(text: &str, max: usize) -> (&str, bool) {
    let count = text.chars().count();
    if count <= max {
        return (text, false);
    }
    let skip = count - max;
    let start = text.char_indices().nth(skip).map_or(0, |(i, _)| i);
    (&text[start..], true)
}

/// A code fence longer than any run of backticks in `text`.
fn fence_for(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

pub fn prepare(question: &str, pieces: &[ContextPiece]) -> Result<Prepared, String> {
    let question = question.trim();
    if question.is_empty() {
        return Err("write a question first".to_owned());
    }
    if question.chars().count() > MAX_QUESTION_CHARS {
        return Err(format!(
            "the question is longer than {MAX_QUESTION_CHARS} characters"
        ));
    }
    if pieces.len() > MAX_PIECES {
        return Err("too many pieces of context".to_owned());
    }
    let mut totals: std::collections::BTreeMap<String, usize> = Default::default();
    let mut add = |removed: Vec<Removed>| {
        for r in removed {
            *totals.entry(r.kind).or_insert(0) += r.count;
        }
    };
    // The question is scrubbed too: a pasted password does not go out.
    let (question, removed) = scrub(question);
    add(removed);
    let mut user = String::new();
    let mut truncated = 0;
    for piece in pieces.iter().filter(|p| !p.text.trim().is_empty()) {
        let (text, cut) = keep_end(&piece.text, MAX_PIECE_CHARS);
        if cut {
            truncated += 1;
        }
        let (text, removed) = scrub(text.trim_matches('\n'));
        add(removed);
        let fence = fence_for(&text);
        user.push_str(&format!(
            "{}{}:\n{fence}text\n{text}\n{fence}\n\n",
            piece.kind.title(),
            if cut { " (shortened to the end)" } else { "" }
        ));
    }
    user.push_str("Question: ");
    user.push_str(&question);
    let redactions = totals
        .into_iter()
        .map(|(kind, count)| Removed { kind, count })
        .collect();
    Ok(Prepared {
        system: SYSTEM_PROMPT.to_owned(),
        user,
        redactions,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piece(kind: ContextKind, text: &str) -> ContextPiece {
        ContextPiece {
            kind,
            text: text.to_owned(),
        }
    }

    #[test]
    fn question_alone_and_with_context() {
        let p = prepare("why?", &[]).unwrap();
        assert_eq!(p.user, "Question: why?");
        assert!(p.redactions.is_empty());
        let p = prepare(
            "why did it fail?",
            &[
                piece(ContextKind::Command, "$ make\nerror: x"),
                piece(ContextKind::Selection, "  "),
            ],
        )
        .unwrap();
        assert!(p
            .user
            .starts_with("Last command and its output:\n```text\n$ make\nerror: x\n```"));
        assert!(!p.user.contains("Selected"), "empty pieces are left out");
        assert!(p.user.ends_with("Question: why did it fail?"));
    }

    #[test]
    fn secrets_are_removed_everywhere_and_counted_once_per_kind() {
        let p = prepare(
            "my token is ghp_0123456789abcdefghijABCDEFGHIJ012345, ok?",
            &[
                piece(ContextKind::Lines, "API_KEY=abc\nPASSWORD=def"),
                piece(ContextKind::Selection, "TOKEN=ghi"),
            ],
        )
        .unwrap();
        for secret in ["ghp_0123", "abc", "def", "ghi"] {
            assert!(!p.user.contains(secret), "{secret}: {}", p.user);
        }
        let get = |kind: &str| {
            p.redactions
                .iter()
                .find(|r| r.kind == kind)
                .map(|r| r.count)
        };
        assert_eq!(get("secret-value"), Some(3));
        assert_eq!(get("github-token"), Some(1));
    }

    #[test]
    fn long_context_keeps_its_end_and_says_so() {
        let text = format!("{}END", "x".repeat(MAX_PIECE_CHARS));
        let p = prepare("q", &[piece(ContextKind::Lines, &text)]).unwrap();
        assert_eq!(p.truncated, 1);
        assert!(p.user.contains("(shortened to the end)"));
        assert!(p.user.contains("xEND\n"));
        assert!(p.user.len() < MAX_PIECE_CHARS + 200);
        // Multi-byte text is cut on a character.
        let text = "主".repeat(MAX_PIECE_CHARS + 5);
        assert!(prepare("q", &[piece(ContextKind::Lines, &text)]).is_ok());
    }

    #[test]
    fn a_fence_in_the_output_cannot_close_ours() {
        let p = prepare("q", &[piece(ContextKind::Selection, "a\n```\nb")]).unwrap();
        assert!(p.user.contains("````text\na\n```\nb\n````"), "{}", p.user);
    }

    #[test]
    fn refuses_an_empty_or_huge_question() {
        assert!(prepare("   ", &[]).is_err());
        assert!(prepare(&"q".repeat(MAX_QUESTION_CHARS + 1), &[]).is_err());
        let many = vec![piece(ContextKind::Lines, "x"); MAX_PIECES + 1];
        assert!(prepare("q", &many).is_err());
    }
}
