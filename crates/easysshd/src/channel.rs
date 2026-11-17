//! Handshake plus the authenticated message channel built on it.

use crate::error::{Error, Result};
use crate::frame::{read_frame, write_frame, MAX_FRAME, MAX_HANDSHAKE_FRAME};
use crate::session::{decode_nonce, encode_nonce, random_bytes, Role, Secret, Session};
use crate::transport::Conn;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::time::timeout;

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct Channel {
    io: Conn,
    session: Session,
}

impl Channel {
    pub async fn send(&mut self, v: &Value) -> Result<()> {
        let json = serde_json::to_vec(v)?;
        let body = self.session.seal(&json);
        write_frame(&mut self.io, &body).await
    }

    pub async fn recv(&mut self) -> Result<Value> {
        let body = read_frame(&mut self.io, MAX_FRAME).await?;
        let json = self.session.open(&body)?;
        Ok(serde_json::from_slice(json)?)
    }

    /// Client side. Sends the hello, then requires the server's first frame to
    /// authenticate under the derived key, which proves the server knows the
    /// secret before the client sends any payload.
    pub async fn client_handshake(mut io: Conn, secret: &Secret) -> Result<Channel> {
        timeout(HANDSHAKE_TIMEOUT, async {
            let cn = random_bytes();
            let hello = json!({"hello": 1, "client_nonce": encode_nonce(&cn)});
            write_frame(&mut io, &serde_json::to_vec(&hello)?).await?;
            let reply = read_frame(&mut io, MAX_HANDSHAKE_FRAME).await?;
            let reply: Value = serde_json::from_slice(&reply)?;
            let sn = decode_nonce(
                reply
                    .get("server_nonce")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Protocol("missing server_nonce".into()))?,
            )?;
            let mut ch = Channel {
                io,
                session: Session::new(secret, &cn, &sn, Role::Client),
            };
            match ch.recv().await {
                Ok(v) if v == json!({"ready": true}) => Ok(ch),
                Ok(_) => Err(Error::Protocol("unexpected first server message".into())),
                Err(Error::BadMac) | Err(Error::BadSequence { .. }) => Err(Error::Auth),
                Err(e) => Err(e),
            }
        })
        .await
        .map_err(|_| Error::Timeout)?
    }

    /// Server side. Returns only after the client's first frame has verified;
    /// that frame is returned as the first message. Any failure means the
    /// caller must drop the connection without sending anything further.
    pub async fn server_handshake(mut io: Conn, secret: &Secret) -> Result<(Channel, Value)> {
        timeout(HANDSHAKE_TIMEOUT, async {
            let hello = read_frame(&mut io, MAX_HANDSHAKE_FRAME).await?;
            let hello: Value = serde_json::from_slice(&hello)?;
            if hello.get("hello").and_then(Value::as_u64) != Some(1) {
                return Err(Error::Protocol("unsupported hello version".into()));
            }
            let cn = decode_nonce(
                hello
                    .get("client_nonce")
                    .and_then(Value::as_str)
                    .ok_or_else(|| Error::Protocol("missing client_nonce".into()))?,
            )?;
            let sn = random_bytes();
            write_frame(
                &mut io,
                &serde_json::to_vec(&json!({"server_nonce": encode_nonce(&sn)}))?,
            )
            .await?;
            let mut ch = Channel {
                io,
                session: Session::new(secret, &cn, &sn, Role::Server),
            };
            // The "ready" frame carries no secret; it lets the client verify us.
            ch.send(&json!({"ready": true})).await?;
            let first = ch.recv().await?;
            Ok((ch, first))
        })
        .await
        .map_err(|_| Error::Timeout)?
    }
}
