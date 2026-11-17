//! The `.esl` format, version 1.
//!
//! ```text
//! header (78 bytes, plain, authenticated as the AAD of every chunk)
//!    0   4  magic            "ESL1"
//!    4   1  version          1
//!    5   1  suite            1 = HKDF-SHA256 key, XChaCha20-Poly1305 chunks
//!    6  16  log id           random
//!   22   8  start time       milliseconds since the Unix epoch, big endian
//!   30  32  salt             random; HKDF salt for this log's key
//!   62  16  nonce prefix     random
//! chunks, repeated
//!    4  u32 big endian n, the length of the sealed chunk
//!    n  sealed chunk = XChaCha20-Poly1305(key, nonce, aad = header,
//!                        plaintext = tag(1) || data)
//! ```
//!
//! * key = `HKDF-SHA256(salt = header.salt, ikm = device key, info = "easyssh/v1/session-log")`.
//! * nonce = `nonce prefix (16) || chunk index (u64 big endian)`, so a chunk
//!   only opens at its own position: reordering, duplicating or dropping a
//!   chunk makes the next one fail to authenticate.
//! * The tag is the first plaintext byte (so it is encrypted and
//!   authenticated, as in libsodium's `crypto_secretstream`): `0x00` MESSAGE
//!   and `0x01` FLUSH (a chunk cut by the 2 s timer) carry terminal output;
//!   `0x03` FINAL ends the log and carries the end time (u64 big endian).
//! * A reader that reaches the end of the file (or a half-written last chunk)
//!   without a FINAL chunk reports the log as incomplete; a chunk that fails
//!   to authenticate, or bytes after FINAL, are an error.
//!
//! Chunks are cut every [`CHUNK_BYTES`] of output or every [`FLUSH_INTERVAL`]
//! (driven by the caller through [`LogWriter::flush_if_due`]).
//!
//! The `chacha20poly1305` crate has no STREAM helper in the version the
//! workspace uses, so the construction above is spelled out here; it follows
//! the same ideas (counter nonce, in-band final tag).

use chacha20poly1305::{aead::Aead, aead::Payload, KeyInit, XChaCha20Poly1305, XNonce};
use rand::{rngs::OsRng, RngCore};
use std::io::Write;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const MAGIC: &[u8; 4] = b"ESL1";
pub const VERSION: u8 = 1;
pub const SUITE_HKDF_XCHACHA: u8 = 1;
pub const HEADER_LEN: usize = 78;
pub const KEY_INFO: &[u8] = b"easyssh/v1/session-log";
/// Output is cut into a chunk at this size.
pub const CHUNK_BYTES: usize = 64 * 1024;
/// Output is cut into a chunk after this long.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(2);
/// A reader refuses chunks claiming more plaintext than this.
const MAX_DATA: usize = 1024 * 1024;
const TAG_LEN: usize = 16;

pub const TAG_MESSAGE: u8 = 0x00;
pub const TAG_FLUSH: u8 = 0x01;
pub const TAG_FINAL: u8 = 0x03;

pub type LogKey = Zeroizing<[u8; 32]>;

/// Derives a log's key from its salt: `Vault::derive_key(KEY_INFO, salt)`.
pub type KeyFn<'a> = &'a dyn Fn(&[u8]) -> Result<LogKey, String>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LogError {
    #[error("not an EasySSH session log")]
    NotALog,
    #[error("unsupported session log version {0}")]
    Unsupported(u8),
    /// A chunk failed to authenticate: the file was modified, reordered, or
    /// the wrong key was used.
    #[error("the session log is damaged or was modified (chunk {0})")]
    Tampered(u64),
    #[error("{0}")]
    Key(String),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for LogError {
    fn from(error: std::io::Error) -> Self {
        LogError::Io(error.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub log_id: [u8; 16],
    pub started_ms: u64,
    pub salt: [u8; 32],
    pub nonce_prefix: [u8; 16],
}

impl Header {
    pub fn new(started_ms: u64) -> Header {
        let mut header = Header {
            log_id: [0; 16],
            started_ms,
            salt: [0; 32],
            nonce_prefix: [0; 16],
        };
        OsRng.fill_bytes(&mut header.log_id);
        OsRng.fill_bytes(&mut header.salt);
        OsRng.fill_bytes(&mut header.nonce_prefix);
        header
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut out = [0u8; HEADER_LEN];
        out[..4].copy_from_slice(MAGIC);
        out[4] = VERSION;
        out[5] = SUITE_HKDF_XCHACHA;
        out[6..22].copy_from_slice(&self.log_id);
        out[22..30].copy_from_slice(&self.started_ms.to_be_bytes());
        out[30..62].copy_from_slice(&self.salt);
        out[62..78].copy_from_slice(&self.nonce_prefix);
        out
    }

    /// Parses the first [`HEADER_LEN`] bytes of a file.
    pub fn parse(bytes: &[u8]) -> Result<Header, LogError> {
        if bytes.len() < HEADER_LEN || &bytes[..4] != MAGIC {
            return Err(LogError::NotALog);
        }
        if bytes[4] != VERSION || bytes[5] != SUITE_HKDF_XCHACHA {
            return Err(LogError::Unsupported(bytes[4]));
        }
        Ok(Header {
            log_id: bytes[6..22].try_into().expect("16 bytes"),
            started_ms: u64::from_be_bytes(bytes[22..30].try_into().expect("8 bytes")),
            salt: bytes[30..62].try_into().expect("32 bytes"),
            nonce_prefix: bytes[62..78].try_into().expect("16 bytes"),
        })
    }

    /// The log id as lower-case hex.
    pub fn id_hex(&self) -> String {
        self.log_id.iter().map(|b| format!("{b:02x}")).collect()
    }
}

fn nonce(prefix: &[u8; 16], index: u64) -> XNonce {
    let mut n = [0u8; 24];
    n[..16].copy_from_slice(prefix);
    n[16..].copy_from_slice(&index.to_be_bytes());
    XNonce::from(n)
}

fn seal(
    key: &LogKey,
    header: &[u8; HEADER_LEN],
    prefix: &[u8; 16],
    index: u64,
    tag: u8,
    data: &[u8],
) -> Result<Vec<u8>, LogError> {
    let mut plain = Zeroizing::new(Vec::with_capacity(1 + data.len()));
    plain.push(tag);
    plain.extend_from_slice(data);
    XChaCha20Poly1305::new(&(**key).into())
        .encrypt(
            &nonce(prefix, index),
            Payload {
                msg: &plain,
                aad: header,
            },
        )
        .map_err(|_| LogError::Tampered(index))
}

/// Writes one log. Output passed to [`LogWriter::write`] is buffered and
/// sealed as chunks; nothing is ever written unencrypted.
pub struct LogWriter<W: Write> {
    out: W,
    header: Header,
    header_bytes: [u8; HEADER_LEN],
    key: LogKey,
    index: u64,
    buffer: Vec<u8>,
    last_cut: Instant,
}

impl<W: Write> LogWriter<W> {
    /// Writes the header and returns a writer. `derive` receives the new
    /// log's salt.
    pub fn new(mut out: W, started_ms: u64, derive: KeyFn<'_>) -> Result<Self, LogError> {
        let header = Header::new(started_ms);
        let key = derive(&header.salt).map_err(LogError::Key)?;
        let header_bytes = header.to_bytes();
        out.write_all(&header_bytes)?;
        out.flush()?;
        Ok(LogWriter {
            out,
            header,
            header_bytes,
            key,
            index: 0,
            buffer: Vec::new(),
            last_cut: Instant::now(),
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Adds output; full chunks are sealed and written at once.
    pub fn write(&mut self, data: &[u8]) -> Result<(), LogError> {
        if self.buffer.is_empty() {
            self.last_cut = Instant::now();
        }
        self.buffer.extend_from_slice(data);
        while self.buffer.len() >= CHUNK_BYTES {
            let rest = self.buffer.split_off(CHUNK_BYTES);
            let chunk = std::mem::replace(&mut self.buffer, rest);
            self.emit(TAG_MESSAGE, &chunk)?;
            self.last_cut = Instant::now();
        }
        Ok(())
    }

    /// Seals what is buffered when it has waited for [`FLUSH_INTERVAL`].
    pub fn flush_if_due(&mut self, now: Instant) -> Result<(), LogError> {
        if !self.buffer.is_empty() && now.saturating_duration_since(self.last_cut) >= FLUSH_INTERVAL
        {
            self.flush()?;
        }
        Ok(())
    }

    /// Seals what is buffered now.
    pub fn flush(&mut self) -> Result<(), LogError> {
        if !self.buffer.is_empty() {
            let chunk = std::mem::take(&mut self.buffer);
            self.emit(TAG_FLUSH, &chunk)?;
        }
        self.last_cut = Instant::now();
        Ok(())
    }

    fn emit(&mut self, tag: u8, data: &[u8]) -> Result<(), LogError> {
        let sealed = seal(
            &self.key,
            &self.header_bytes,
            &self.header.nonce_prefix,
            self.index,
            tag,
            data,
        )?;
        self.index += 1;
        self.out.write_all(&(sealed.len() as u32).to_be_bytes())?;
        self.out.write_all(&sealed)?;
        self.out.flush()?;
        Ok(())
    }

    /// Seals the rest and the final chunk (carrying `ended_ms`), and returns
    /// the underlying writer. A log that never gets here is "incomplete".
    pub fn finish(mut self, ended_ms: u64) -> Result<W, LogError> {
        self.flush()?;
        self.emit(TAG_FINAL, &ended_ms.to_be_bytes())?;
        Ok(self.out)
    }
}

/// A log read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decrypted {
    pub header: Header,
    /// The terminal output, in order.
    pub data: Vec<u8>,
    /// The final chunk was present and authentic.
    pub complete: bool,
    /// From the final chunk.
    pub ended_ms: Option<u64>,
}

/// Decrypts a whole log held in memory.
///
/// Returns `Err(Tampered)` when a chunk does not authenticate or anything
/// follows the final chunk. A file that simply stops (at a chunk boundary or
/// inside a chunk) is returned with `complete == false`.
pub fn decrypt(bytes: &[u8], derive: KeyFn<'_>) -> Result<Decrypted, LogError> {
    let header = Header::parse(bytes)?;
    let header_bytes: [u8; HEADER_LEN] = bytes[..HEADER_LEN].try_into().expect("checked");
    let key = derive(&header.salt).map_err(LogError::Key)?;
    let cipher = XChaCha20Poly1305::new(&(*key).into());
    let mut pos = HEADER_LEN;
    let mut index = 0u64;
    let mut data = Vec::new();
    let mut complete = false;
    let mut ended_ms = None;
    while pos < bytes.len() {
        if complete {
            return Err(LogError::Tampered(index));
        }
        let Some(len_bytes) = bytes.get(pos..pos + 4) else {
            break;
        };
        let len = u32::from_be_bytes(len_bytes.try_into().expect("4 bytes")) as usize;
        if !(TAG_LEN + 1..=TAG_LEN + 1 + MAX_DATA).contains(&len) {
            return Err(LogError::Tampered(index));
        }
        let Some(sealed) = bytes.get(pos + 4..pos + 4 + len) else {
            // The last chunk was cut short.
            break;
        };
        let plain = cipher
            .decrypt(
                &nonce(&header.nonce_prefix, index),
                Payload {
                    msg: sealed,
                    aad: &header_bytes,
                },
            )
            .map_err(|_| LogError::Tampered(index))?;
        let plain = Zeroizing::new(plain);
        match plain[0] {
            TAG_MESSAGE | TAG_FLUSH => data.extend_from_slice(&plain[1..]),
            TAG_FINAL => {
                complete = true;
                ended_ms = plain
                    .get(1..9)
                    .map(|b| u64::from_be_bytes(b.try_into().expect("8 bytes")));
            }
            _ => return Err(LogError::Tampered(index)),
        }
        pos += 4 + len;
        index += 1;
    }
    Ok(Decrypted {
        header,
        data,
        complete,
        ended_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_fn(salt: &[u8]) -> Result<LogKey, String> {
        // A test stand-in for the vault: any function of the salt.
        let mut k = [7u8; 32];
        for (i, b) in salt.iter().enumerate() {
            k[i % 32] ^= *b;
        }
        Ok(Zeroizing::new(k))
    }

    fn write_log(chunks: &[&[u8]], finish: bool) -> Vec<u8> {
        let mut w = LogWriter::new(Vec::new(), 1_000, &key_fn).unwrap();
        for c in chunks {
            w.write(c).unwrap();
            w.flush().unwrap();
        }
        if finish {
            w.finish(5_000).unwrap()
        } else {
            w.out
        }
    }

    #[test]
    fn round_trip() {
        let bytes = write_log(&[b"hello ", b"world\r\n"], true);
        let d = decrypt(&bytes, &key_fn).unwrap();
        assert_eq!(d.data, b"hello world\r\n");
        assert!(d.complete);
        assert_eq!(d.ended_ms, Some(5_000));
        assert_eq!(d.header.started_ms, 1_000);
        assert!(!bytes.windows(5).any(|w| w == b"hello"));
    }

    #[test]
    fn large_output_is_cut_at_64_kib() {
        let big = vec![b'x'; CHUNK_BYTES * 2 + 5];
        let mut w = LogWriter::new(Vec::new(), 0, &key_fn).unwrap();
        w.write(&big).unwrap();
        // Two full chunks are already on disk before finish.
        assert!(w.out.len() > 2 * CHUNK_BYTES);
        let bytes = w.finish(1).unwrap();
        let d = decrypt(&bytes, &key_fn).unwrap();
        assert_eq!(d.data, big);
        assert!(d.complete);
    }

    #[test]
    fn the_timer_cuts_a_chunk() {
        let mut w = LogWriter::new(Vec::new(), 0, &key_fn).unwrap();
        w.write(b"abc").unwrap();
        let before = w.out.len();
        w.flush_if_due(Instant::now()).unwrap();
        assert_eq!(w.out.len(), before, "too early");
        w.flush_if_due(Instant::now() + FLUSH_INTERVAL).unwrap();
        assert!(w.out.len() > before);
        let d = decrypt(&w.out, &key_fn).unwrap();
        assert_eq!(d.data, b"abc");
        assert!(!d.complete);
    }

    #[test]
    fn missing_final_chunk_is_incomplete_not_an_error() {
        let bytes = write_log(&[b"one", b"two"], false);
        let d = decrypt(&bytes, &key_fn).unwrap();
        assert_eq!(d.data, b"onetwo");
        assert!(!d.complete);
        // Cut inside the last chunk.
        let d = decrypt(&bytes[..bytes.len() - 5], &key_fn).unwrap();
        assert_eq!(d.data, b"one");
        assert!(!d.complete);
        // Header only.
        let d = decrypt(&bytes[..HEADER_LEN], &key_fn).unwrap();
        assert!(d.data.is_empty() && !d.complete);
    }

    #[test]
    fn truncating_at_a_chunk_boundary_drops_the_final_tag() {
        let bytes = write_log(&[b"one", b"two"], true);
        // Remove the final chunk: 4 + 16 + 1 + 8.
        let cut = &bytes[..bytes.len() - (4 + TAG_LEN + 1 + 8)];
        let d = decrypt(cut, &key_fn).unwrap();
        assert_eq!(d.data, b"onetwo");
        assert!(!d.complete);
    }

    #[test]
    fn every_bit_flip_is_detected() {
        let bytes = write_log(&[b"secret one", b"secret two"], true);
        for i in 0..bytes.len() {
            let mut bad = bytes.clone();
            bad[i] ^= 0x01;
            match decrypt(&bad, &key_fn) {
                Err(_) => {}
                // A flipped length that now looks like a cut-short last chunk
                // is the one case that may read as incomplete.
                Ok(d) => assert!(!d.complete, "bit flip at {i} went unnoticed"),
            }
        }
    }

    #[test]
    fn reordered_chunks_are_rejected() {
        let bytes = write_log(&[b"aaaa", b"bbbb"], true);
        let one = 4 + TAG_LEN + 1 + 4;
        let first = &bytes[HEADER_LEN..HEADER_LEN + one];
        let second = &bytes[HEADER_LEN + one..HEADER_LEN + 2 * one];
        let mut swapped = bytes[..HEADER_LEN].to_vec();
        swapped.extend_from_slice(second);
        swapped.extend_from_slice(first);
        swapped.extend_from_slice(&bytes[HEADER_LEN + 2 * one..]);
        assert!(matches!(
            decrypt(&swapped, &key_fn),
            Err(LogError::Tampered(0))
        ));
        // A dropped middle chunk too.
        let mut dropped = bytes[..HEADER_LEN].to_vec();
        dropped.extend_from_slice(first);
        dropped.extend_from_slice(&bytes[HEADER_LEN + 2 * one..]);
        assert!(matches!(
            decrypt(&dropped, &key_fn),
            Err(LogError::Tampered(1))
        ));
    }

    #[test]
    fn data_after_the_final_chunk_is_rejected() {
        let mut bytes = write_log(&[b"x"], true);
        let again = bytes[HEADER_LEN..].to_vec();
        bytes.extend_from_slice(&again);
        assert!(matches!(
            decrypt(&bytes, &key_fn),
            Err(LogError::Tampered(_))
        ));
    }

    #[test]
    fn wrong_key_and_foreign_files_fail() {
        let bytes = write_log(&[b"x"], true);
        let other = |_: &[u8]| -> Result<LogKey, String> { Ok(Zeroizing::new([1; 32])) };
        assert!(matches!(
            decrypt(&bytes, &other),
            Err(LogError::Tampered(0))
        ));
        assert_eq!(decrypt(b"nope", &key_fn), Err(LogError::NotALog));
        let locked = |_: &[u8]| -> Result<LogKey, String> { Err("the vault is locked".into()) };
        assert_eq!(
            decrypt(&bytes, &locked),
            Err(LogError::Key("the vault is locked".into()))
        );
    }

    #[test]
    fn each_log_gets_its_own_salt_and_key() {
        let a = write_log(&[b"x"], true);
        let b = write_log(&[b"x"], true);
        assert_ne!(a[30..62], b[30..62]);
        assert_ne!(a[HEADER_LEN..], b[HEADER_LEN..]);
    }
}
