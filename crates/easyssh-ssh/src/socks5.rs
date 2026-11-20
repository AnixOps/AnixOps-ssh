//! The server half of SOCKS5 (RFC 1928), just enough for `-D`: no
//! authentication, the CONNECT command, IPv4, IPv6 and domain names. Like
//! `ssh -D`, it also speaks SOCKS4 and SOCKS4a (CONNECT to an IPv4 address,
//! or a host name for 4a); the user ID is ignored.

use std::net::{Ipv4Addr, Ipv6Addr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const VERSION: u8 = 5;
const NO_AUTH: u8 = 0x00;
const NO_ACCEPTABLE_METHOD: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const SOCKS4: u8 = 4;
/// SOCKS4 reply codes (CD field).
const SOCKS4_GRANTED: u8 = 90;
const SOCKS4_REJECTED: u8 = 91;
/// The longest SOCKS4 user ID or SOCKS4a host name read.
const SOCKS4_FIELD_MAX: usize = 255;

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
    #[error("a SOCKS4 field is longer than {SOCKS4_FIELD_MAX} bytes")]
    FieldTooLong,
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// The protocol the client spoke, which decides the shape of the reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Version {
    /// SOCKS4 or SOCKS4a.
    Socks4,
    #[default]
    Socks5,
}

/// Where the client wants to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectRequest {
    pub host: String,
    pub port: u16,
    pub version: Version,
}

/// Reads the greeting and the request. Writes the method selection, and on
/// a request it cannot serve also the failure reply. After `Ok`, the caller
/// connects and then calls [`reply_to`].
pub async fn accept_request<S>(stream: &mut S) -> Result<ConnectRequest, Socks5Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await?;
    if head[0] == SOCKS4 {
        return accept_socks4(stream, head[1]).await;
    }
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
        version: Version::Socks5,
    })
}

/// The rest of a SOCKS4(a) request after `VN` and `CD`: `DSTPORT`, `DSTIP`,
/// a NUL-terminated user ID and, for 4a (`DSTIP` 0.0.0.x with x > 0), a
/// NUL-terminated host name.
async fn accept_socks4<S>(stream: &mut S, command: u8) -> Result<ConnectRequest, Socks5Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut fixed = [0u8; 6];
    stream.read_exact(&mut fixed).await?;
    let port = u16::from_be_bytes([fixed[0], fixed[1]]);
    let ip = Ipv4Addr::new(fixed[2], fixed[3], fixed[4], fixed[5]);
    let _user = read_nul_terminated(stream).await?;
    let socks4a = fixed[2..5] == [0, 0, 0] && fixed[5] != 0;
    let host = if socks4a {
        match String::from_utf8(read_nul_terminated(stream).await?) {
            Ok(name) if !name.is_empty() => name,
            _ => {
                send_socks4_reply(stream, SOCKS4_REJECTED).await?;
                return Err(Socks5Error::BadDomain);
            }
        }
    } else {
        ip.to_string()
    };
    if command != CMD_CONNECT {
        send_socks4_reply(stream, SOCKS4_REJECTED).await?;
        return Err(Socks5Error::UnsupportedCommand(command));
    }
    Ok(ConnectRequest {
        host,
        port,
        version: Version::Socks4,
    })
}

async fn read_nul_terminated<S>(stream: &mut S) -> Result<Vec<u8>, Socks5Error>
where
    S: AsyncRead + Unpin,
{
    let mut field = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        if byte[0] == 0 {
            return Ok(field);
        }
        if field.len() == SOCKS4_FIELD_MAX {
            return Err(Socks5Error::FieldTooLong);
        }
        field.push(byte[0]);
    }
}

/// Answers `request` in its own protocol. `code` is a SOCKS5 reply code
/// ([`reply`]); SOCKS4 only knows granted and rejected.
pub async fn reply_to<S>(stream: &mut S, request: &ConnectRequest, code: u8) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    match request.version {
        Version::Socks5 => send_reply(stream, code).await,
        Version::Socks4 => {
            let cd = if code == reply::SUCCEEDED {
                SOCKS4_GRANTED
            } else {
                SOCKS4_REJECTED
            };
            send_socks4_reply(stream, cd).await
        }
    }
}

/// A SOCKS4 reply: `VN` 0, `CD`, and a bound address clients ignore.
async fn send_socks4_reply<S>(stream: &mut S, cd: u8) -> std::io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    stream.write_all(&[0, cd, 0, 0, 0, 0, 0, 0]).await
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
                port: 7777,
                version: Version::Socks5,
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
                port: 443,
                version: Version::Socks5,
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
                port: 22,
                version: Version::Socks5,
            }
        );
    }

    #[tokio::test]
    async fn rejects_other_versions() {
        let (result, written) = run(&[6, 1, 0]).await;
        assert!(matches!(result, Err(Socks5Error::BadVersion(6))));
        assert!(written.is_empty());
    }

    #[tokio::test]
    async fn socks4_connect_to_ipv4() {
        // VN 4, CONNECT, port 7777, 127.0.0.1, user "me".
        let (result, written) = run(&[4, 1, 0x1E, 0x61, 127, 0, 0, 1, b'm', b'e', 0]).await;
        let request = result.unwrap();
        assert_eq!(
            request,
            ConnectRequest {
                host: "127.0.0.1".into(),
                port: 7777,
                version: Version::Socks4,
            }
        );
        assert!(written.is_empty(), "SOCKS4 has no method selection");
    }

    #[tokio::test]
    async fn socks4a_connect_to_a_host_name() {
        let mut bytes = vec![4, 1, 0, 80, 0, 0, 0, 1, 0];
        bytes.extend_from_slice(b"example.org\0");
        let (result, _) = run(&bytes).await;
        assert_eq!(
            result.unwrap(),
            ConnectRequest {
                host: "example.org".into(),
                port: 80,
                version: Version::Socks4,
            }
        );
    }

    #[tokio::test]
    async fn socks4_bind_is_rejected() {
        let (result, written) = run(&[4, 2, 0, 80, 1, 2, 3, 4, 0]).await;
        assert!(matches!(result, Err(Socks5Error::UnsupportedCommand(2))));
        assert_eq!(written, [0, 91, 0, 0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn socks4_fields_are_bounded() {
        let mut bytes = vec![4, 1, 0, 80, 1, 2, 3, 4];
        bytes.extend(std::iter::repeat_n(b'x', 300));
        let (result, _) = run(&bytes).await;
        assert!(matches!(result, Err(Socks5Error::FieldTooLong)));
        // A truncated user ID is an i/o error.
        let (result, _) = run(&[4, 1, 0, 80, 1, 2, 3, 4, b'u']).await;
        assert!(matches!(result, Err(Socks5Error::Io(_))));
    }

    #[tokio::test]
    async fn replies_follow_the_clients_version() {
        let four = ConnectRequest {
            host: "h".into(),
            port: 1,
            version: Version::Socks4,
        };
        let five = ConnectRequest {
            version: Version::Socks5,
            ..four.clone()
        };
        for (request, code, expected) in [
            (&four, reply::SUCCEEDED, vec![0, 90, 0, 0, 0, 0, 0, 0]),
            (
                &four,
                reply::CONNECTION_REFUSED,
                vec![0, 91, 0, 0, 0, 0, 0, 0],
            ),
            (
                &five,
                reply::NOT_ALLOWED,
                vec![5, 2, 0, 1, 0, 0, 0, 0, 0, 0],
            ),
        ] {
            let (mut client, mut server) = duplex(64);
            reply_to(&mut server, request, code).await.unwrap();
            drop(server);
            let mut got = Vec::new();
            client.read_to_end(&mut got).await.unwrap();
            assert_eq!(got, expected);
        }
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
