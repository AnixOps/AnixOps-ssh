//! Small helpers shared by the importers.

/// Decodes text exports: UTF-8 (with or without BOM) and UTF-16 with BOM,
/// which PuTTY `.reg` and Xshell `.xsh` files commonly use. Invalid
/// sequences are replaced, never an error.
pub(crate) fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, u16::from_le_bytes);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, u16::from_be_bytes);
    }
    // BOM-less UTF-16 of mostly ASCII text: a NUL in the second byte.
    if bytes.len() >= 4 && bytes[0] != 0 && bytes[1] == 0 && bytes[2] != 0 && bytes[3] == 0 {
        return decode_utf16(bytes, u16::from_le_bytes);
    }
    if bytes.len() >= 4 && bytes[0] == 0 && bytes[1] != 0 && bytes[2] == 0 && bytes[3] != 0 {
        return decode_utf16(bytes, u16::from_be_bytes);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

fn decode_utf16(bytes: &[u8], read: fn([u8; 2]) -> u16) -> String {
    let units = bytes.chunks_exact(2).map(|pair| read([pair[0], pair[1]]));
    char::decode_utf16(units)
        .map(|c| c.unwrap_or('\u{FFFD}'))
        .collect()
}

/// Percent-decodes (`%20`), as PuTTY does for session names. Malformed
/// escapes are kept literally.
pub(crate) fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|d| d as u8)
}

/// Parses a port; `None` for empty, zero or out-of-range values.
pub(crate) fn parse_port(text: &str) -> Option<u16> {
    text.trim().parse::<u16>().ok().filter(|port| *port != 0)
}

/// One `[section]` of an INI file.
#[derive(Debug, Default)]
pub(crate) struct IniSection {
    pub name: String,
    /// `(key, value, line)` in file order.
    pub entries: Vec<(String, String, usize)>,
}

impl IniSection {
    /// First value for `key`, compared case-insensitively.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v, _)| v.as_str())
    }
}

/// Minimal INI reader: `[section]`, `key=value` split at the first `=`,
/// `;` comment lines. Entries before the first header go to a section with
/// an empty name.
pub(crate) fn parse_ini(text: &str) -> Vec<IniSection> {
    let mut sections = vec![IniSection::default()];
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}');
        let number = index + 1;
        if line.is_empty() || line.starts_with(';') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            let name = rest.split_once(']').map_or(rest, |(name, _)| name);
            sections.push(IniSection {
                name: name.trim().to_owned(),
                entries: Vec::new(),
            });
        } else if let Some((key, value)) = line.split_once('=') {
            if let Some(section) = sections.last_mut() {
                section
                    .entries
                    .push((key.trim().to_owned(), value.trim().to_owned(), number));
            }
        }
    }
    sections
}

/// Splits a `/` or `\` separated folder path.
pub(crate) fn split_path(text: &str) -> Vec<String> {
    crate::model::clean_path(text.split(['/', '\\']))
}
