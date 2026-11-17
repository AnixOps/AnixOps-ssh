//! Handshake key derivation and per-frame authentication.
//!
//! Public so that tests (and future fuzzers) can build raw, deliberately wrong
//! frames; applications use [`crate::Client`] and [`crate::Server`].

use crate::error::{Error, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use hmac::{Hmac, Mac};
use rand::{rngs::OsRng, RngCore};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub const SECRET_LEN: usize = 32;
pub const NONCE_LEN: usize = 32;
pub const MAC_LEN: usize = 32;
const CONTEXT: &[u8] = b"easysshd-ipc-v1";

pub type Secret = [u8; SECRET_LEN];
pub type Nonce = [u8; NONCE_LEN];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Client,
    Server,
}

impl Role {
    fn direction(self) -> u8 {
        match self {
            Role::Client => 0x01, // client -> server
            Role::Server => 0x02, // server -> client
        }
    }
    fn peer(self) -> Role {
        match self {
            Role::Client => Role::Server,
            Role::Server => Role::Client,
        }
    }
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    OsRng.fill_bytes(&mut b);
    b
}

pub fn encode_nonce(n: &Nonce) -> String {
    STANDARD.encode(n)
}

pub fn decode_nonce(s: &str) -> Result<Nonce> {
    let v = STANDARD
        .decode(s)
        .map_err(|_| Error::Protocol("nonce is not valid base64".into()))?;
    v.try_into()
        .map_err(|_| Error::Protocol(format!("nonce must be {NONCE_LEN} bytes")))
}

/// Authenticated, sequenced frame state for one connection.
pub struct Session {
    key: [u8; 32],
    me: Role,
    send_seq: u64,
    recv_seq: u64,
}

impl Session {
    pub fn new(secret: &Secret, client_nonce: &Nonce, server_nonce: &Nonce, me: Role) -> Self {
        let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
        mac.update(CONTEXT);
        mac.update(client_nonce);
        mac.update(server_nonce);
        Session {
            key: mac.finalize().into_bytes().into(),
            me,
            send_seq: 0,
            recv_seq: 0,
        }
    }

    fn mac(&self, dir: u8, seq: u64, json: &[u8]) -> HmacSha256 {
        let mut mac = HmacSha256::new_from_slice(&self.key).expect("hmac accepts any key length");
        mac.update(&[dir]);
        mac.update(&seq.to_be_bytes());
        mac.update(json);
        mac
    }

    /// Build the frame body `seq || mac || json` for the next outgoing message.
    pub fn seal(&mut self, json: &[u8]) -> Vec<u8> {
        let seq = self.send_seq;
        self.send_seq += 1;
        let tag = self
            .mac(self.me.direction(), seq, json)
            .finalize()
            .into_bytes();
        let mut out = Vec::with_capacity(8 + MAC_LEN + json.len());
        out.extend_from_slice(&seq.to_be_bytes());
        out.extend_from_slice(&tag);
        out.extend_from_slice(json);
        out
    }

    /// Verify a received frame body and return its JSON bytes. The MAC is
    /// checked first (in constant time), then the sequence number. On any
    /// error the caller must drop the connection.
    pub fn open<'a>(&mut self, body: &'a [u8]) -> Result<&'a [u8]> {
        if body.len() < 8 + MAC_LEN {
            return Err(Error::Protocol("frame shorter than header".into()));
        }
        let seq = u64::from_be_bytes(body[..8].try_into().expect("8 bytes"));
        let tag = &body[8..8 + MAC_LEN];
        let json = &body[8 + MAC_LEN..];
        // `verify_slice` compares in constant time.
        self.mac(self.me.peer().direction(), seq, json)
            .verify_slice(tag)
            .map_err(|_| Error::BadMac)?;
        if seq != self.recv_seq {
            return Err(Error::BadSequence {
                expected: self.recv_seq,
                got: seq,
            });
        }
        self.recv_seq += 1;
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Session, Session) {
        let (s, c, n) = ([1u8; 32], [2u8; 32], [3u8; 32]);
        (
            Session::new(&s, &c, &n, Role::Client),
            Session::new(&s, &c, &n, Role::Server),
        )
    }

    #[test]
    fn seal_open_roundtrip_and_direction_binding() {
        let (mut c, mut s) = pair();
        let f = c.seal(b"{}");
        assert_eq!(s.open(&f).unwrap(), b"{}");
        // A client frame reflected back to the client must not verify.
        let f2 = c.seal(b"{}");
        assert!(matches!(c.open(&f2), Err(Error::BadMac)));
    }

    #[test]
    fn replay_and_gap_rejected() {
        let (mut c, mut s) = pair();
        let f0 = c.seal(b"a");
        let f1 = c.seal(b"b");
        let f2 = c.seal(b"c");
        s.open(&f0).unwrap();
        assert!(matches!(s.open(&f0), Err(Error::BadSequence { .. })));
        assert!(matches!(s.open(&f2), Err(Error::BadSequence { .. })));
        s.open(&f1).unwrap();
    }
}
