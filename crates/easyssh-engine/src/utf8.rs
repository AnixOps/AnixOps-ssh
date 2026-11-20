//! Terminal bytes to text and back, without losing a byte.
//!
//! The web view receives text, but a terminal stream is not always UTF-8: the
//! ZMODEM file transfer (`rz`/`sz`) sends arbitrary bytes. A byte that can
//! never be part of a UTF-8 character is therefore sent as the private-use
//! character `U+100000 + byte` (Supplementary Private Use Area-B, which no font
//! set uses; the BMP range Nerd Fonts fill with icons is left alone), and
//! [`text_to_bytes`] undoes it on the way back. The web view shows those
//! characters as U+FFFD while it is a plain terminal, and turns them back into
//! bytes while a transfer runs.

/// Base of the escape range: `U+100080..=U+1000FF` stand
/// for the bytes `0x80..=0xFF`.
pub const ESCAPE_BASE: u32 = 0x10_0000;

/// The character that carries an undecodable byte (`0x80..=0xFF`).
pub fn escape_byte(byte: u8) -> char {
    char::from_u32(ESCAPE_BASE + u32::from(byte)).unwrap_or(char::REPLACEMENT_CHARACTER)
}

/// The bytes of `text`, with escaped characters turned back into their byte.
pub fn text_to_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    let mut utf8 = [0u8; 4];
    for c in text.chars() {
        match u32::from(c) {
            n @ 0x10_0080..=0x10_00FF => out.push((n - ESCAPE_BASE) as u8),
            _ => out.extend_from_slice(c.encode_utf8(&mut utf8).as_bytes()),
        }
    }
    out
}

/// Turns terminal bytes into text for the web view. A character split across
/// two reads is held back until its remaining bytes arrive (see
/// [`Utf8Stream::flush`] for when none do); bytes that can never form a
/// character are escaped, see the module docs.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.pending.clear();
                    return text;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(&String::from_utf8_lossy(&self.pending[..valid]));
                    match error.error_len() {
                        Some(invalid) => {
                            for byte in &self.pending[valid..valid + invalid] {
                                text.push(escape_byte(*byte));
                            }
                            self.pending.drain(..valid + invalid);
                        }
                        None => {
                            self.pending.drain(..valid);
                            return text;
                        }
                    }
                }
            }
        }
    }

    /// Whether the start of a character is being held back.
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Gives up waiting for the rest of a split character and releases the
    /// held bytes as escaped bytes. Callers do this after a short quiet
    /// period: a transfer must not stall because its last byte looked like
    /// the start of a character.
    pub fn flush(&mut self) -> String {
        self.pending.drain(..).map(escape_byte).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{escape_byte, text_to_bytes, Utf8Stream};

    #[test]
    fn keeps_split_characters_until_complete() {
        let mut stream = Utf8Stream::default();
        let bytes = "主机".as_bytes();
        assert_eq!(stream.push(&bytes[..2]), "");
        assert_eq!(stream.push(&bytes[2..4]), "主");
        assert_eq!(stream.push(&bytes[4..]), "机");
    }

    #[test]
    fn escapes_invalid_bytes() {
        let mut stream = Utf8Stream::default();
        let text = stream.push(b"a\xffb");
        assert_eq!(text, format!("a{}b", escape_byte(0xff)));
        assert_eq!(text_to_bytes(&text), b"a\xffb");
    }

    #[test]
    fn every_byte_round_trips_across_any_split() {
        let mut data: Vec<u8> = (0..=255u8).collect();
        data.extend((0..=255u8).rev());
        data.extend("主机\u{1F600}".as_bytes());
        for split in 0..data.len() {
            let mut stream = Utf8Stream::default();
            let mut text = stream.push(&data[..split]);
            text.push_str(&stream.push(&data[split..]));
            text.push_str(&stream.flush());
            assert_eq!(text_to_bytes(&text), data, "split at {split}");
        }
    }

    #[test]
    fn flush_releases_a_dangling_lead_byte() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.push(b"\x18\xc3"), "\x18");
        assert!(stream.has_pending());
        assert_eq!(text_to_bytes(&stream.flush()), b"\xc3");
        assert!(!stream.has_pending());
    }
}
