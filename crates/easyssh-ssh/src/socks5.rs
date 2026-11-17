//! The server half of SOCKS5 (RFC 1928), just enough for `-D`: no
//! authentication, the CONNECT command, IPv4, IPv6 and domain names.

use std::net::{Ipv4Addr, Ipv6Addr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const VERSION: u8 = 5;
const NO_AUTH: u8 = 0x00;
const NO_ACCEPTABLE_METHOD: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;

/// Reply codes (REP field).
pub mod reply {
    pub const SUCCEEDED: u8 = 0x00;
    pub const GENERAL_FAILURE: u8 = 0x01;
    pub const NOT_ALLOWED: u8 = 0x02;
    pub const NETWORK_UNREACHABLE: u8 = 0x03;
    pub const HOST_UNREACHABLE: u8 = 0x04;
    pub const CONNECTION_REFUSED: u8 = 0x05;
    pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
    pub const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 0x08;
}

#[derive(Debug, thiserror::Error)]
pub enum Socks5Error {
    #[error("not a SOCKS5 client (version byte {0})")]
    BadVersion(u8),
    #[error("the client offers no usable authentication method")]
    NoAcceptableMethod,
    #[error("command {0:#04x} is not supported (only CONNECT)")]
    UnsupportedCommand(u8),
    #[error("address type {0:#04x} is not supported")]
    UnsupportedAddress(u8),
    #[error("domain name is not valid UTF-8")]
    BadDomain,
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Where the client wants to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub host: String,
    pub port: u16,
}

/// Reads the greeting and the request. Writes the method selection, and on
/// a request it cannot serve also the failure reply. After `Ok`, the caller
/// connects and then calls [`send_reply`].
pub async fn accept_request<S>(stream: &mut S) -> Result<ConnectRequest, Socks5Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] != VERSION {
        return Err(Socks5Error::BadVersion(head[0]));
    }
    let mut methods = vec![0u8; head[1] as usize];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&NO_AUTH) {
        stream.write_all(&[VERSION, NO_ACCEPTABLE_METHOD]).await?;
        return Err(Socks5Error::NoAcceptableMethod);
    }
    stream.write_all(&[VERSION, NO_AUTH]).await?;

    let mut request = [0u8; 4];
    stream.read_exact(&mut request).await?;
    if request[0] != VERSION {
        return Err(Socks5Error::BadVersion(request[0]));
    }
    let command = request[1];
    let address_type = request[3];
    // Parse the address before judging the command, so the stream is left
    // at a clean boundary when we answer.
    let host = match address_type {
        0x01 => {
            let mut octets = [0u8; 4];
            stream.read_exact(&mut octets).await?;
            Ipv4Addr::from(octets).to_string()
        }
        0x03 => {
            let mut len = [0u8; 1];
            stream.read_exact(&mut len).await?;
            let mut name = vec![0u8; len[0] as usize];
            stream.read_exact(&mut name).await?;
            match String::from_utf8(name) {
                Ok(name) => name,
                Err(_) => {
                    send_reply(stream, reply::GENERAL_FAILURE).await?;
                    return Err(Socks5Error::BadDomain);
                }
            }
        }
        0x04 => {
            let mut octets = [0u8; 16];
            stream.read_exact(&mut octets).await?;
            Ipv6Addr::from(octets).to_string()
        }
        other => {
            send_reply(stream, reply::ADDRESS_TYPE_NOT_SUPPORTED).await?;
            return Err(Socks5Error::UnsupportedAddress(other));
        }
    };
    let mut port = [0u8; 2];
    stream.read_exact(&mut port).await?;
    if command != CMD_CONNECT {
        send_reply(stream, reply::COMMAND_NOT_SUPPORTED).await?;
        return Err(Socks5Error::UnsupportedCommand(command));
    }
    Ok(ConnectRequest {
        host,
        port: u16::from_be_bytes(port),
    })
}

/// Sends a reply with the bound address 0.0.0.0:0, which clients ignore for
/// CONNECT.
pub async fn send_reply<S>(stream: &mut S, code: u8) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&[VERSION, code, 0, 0x01, 0, 0, 0, 0, 0, 0])
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    async fn run(client_bytes: &[u8]) -> (Result<ConnectRequest, Socks5Error>, Vec<u8>) {
        let (mut client, mut server) = duplex(1024);
        client.write_all(client_bytes).await.unwrap();
        client.shutdown().await.unwrap();
        let result = accept_request(&mut server).await;
        drop(server);
        let mut written = Vec::new();
        client.read_to_end(&mut written).await.unwrap();
        (result, written)
    }

    #[tokio::test]
    async fn connect_to_ipv4() {
        let (result, written) = run(&[5, 1, 0, 5, 1, 0, 1, 127, 0, 0, 1, 0x1E, 0x61]).await;
        assert_eq!(
            result.unwrap(),
            ConnectRequest {
                host: "127.0.0.1".into(),
                port: 7777
            }
        );
        assert_eq!(written, [5, 0]);
    }

    #[tokio::test]
    async fn connect_to_domain_name() {
        let mut bytes = vec![5, 2, 2, 0, 5, 1, 0, 3, 11];
        bytes.extend_from_slice(b"example.org");
        bytes.extend_from_slice(&443u16.to_be_bytes());
        let (result, written) = run(&bytes).await;
        assert_eq!(
            result.unwrap(),
            ConnectRequest {
                host: "example.org".into(),
                port: 443
            }
        );
        assert_eq!(written, [5, 0]);
    }

    #[tokio::test]
    async fn connect_to_ipv6() {
        let mut bytes = vec![5, 1, 0, 5, 1, 0, 4];
        bytes.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        bytes.extend_from_slice(&22u16.to_be_bytes());
        let (result, _) = run(&bytes).await;
        assert_eq!(
            result.unwrap(),
            ConnectRequest {
                host: "::1".into(),
                port: 22
            }
        );
    }

    #[tokio::test]
    async fn rejects_other_versions() {
        let (result, written) = run(&[4, 1, 0]).await;
        assert!(matches!(result, Err(Socks5Error::BadVersion(4))));
        assert!(written.is_empty());
    }

    #[tokio::test]
    async fn requires_the_no_auth_method() {
        let (result, written) = run(&[5, 1, 2]).await;
        assert!(matches!(result, Err(Socks5Error::NoAcceptableMethod)));
        assert_eq!(written, [5, 0xFF]);
    }

    #[tokio::test]
    async fn bind_is_refused_with_command_not_supported() {
        let (result, written) = run(&[5, 1, 0, 5, 2, 0, 1, 1, 2, 3, 4, 0, 80]).await;
        assert!(matches!(result, Err(Socks5Error::UnsupportedCommand(2))));
        assert_eq!(written[..2], [5, 0]);
        assert_eq!(written[2..4], [5, reply::COMMAND_NOT_SUPPORTED]);
    }

    #[tokio::test]
    async fn unknown_address_type_is_refused() {
        let (result, written) = run(&[5, 1, 0, 5, 1, 0, 9]).await;
        assert!(matches!(result, Err(Socks5Error::UnsupportedAddress(9))));
        assert_eq!(written[2..4], [5, reply::ADDRESS_TYPE_NOT_SUPPORTED]);
    }

    #[tokio::test]
    async fn truncated_request_is_an_io_error() {
        let (result, _) = run(&[5, 1, 0, 5, 1, 0, 1, 127]).await;
        assert!(matches!(result, Err(Socks5Error::Io(_))));
    }

    #[tokio::test]
    async fn reply_has_the_documented_layout() {
        let (mut client, mut server) = duplex(64);
        send_reply(&mut server, reply::CONNECTION_REFUSED)
            .await
            .unwrap();
        drop(server);
        let mut got = Vec::new();
        client.read_to_end(&mut got).await.unwrap();
        assert_eq!(got, [5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
    }
}
