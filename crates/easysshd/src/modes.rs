//! Terminal modes that outlive the output that set them.
//!
//! A full-screen program sets its modes once, when it starts: btop, htop,
//! vim and tmux turn on mouse reporting, the alternate screen, application
//! cursor keys and a hidden cursor, then only redraw. The scrollback ring
//! keeps the last megabyte of output, and a busy screen (btop repaints every
//! two seconds) pushes those few bytes out of it within a minute. A view that
//! attaches later (a reload, a restored layout) would replay only the
//! repaints: clicks would select text instead of reaching the program, the
//! cursor would blink in a corner and the frames would pile up in the normal
//! screen's scrollback.
//!
//! [`ModeTracker`] reads the text that leaves the ring and remembers the
//! modes it left behind; [`ModeTracker::restore`] gives the sequences that put
//! a fresh terminal back in that state, sent before the replay.

/// Longest control sequence kept while parsing; longer ones are ignored.
const MAX_SEQUENCE: usize = 64;
/// Longest window title kept.
const MAX_TITLE: usize = 1024;

/// DEC private modes worth restoring, besides the mouse and screen modes
/// handled on their own: (mode, default state).
const FLAGS: &[(u16, bool)] = &[
    (1, false),    // DECCKM: application cursor keys (vim, htop, less)
    (5, false),    // DECSCNM: reverse video
    (6, false),    // DECOM: origin mode
    (7, true),     // DECAWM: wraparound
    (12, false),   // blinking cursor
    (25, true),    // DECTCEM: cursor visible
    (45, false),   // reverse wraparound
    (66, false),   // DECNKM: application keypad
    (1004, false), // focus in/out reports (tmux, vim)
    (2004, false), // bracketed paste
    // Mouse report encodings; xterm.js acts on 1006 (SGR) and 1016 (SGR
    // pixels) and ignores the others, so each is kept as sent.
    (1005, false),
    (1015, false),
    (1006, false),
    (1016, false),
];

/// Mouse tracking: only one is active at a time.
const MOUSE_TRACKING: &[u16] = &[9, 1000, 1002, 1003];
/// Mouse report encodings, which a soft reset leaves alone.
const MOUSE_ENCODING: &[u16] = &[1005, 1015, 1006, 1016];
/// Alternate screen variants.
const ALT_SCREEN: &[u16] = &[47, 1047, 1049];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Parse {
    Ground,
    Esc,
    /// `ESC` followed by intermediates such as `(` (charset designation).
    EscIntermediate(char),
    Csi,
    /// OSC, DCS, APC, PM or SOS: skipped up to BEL or ST.
    String {
        osc: bool,
    },
    /// `ESC` inside a string: `\` ends it.
    StringEsc {
        osc: bool,
    },
}

#[derive(Debug, Clone)]
pub struct ModeTracker {
    parse: Parse,
    buf: String,
    /// The OSC being read is a title (`0;` or `2;`) that still fits.
    osc: String,
    flags: Vec<(u16, bool)>,
    mouse_tracking: Option<u16>,
    alt_screen: bool,
    /// DECSTBM parameters, when a scroll region is set.
    scroll_region: Option<String>,
    /// DECSCUSR parameter, when not the default.
    cursor_style: Option<String>,
    /// G0 designated as DEC line drawing (`ESC ( 0`).
    g0_line_drawing: bool,
    /// Shifted out to G1 (SO).
    shift_out: bool,
    title: Option<String>,
}

impl Default for ModeTracker {
    fn default() -> Self {
        ModeTracker {
            parse: Parse::Ground,
            buf: String::new(),
            osc: String::new(),
            flags: FLAGS.to_vec(),
            mouse_tracking: None,
            alt_screen: false,
            scroll_region: None,
            cursor_style: None,
            g0_line_drawing: false,
            shift_out: false,
            title: None,
        }
    }
}

impl ModeTracker {
    /// Reads `text`, which may end or start in the middle of a sequence.
    pub fn feed(&mut self, text: &str) {
        for c in text.chars() {
            self.step(c);
        }
    }

    fn step(&mut self, c: char) {
        match self.parse {
            Parse::Ground => match c {
                '\x1b' => self.parse = Parse::Esc,
                '\u{9b}' => self.begin_csi(),
                '\u{9d}' => self.begin_string(true),
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => self.begin_string(false),
                '\x0e' => self.shift_out = true,
                '\x0f' => self.shift_out = false,
                _ => {}
            },
            Parse::Esc => match c {
                '[' => self.begin_csi(),
                ']' => self.begin_string(true),
                'P' | 'X' | '^' | '_' => self.begin_string(false),
                '(' => self.parse = Parse::EscIntermediate(c),
                ' '..='/' => self.parse = Parse::EscIntermediate(c),
                '=' => self.esc_final(Some(true)),
                '>' => self.esc_final(Some(false)),
                'c' => {
                    // RIS: everything back to the defaults.
                    *self = ModeTracker::default();
                }
                '\x1b' => {}
                _ => self.parse = Parse::Ground,
            },
            Parse::EscIntermediate(intermediate) => {
                if intermediate == '(' {
                    self.g0_line_drawing = c == '0';
                }
                if (' '..='/').contains(&c) {
                    return;
                }
                self.parse = Parse::Ground;
            }
            Parse::Csi => match c {
                '\x40'..='\x7e' => {
                    self.csi(c);
                    self.parse = Parse::Ground;
                }
                '\x1b' => self.parse = Parse::Esc,
                '\x18' | '\x1a' => self.parse = Parse::Ground,
                '\x00'..='\x1f' => {} // C0 controls run inside a CSI; nothing to track
                _ => {
                    if self.buf.len() < MAX_SEQUENCE {
                        self.buf.push(c);
                    } else {
                        // Too long to be anything tracked: skip to its end.
                        self.buf.clear();
                        self.buf.push('\u{1}');
                    }
                }
            },
            Parse::String { osc } => match c {
                '\x07' => self.end_string(osc),
                '\u{9c}' => self.end_string(osc),
                '\x1b' => self.parse = Parse::StringEsc { osc },
                '\x18' | '\x1a' => {
                    self.osc.clear();
                    self.parse = Parse::Ground;
                }
                _ => {
                    if osc && !self.buf.is_empty() {
                        if self.osc.len() < MAX_TITLE + 2 {
                            self.osc.push(c);
                        } else {
                            self.buf.clear();
                            self.osc.clear();
                        }
                    }
                }
            },
            Parse::StringEsc { osc } => {
                if c == '\\' {
                    self.end_string(osc);
                } else {
                    // A new sequence cut the string short.
                    self.osc.clear();
                    self.parse = Parse::Esc;
                    self.step(c);
                }
            }
        }
    }

    fn begin_csi(&mut self) {
        self.buf.clear();
        self.parse = Parse::Csi;
    }

    fn begin_string(&mut self, osc: bool) {
        // `buf` non-empty marks an OSC that is still being recorded.
        self.buf.clear();
        if osc {
            self.buf.push('o');
        }
        self.osc.clear();
        self.parse = Parse::String { osc };
    }

    fn end_string(&mut self, osc: bool) {
        self.parse = Parse::Ground;
        if !osc || self.buf.is_empty() {
            self.osc.clear();
            return;
        }
        let text = std::mem::take(&mut self.osc);
        if let Some(title) = text.strip_prefix("0;").or_else(|| text.strip_prefix("2;")) {
            self.title = (!title.is_empty()).then(|| title.to_owned());
        }
    }

    fn esc_final(&mut self, keypad: Option<bool>) {
        if let Some(on) = keypad {
            self.set_flag(66, on);
        }
        self.parse = Parse::Ground;
    }

    fn set_flag(&mut self, mode: u16, on: bool) {
        if let Some(entry) = self.flags.iter_mut().find(|(m, _)| *m == mode) {
            entry.1 = on;
        }
    }

    fn csi(&mut self, last: char) {
        let seq = std::mem::take(&mut self.buf);
        if seq.contains('\u{1}') {
            return;
        }
        match (seq.as_bytes().first(), last) {
            (Some(b'?'), 'h' | 'l') => {
                let on = last == 'h';
                for mode in seq[1..].split(';').filter_map(|p| p.parse::<u16>().ok()) {
                    self.private_mode(mode, on);
                }
            }
            (Some(b'!'), 'p') if seq == "!" => {
                // DECSTR soft reset: modes, not the screen or the title.
                let old = std::mem::take(self);
                self.alt_screen = old.alt_screen;
                self.title = old.title;
                self.mouse_tracking = old.mouse_tracking;
                for (mode, on) in old.flags {
                    if MOUSE_ENCODING.contains(&mode) {
                        self.set_flag(mode, on);
                    }
                }
            }
            (_, 'r') if seq.bytes().all(|b| b.is_ascii_digit() || b == b';') => {
                // DECSTBM; no parameters (or the full screen) clears it.
                let full = seq.is_empty() || seq == ";" || seq == "0;0" || seq == "1;0";
                self.scroll_region = (!full).then_some(seq);
            }
            (_, 'q') if seq.ends_with(' ') => {
                let style = seq.trim_end();
                self.cursor_style = (!style.is_empty() && style != "0").then(|| style.to_owned());
            }
            _ => {}
        }
    }

    fn private_mode(&mut self, mode: u16, on: bool) {
        if MOUSE_TRACKING.contains(&mode) {
            // Setting one replaces the others; resetting any turns it off.
            self.mouse_tracking = on.then_some(mode);
        } else if ALT_SCREEN.contains(&mode) {
            self.alt_screen = on;
        } else {
            self.set_flag(mode, on);
        }
    }

    /// The sequences that bring a terminal in its default state to the one
    /// this tracker is in. Empty when nothing differs from the defaults.
    pub fn restore(&self) -> String {
        let mut out = String::new();
        if self.alt_screen {
            out.push_str("\x1b[?1049h");
        }
        if let Some(region) = &self.scroll_region {
            out.push_str(&format!("\x1b[{region}r"));
        }
        for (&(mode, default), &(_, on)) in FLAGS.iter().zip(&self.flags) {
            if on != default {
                out.push_str(&format!("\x1b[?{mode}{}", if on { 'h' } else { 'l' }));
            }
        }
        if let Some(mode) = self.mouse_tracking {
            out.push_str(&format!("\x1b[?{mode}h"));
        }
        if let Some(style) = &self.cursor_style {
            out.push_str(&format!("\x1b[{style} q"));
        }
        if self.g0_line_drawing {
            out.push_str("\x1b(0");
        }
        if self.shift_out {
            out.push('\x0e');
        }
        if let Some(title) = &self.title {
            out.push_str(&format!("\x1b]2;{title}\x07"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(text: &str) -> String {
        let mut t = ModeTracker::default();
        t.feed(text);
        t.restore()
    }

    #[test]
    fn defaults_restore_nothing() {
        assert_eq!(after(""), "");
        assert_eq!(after("plain text\r\n\x1b[1;31mred\x1b[0m"), "");
    }

    #[test]
    fn btop_startup_is_restored() {
        // What btop prints when it starts, then a repaint.
        let start =
            "\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1015h\x1b[?1006h\x1b]0;btop\x07\x1b[H\x1b[2J";
        assert_eq!(
            after(start),
            "\x1b[?1049h\x1b[?25l\x1b[?1015h\x1b[?1006h\x1b[?1002h\x1b]2;btop\x07"
        );
    }

    #[test]
    fn leaving_a_program_clears_its_modes() {
        let session = "\x1b[?1049h\x1b[?1h\x1b=\x1b[?1000h\x1b[?1006h\x1b[?25l\
                       \x1b[?1000l\x1b[?1006l\x1b[?1l\x1b>\x1b[?25h\x1b[?1049l";
        assert_eq!(after(session), "");
    }

    #[test]
    fn modes_split_across_reads() {
        let mut t = ModeTracker::default();
        for part in ["\x1b[", "?10", "02", "h\x1b", "[?1", "006h"] {
            t.feed(part);
        }
        assert_eq!(t.restore(), "\x1b[?1006h\x1b[?1002h");
    }

    #[test]
    fn several_modes_in_one_sequence() {
        assert_eq!(
            after("\x1b[?1;1000;2004h"),
            "\x1b[?1h\x1b[?2004h\x1b[?1000h"
        );
        assert_eq!(after("\x1b[?1000h\x1b[?1003h"), "\x1b[?1003h");
        assert_eq!(after("\x1b[?1003h\x1b[?1000l"), "");
    }

    #[test]
    fn tmux_style_state() {
        let text = "\x1b[?1049h\x1b[1;23r\x1b[?1004h\x1b[?2004h\x1b[5 q\x1b(0qqq\x1b(B\x1b(0";
        assert_eq!(
            after(text),
            "\x1b[?1049h\x1b[1;23r\x1b[?1004h\x1b[?2004h\x1b[5 q\x1b(0"
        );
        assert_eq!(after("\x1b[1;23r\x1b[r\x1b[5 q\x1b[0 q"), "");
    }

    #[test]
    fn resets_clear_state() {
        assert_eq!(after("\x1b[?1049h\x1b[?1000h\x1b]2;x\x07\x1bc"), "");
        // DECSTR keeps the screen and the title.
        assert_eq!(
            after("\x1b[?1049h\x1b[?1h\x1b]2;vim\x1b\\\x1b[!p"),
            "\x1b[?1049h\x1b]2;vim\x07"
        );
    }

    #[test]
    fn strings_do_not_confuse_the_parser() {
        // An OSC 52 payload or a DCS full of bytes that look like CSI.
        let big = "A".repeat(10_000);
        let text = format!("\x1b]52;c;{big}\x07\x1bP[?1000h\x1b\\\x1b[?2004h");
        assert_eq!(after(&text), "\x1b[?2004h");
        // A title too long to keep is dropped, not cut.
        assert_eq!(after(&format!("\x1b]2;{big}\x07")), "");
        // C1 introducers.
        assert_eq!(
            after("\u{9b}?1000h\u{9d}0;t\u{9c}"),
            "\x1b[?1000h\x1b]2;t\x07"
        );
    }

    #[test]
    fn overlong_csi_is_ignored() {
        let long = "1;".repeat(100);
        assert_eq!(after(&format!("\x1b[?{long}1000h\x1b[?25l")), "\x1b[?25l");
    }
}
