use crate::channel::Channel;
use crate::error::Result;
use crate::runtime::{read_secret, Paths};
use crate::transport;
use serde_json::Value;
use std::path::Path;

/// An authenticated connection to a running daemon.
pub struct Client {
    ch: Channel,
}

impl Client {
    /// Read the secret from `<runtime_dir>/easysshd.secret` and connect.
    pub async fn connect(runtime_dir: impl AsRef<Path>) -> Result<Client> {
        let paths = Paths::new(runtime_dir.as_ref())?;
        let secret = read_secret(&paths)?;
        let io = transport::connect(&paths).await?;
        Ok(Client {
            ch: Channel::client_handshake(io, &secret).await?,
        })
    }

    pub async fn send(&mut self, msg: Value) -> Result<()> {
        self.ch.send(&msg).await
    }

    pub async fn recv(&mut self) -> Result<Value> {
        self.ch.recv().await
    }
}
