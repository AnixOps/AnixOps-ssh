//! Plain-text export of a decrypted log: escape sequences are removed and
//! carriage returns and backspaces are applied to the current line, so a
//! progress bar that redraws itself exports as its last state. Cursor
//! movement and full-screen programs are not emulated; this is a readable
//! transcript, not a screen capture.

/// Terminal output (UTF-8, lossy) as plain text with `\n` line ends.
pub fn to_plain_text(data: &[u8]) -> String {
    let text = String::from_utf8_lossy(data);
    let mut out = String::with_capacity(text.len());
    let mut line: Vec<char> = Vec::new();
    let mut col = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek().copied() {
                // CSI: parameters, intermediates, then a final byte @ to ~.
                Some('[') => {
                    chars.next();
                    for n in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC and similar strings end at BEL or ESC \.
                Some(']') | Some('P') | Some('^') | Some('_') | Some('X') => {
                    chars.next();
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Charset selection takes one more byte, other escapes none.
                Some('(') | Some(')') | Some('*') | Some('+') | Some('#') => {
                    chars.next();
                    chars.next();
                }
                Some(_) => {
                    chars.next();
                }
                None => {}
            },
            '\n' => {
                out.extend(line.drain(..));
                out.push('\n');
                col = 0;
            }
            '\r' => col = 0,
            '\x08' => col = col.saturating_sub(1),
            '\t' => {
                let next = (col / 8 + 1) * 8;
                while col < next {
                    put(&mut line, &mut col, ' ');
                }
            }
            c if c.is_control() => {}
            c => put(&mut line, &mut col, c),
        }
    }
    out.extend(line);
    out
}

fn put(line: &mut Vec<char>, col: &mut usize, c: char) {
    if *col < line.len() {
        line[*col] = c;
    } else {
        line.push(c);
    }
    *col += 1;
}

#[cfg(test)]
mod tests {
    use super::to_plain_text;

    #[test]
    fn strips_escapes_and_normalises_line_ends() {
        let raw =
            "\x1b[1;32muser@host\x1b[0m:~$ echo hi\r\nhi\r\n\x1b]0;title\x07\x1b]6973;A\x1b\\next";
        assert_eq!(
            to_plain_text(raw.as_bytes()),
            "user@host:~$ echo hi\nhi\nnext"
        );
    }

    #[test]
    fn carriage_return_and_backspace_overwrite() {
        assert_eq!(to_plain_text(b"10%\r50%\r100%\r\n"), "100%\n");
        assert_eq!(to_plain_text(b"abc\x08\x08X\r\n"), "aXc\n");
        assert_eq!(to_plain_text(b"ab\tc"), "ab      c");
    }

    #[test]
    fn invalid_utf8_and_stray_controls_are_harmless() {
        assert_eq!(to_plain_text(b"a\xffb\x07c\x1b"), "a\u{fffd}bc");
        assert_eq!(to_plain_text("主机\r\n".as_bytes()), "主机\n");
    }
}
