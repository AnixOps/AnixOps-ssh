//! Length-prefixed framing: `u32` big-endian length, then the body.

use crate::error::{Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Largest frame body accepted after the handshake.
pub const MAX_FRAME: u32 = 4 * 1024 * 1024;
/// Largest frame body accepted before the peer has authenticated.
pub const MAX_HANDSHAKE_FRAME: u32 = 1024;

/// Read one frame. The length is checked against `max` before any buffer is
/// allocated, and the buffer then grows only as bytes actually arrive.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R, max: u32) -> Result<Vec<u8>> {
    let mut hdr = [0u8; 4];
    match r.read_exact(&mut hdr).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(Error::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(hdr);
    if len > max {
        return Err(Error::FrameTooLarge {
            len: len as u64,
            max,
        });
    }
    let mut body = Vec::new();
    let got = r.take(len as u64).read_to_end(&mut body).await?;
    if got != len as usize {
        return Err(Error::Closed);
    }
    Ok(body)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, body: &[u8]) -> Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| Error::FrameTooLarge {
        len: body.len() as u64,
        max: MAX_FRAME,
    })?;
    if len > MAX_FRAME {
        return Err(Error::FrameTooLarge {
            len: len as u64,
            max: MAX_FRAME,
        });
    }
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(body);
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip() {
        let mut buf = Vec::new();
        write_frame(&mut buf, b"hello").await.unwrap();
        let got = read_frame(&mut &buf[..], MAX_FRAME).await.unwrap();
        assert_eq!(got, b"hello");
    }

    #[tokio::test]
    async fn oversized_rejected_from_header_alone() {
        // Only the header is present: had the reader tried to consume the body
        // it would report `Closed`, not `FrameTooLarge`.
        let hdr = (MAX_FRAME + 1).to_be_bytes();
        let err = read_frame(&mut &hdr[..], MAX_FRAME).await.unwrap_err();
        assert!(matches!(err, Error::FrameTooLarge { .. }), "{err:?}");
        let hdr = u32::MAX.to_be_bytes();
        let err = read_frame(&mut &hdr[..], MAX_FRAME).await.unwrap_err();
        assert!(matches!(err, Error::FrameTooLarge { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn truncated_body_is_closed() {
        let mut data = 10u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"abc");
        let err = read_frame(&mut &data[..], MAX_FRAME).await.unwrap_err();
        assert!(matches!(err, Error::Closed));
    }
}
