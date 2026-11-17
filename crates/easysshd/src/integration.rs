//! Opt-in shell integration: delivering `shell-integration/*` to a session.
//!
//! The daemon owns this, not the UI and not the SSH engine, because it sits
//! on the byte stream both engines produce (the system `ssh` on a PTY today,
//! russh later) and survives UI reloads: the same code, the same output
//! filtering and the same replay for every engine and every window.
//!
//! How a session gets the script, only for hosts that enabled it:
//!
//! 1. **Wait for a prompt.** Nothing is typed until the output has been idle
//!    for a moment and its last line looks like a shell prompt (and not like
//!    a password, passphrase or host key question). If the user types
//!    anything first, or no prompt appears within [`GIVE_UP`], nothing is
//!    ever sent: bytes must not reach an authentication prompt.
//! 2. **Send two lines**, both starting with a space so shells that honour
//!    that skip their history. The first only prepares zsh (scoped
//!    `HIST_IGNORE_SPACE`) and cleans bash's history. The second dispatches
//!    on `$BASH_VERSION`, `$ZSH_VERSION` and `$FISH_VERSION` (it works as
//!    typed text in all three and in nothing else), runs the matching script
//!    through `eval`, and ends with an `E` sequence. An unknown shell
//!    evaluates nothing and simply reports `E`, so it degrades silently.
//! 3. **Hide the exchange.** From the moment the lines are sent until `E`
//!    arrives (or [`MASK_TIMEOUT`] passes) the output is held back and
//!    dropped, except the `R` (ready) sequence, so the typed script never
//!    shows and never enters the scrollback that a reattach replays.

use std::time::{Duration, Instant};

pub const BASH: &str = include_str!("../../../shell-integration/easyssh.bash");
pub const ZSH: &str = include_str!("../../../shell-integration/easyssh.zsh");
pub const FISH: &str = include_str!("../../../shell-integration/easyssh.fish");

/// No prompt seen within this long after the session started: give up.
pub const GIVE_UP: Duration = Duration::from_secs(45);
/// Output is hidden for at most this long while the script runs.
pub const MASK_TIMEOUT: Duration = Duration::from_secs(15);
/// Quiet time that, after a recognisable prompt, means the shell waits for input.
pub const PROMPT_IDLE: Duration = Duration::from_millis(300);
/// Quiet time that is enough for output that does not look like a known prompt.
pub const UNKNOWN_IDLE: Duration = Duration::from_millis(2000);

const OSC_PREFIX: &str = "\x1b]6973;";
const END_MARKER: &str = "\x1b]6973;E\x07";
const READY_PREFIX: &str = "\x1b]6973;R;";
/// fish repaints the whole typed command after every key, so the hidden echo
/// can run to megabytes. Past this much, older text is dropped (the `R`
/// sequences in it are kept) and only the end is searched for `E`.
const MAX_HELD: usize = 256 * 1024;
const KEEP_HELD: usize = 1024;
const MAX_TAIL: usize = 512;

/// Removes comments, blank lines and indentation; the lines are joined with
/// carriage returns, which is what Enter sends.
pub fn minify(script: &str) -> String {
    script
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\r")
}

fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The two lines to type, each ending with Enter.
pub fn bootstrap_lines() -> [String; 2] {
    let prepare = concat!(
        r#" [ -n "$ZSH_VERSION" ] && eval 'if [[ -o histignorespace ]]; then ES_HS=1; else ES_HS=0; setopt histignorespace; fi'; "#,
        r#"[ -n "$BASH_VERSION" ] && eval '__easyssh_h=$(HISTTIMEFORMAT= builtin history 1 2>/dev/null); "#,
        r#"case $__easyssh_h in *easyssh-si-bootstrap*) read -r __easyssh_n _ <<<"$__easyssh_h"; builtin history -d "${__easyssh_n%\*}" 2>/dev/null;; esac; unset __easyssh_h __easyssh_n'"#,
        "\r"
    );
    let run = format!(
        " [ -n \"$BASH_VERSION\" ] && eval {} 2>/dev/null; [ -n \"$ZSH_VERSION\" ] && eval {} 2>/dev/null; [ -n \"$FISH_VERSION\" ] && eval {} 2>/dev/null; printf '\\033]6973;E\\a'\r",
        quote(&minify(BASH)),
        quote(&minify(ZSH)),
        quote(&minify(FISH)),
    );
    [prepare.to_owned(), run]
}

/// `text` without terminal escape sequences (CSI, OSC and two-byte ones).
pub fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            Some('(') | Some(')') => {
                chars.next();
            }
            _ => {}
        }
    }
    out
}

/// What the end of the output says about whether the shell waits for a command.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Prompt {
    /// A known prompt character ends the line.
    Likely,
    /// Something else that is not an authentication or other question.
    Unknown,
    /// A password, passphrase or yes/no question: type nothing.
    Question,
}

pub fn classify(tail: &str) -> Prompt {
    let visible = strip_escapes(tail);
    let line = visible
        .rsplit(['\n', '\r'])
        .find(|segment| !segment.trim().is_empty())
        .unwrap_or("");
    let lower = line.to_ascii_lowercase();
    if ["password", "passphrase", "yes/no", "verification", "otp"]
        .iter()
        .any(|word| lower.contains(word))
    {
        return Prompt::Question;
    }
    let trimmed = line.trim_end();
    match trimmed.chars().last() {
        None => Prompt::Unknown,
        Some('$' | '#' | '%' | '>' | '❯' | '➜' | 'λ' | '»' | '→' | '›') => Prompt::Likely,
        Some(':' | '?' | ']' | ')') => Prompt::Question,
        Some(_) => Prompt::Unknown,
    }
}

#[derive(Debug)]
enum Phase {
    Waiting,
    Masking {
        since: Instant,
        held: String,
        /// `R` sequences found in text that was already dropped.
        ready: String,
    },
    Done,
}

/// What the session should do after an event.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Type these lines (the output is being hidden from now on).
    Inject,
}

/// The per-session state of the delivery, kept under the session's lock.
#[derive(Debug)]
pub struct Integration {
    phase: Phase,
    started: Instant,
    last_output: Instant,
    saw_output: bool,
    user_input: bool,
    tail: String,
}

impl Integration {
    pub fn new(now: Instant) -> Self {
        Integration {
            phase: Phase::Waiting,
            started: now,
            last_output: now,
            saw_output: false,
            user_input: false,
            tail: String::new(),
        }
    }

    /// The user (or anything but the integration itself) sent input.
    pub fn note_input(&mut self) {
        if matches!(self.phase, Phase::Waiting) {
            self.user_input = true;
        }
    }

    pub fn is_done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }

    /// Takes the text the PTY produced and returns what may be shown.
    pub fn filter(&mut self, text: &str, now: Instant) -> String {
        match &mut self.phase {
            Phase::Waiting => {
                self.last_output = now;
                self.saw_output = true;
                self.tail.push_str(text);
                if self.tail.len() > MAX_TAIL {
                    let mut cut = self.tail.len() - MAX_TAIL;
                    while !self.tail.is_char_boundary(cut) {
                        cut += 1;
                    }
                    self.tail.drain(..cut);
                }
                text.to_owned()
            }
            Phase::Masking { held, ready, .. } => {
                held.push_str(text);
                if let Some(at) = held.find(END_MARKER) {
                    let released = format!("{ready}{}", release(held, Some(at)));
                    self.phase = Phase::Done;
                    released
                } else {
                    if held.len() > MAX_HELD {
                        ready.push_str(&ready_sequences(
                            &held[..held.len() - KEEP_HELD.min(held.len())],
                        ));
                        let mut cut = held.len() - KEEP_HELD.min(held.len());
                        while !held.is_char_boundary(cut) {
                            cut += 1;
                        }
                        held.drain(..cut);
                    }
                    String::new()
                }
            }
            Phase::Done => text.to_owned(),
        }
    }

    /// Called on a timer. May decide to start the delivery, or to give up.
    pub fn tick(&mut self, now: Instant) -> Action {
        match &self.phase {
            Phase::Waiting => {
                if self.user_input || now.duration_since(self.started) > GIVE_UP {
                    self.phase = Phase::Done;
                    return Action::Nothing;
                }
                if !self.saw_output {
                    return Action::Nothing;
                }
                let idle = now.duration_since(self.last_output);
                let ready = match classify(&self.tail) {
                    Prompt::Likely => idle >= PROMPT_IDLE,
                    Prompt::Unknown => idle >= UNKNOWN_IDLE,
                    Prompt::Question => false,
                };
                if ready {
                    self.phase = Phase::Masking {
                        since: now,
                        held: String::new(),
                        ready: String::new(),
                    };
                    Action::Inject
                } else {
                    Action::Nothing
                }
            }
            _ => Action::Nothing,
        }
    }

    /// Output held back for too long (the script never reported `E`): gives it
    /// back so that nothing stays hidden.
    pub fn flush_stalled(&mut self, now: Instant) -> Option<String> {
        let Phase::Masking { since, held, ready } = &mut self.phase else {
            return None;
        };
        if now.duration_since(*since) < MASK_TIMEOUT {
            return None;
        }
        let released = format!("{ready}{}", release(held, None));
        self.phase = Phase::Done;
        Some(released)
    }
}

/// What is shown from the held output: the `R` sequences, then whatever
/// followed `E`. Without an `E` (a timeout) the typed lines are dropped and
/// the rest is kept.
fn release(held: &str, end: Option<usize>) -> String {
    let (before, after) = match end {
        Some(at) => (&held[..at], &held[at + END_MARKER.len()..]),
        None => (held, ""),
    };
    let mut out = ready_sequences(before);
    out.push_str(after);
    out
}

/// The complete `R` (ready) sequences in `text`, in order.
fn ready_sequences(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(READY_PREFIX) {
        let tail = &rest[start..];
        let Some(stop) = tail.find('\x07') else { break };
        out.push_str(&tail[..=stop]);
        rest = &tail[stop + 1..];
    }
    out
}

/// Whether `text` holds an integration sequence of this kind (used by tests
/// and the daemon's log).
pub fn has_sequence(text: &str, kind: char) -> bool {
    text.contains(&format!("{OSC_PREFIX}{kind}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_fit_on_one_typed_line() {
        for (name, script) in [("bash", BASH), ("zsh", ZSH), ("fish", FISH)] {
            let text = minify(script);
            assert!(text.is_ascii(), "{name} has non-ASCII text");
            assert!(!text.contains('\t'), "{name} has a tab");
            assert!(!text.contains('!'), "{name} has '!' (history expansion)");
            assert!(!text.contains("\\\\"), "{name} has a backslash pair");
            assert!(
                !text.contains("\\'"),
                "{name} has a backslash before a quote"
            );
            for line in text.split('\r') {
                assert!(!line.starts_with('^'), "{name}: {line}");
                assert!(!line.chars().any(char::is_control), "{name}: {line}");
            }
        }
        let [first, second] = bootstrap_lines();
        assert!(first.starts_with(' ') && second.starts_with(' '));
        assert!(second.len() < 8000, "line is {} bytes", second.len());
        assert!(second.ends_with("printf '\\033]6973;E\\a'\r"));
    }

    #[test]
    fn classifies_prompts_and_questions() {
        assert_eq!(classify("Last login: Mon\r\nuser@host:~$ "), Prompt::Likely);
        assert_eq!(classify("\x1b[1;32muser@host\x1b[0m:~# "), Prompt::Likely);
        assert_eq!(classify("host% "), Prompt::Likely);
        assert_eq!(classify("~ ❯ "), Prompt::Likely);
        assert_eq!(classify("user@host's password: "), Prompt::Question);
        assert_eq!(classify("Enter passphrase for key 'k': "), Prompt::Question);
        assert_eq!(
            classify("Are you sure (yes/no/[fingerprint])? "),
            Prompt::Question
        );
        assert_eq!(classify("Verification code: "), Prompt::Question);
        assert_eq!(classify("Welcome to the box"), Prompt::Unknown);
        assert_eq!(classify(""), Prompt::Unknown);
    }

    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    #[test]
    fn injects_after_a_quiet_prompt_and_hides_the_exchange() {
        let t0 = Instant::now();
        let mut i = Integration::new(t0);
        assert_eq!(i.tick(at(t0, 5000)), Action::Nothing, "no output yet");
        assert_eq!(
            i.filter("banner\r\nme@h:~$ ", at(t0, 100)),
            "banner\r\nme@h:~$ "
        );
        assert_eq!(
            i.tick(at(t0, 200)),
            Action::Nothing,
            "not quiet long enough"
        );
        assert_eq!(i.tick(at(t0, 500)), Action::Inject);
        // The echo of the typed lines is hidden; R passes; E ends the hiding.
        assert_eq!(i.filter(" [ -n \"$ZSH", at(t0, 600)), "");
        let shown = i.filter(
            "\r\n> more\r\n\x1b]6973;R;bash\x07\x1b]6973;E\x07\x1b]6973;A\x07me@h:~$ ",
            at(t0, 650),
        );
        assert_eq!(shown, "\x1b]6973;R;bash\x07\x1b]6973;A\x07me@h:~$ ");
        assert!(i.is_done());
        assert_eq!(i.filter("ls\r\n", at(t0, 700)), "ls\r\n");
    }

    #[test]
    fn end_marker_split_across_chunks() {
        let t0 = Instant::now();
        let mut i = Integration::new(t0);
        i.filter("h$ ", t0);
        assert_eq!(i.tick(at(t0, 400)), Action::Inject);
        assert_eq!(i.filter("echo\x1b]69", at(t0, 410)), "");
        assert_eq!(i.filter("73;E\x07h$ ", at(t0, 420)), "h$ ");
    }

    #[test]
    fn never_types_into_a_question_or_after_user_input() {
        let t0 = Instant::now();
        let mut i = Integration::new(t0);
        i.filter("me@host's password: ", t0);
        assert_eq!(i.tick(at(t0, 10_000)), Action::Nothing);

        let mut i = Integration::new(t0);
        i.filter("me@h:~$ ", t0);
        i.note_input();
        assert_eq!(i.tick(at(t0, 1000)), Action::Nothing);
        assert!(i.is_done(), "once the user typed, the offer is withdrawn");
    }

    #[test]
    fn unknown_looking_output_waits_longer_and_gives_up() {
        let t0 = Instant::now();
        let mut i = Integration::new(t0);
        i.filter("custom prompt", t0);
        assert_eq!(i.tick(at(t0, 1000)), Action::Nothing);
        assert_eq!(i.tick(at(t0, 2100)), Action::Inject);

        let mut i = Integration::new(t0);
        i.filter("Press a key:", t0);
        assert_eq!(i.tick(at(t0, 50_000)), Action::Nothing);
        assert!(i.is_done());
    }

    #[test]
    fn a_huge_echo_keeps_the_ready_sequence() {
        let t0 = Instant::now();
        let mut i = Integration::new(t0);
        i.filter("h$ ", t0);
        assert_eq!(i.tick(at(t0, 400)), Action::Inject);
        let noise = "x".repeat(100 * 1024);
        assert_eq!(i.filter(&noise, at(t0, 410)), "");
        assert_eq!(i.filter("\x1b]6973;R;fish\x07", at(t0, 420)), "");
        for _ in 0..5 {
            assert_eq!(i.filter(&noise, at(t0, 430)), "");
        }
        assert_eq!(
            i.filter("\x1b]6973;E\x07h> ", at(t0, 440)),
            "\x1b]6973;R;fish\x07h> "
        );
    }

    #[test]
    fn a_stalled_script_gives_the_output_back() {
        let t0 = Instant::now();
        let mut i = Integration::new(t0);
        i.filter("h$ ", t0);
        assert_eq!(i.tick(at(t0, 400)), Action::Inject);
        assert_eq!(i.filter("sh: syntax error\r\n", at(t0, 500)), "");
        assert_eq!(i.flush_stalled(at(t0, 1000)), None);
        assert!(i.flush_stalled(at(t0, 16_000)).is_some());
        assert!(i.is_done());
    }
}
