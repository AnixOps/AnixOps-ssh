//! Outbound proxies: the client side of SOCKS5 (RFC 1928, with the
//! username/password method of RFC 1929) and of HTTP `CONNECT`, with basic
//! authentication. They give the SSH handshake a TCP stream that already
//! reaches the target through the proxy.
//!
//! Written by hand on tokio because the protocols are short and every byte
//! of them is tested here against scripted proxies; a crate would add a
//! dependency tree (and its own idea of timeouts and error text) for a few
//! hundred lines. The destination always travels as given: a name is sent to
//! the proxy as a name (SOCKS5 address type 3, the authority of `CONNECT`),
//! so the proxy resolves it (remote DNS) and a name that only the proxy's
//! network knows still works.
//!
//! Failures say which side said no: the proxy could not be reached, it wants
//! (or refused) a login, it refused the request, or it could not reach the
//! target.

use base64::Engine as _;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use zeroize::Zeroizing;

/// The protocol an outbound proxy speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyKind {
    Socks5,
    Http,
}

/// An outbound proxy and the login for it, if any.
#[derive(Clone)]
pub struct Proxy {
    pub kind: ProxyKind,
    pub host: String,
    pub port: u16,
    username: Option<String>,
    password: Option<Arc<Zeroizing<String>>>,
}

impl Proxy {
    pub fn new(kind: ProxyKind, host: impl Into<String>, port: u16) -> Self {
        Self {
            kind,
            host: host.into(),
            port,
            username: None,
            password: None,
        }
    }

    pub fn socks5(host: impl Into<String>, port: u16) -> Self {
        Self::new(ProxyKind::Socks5, host, port)
    }

    pub fn http(host: impl Into<String>, port: u16) -> Self {
        Self::new(ProxyKind::Http, host, port)
    }

    /// Logs in with `username` and `password` (which may be empty). Without
    /// a username the proxy is used without authentication.
    pub fn login(mut self, username: impl Into<String>, password: impl Into<String>) -> Self {
        self.username = Some(username.into());
        self.password = Some(Arc::new(Zeroizing::new(password.into())));
        self
    }

    /// The user name of the login, if there is one.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// `socks5://host:port` or `http://host:port`; never the login.
    pub fn display(&self) -> String {
        let scheme = match self.kind {
            ProxyKind::Socks5 => "socks5",
            ProxyKind::Http => "http",
        };
        if self.host.contains(':') {
            format!("{scheme}://[{}]:{}", self.host, self.port)
        } else {
            format!("{scheme}://{}:{}", self.host, self.port)
        }
    }

    fn credentials(&self) -> Option<(&str, &str)> {
        let user = self.username.as_deref()?;
        let password = self.password.as_ref().map_or("", |p| p.as_str());
        Some((user, password))
    }
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proxy")
            .field("proxy", &self.display())
            .field("login", &self.username.is_some())
            .finish()
    }
}

/// What went wrong with a proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyFailure {
    /// The TCP connection to the proxy itself failed.
    Unreachable(String),
    /// The proxy did not finish in time.
    Timeout,
    /// The proxy wants a login and none is set on the host.
    AuthRequired,
    /// The proxy refused the user name or password.
    AuthFailed,
    /// The proxy refused the request: not allowed, or not supported.
    Refused(String),
    /// The proxy accepted the request but could not reach the target.
    TargetUnreachable(String),
    /// The proxy said something that is not the protocol.
    BadReply(String),
    /// The proxy closed the connection before it answered.
    Closed,
}

/// A failed proxy connection, naming the proxy and the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyError {
    /// `socks5://host:port` or `http://host:port`.
    pub proxy: String,
    /// `host:port` the connection was meant for.
    pub target: String,
    pub failure: ProxyFailure,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (proxy, target) = (&self.proxy, &self.target);
        match &self.failure {
            ProxyFailure::Unreachable(reason) => {
                write!(f, "cannot connect to the proxy {proxy}: {reason}")
            }
            ProxyFailure::Timeout => write!(f, "the proxy {proxy} did not answer in time"),
            ProxyFailure::AuthRequired => write!(
                f,
                "the proxy {proxy} requires a user name and password; set them in the host's proxy settings"
            ),
            ProxyFailure::AuthFailed => {
                write!(f, "the proxy {proxy} rejected the user name or password")
            }
            ProxyFailure::Refused(reason) => {
                write!(f, "the proxy {proxy} refused to connect to {target}: {reason}")
            }
            ProxyFailure::TargetUnreachable(reason) => {
                write!(f, "the proxy {proxy} could not reach {target}: {reason}")
            }
            ProxyFailure::BadReply(reason) => {
                write!(f, "the proxy {proxy} sent an unexpected reply: {reason}")
            }
            ProxyFailure::Closed => {
                write!(f, "the proxy {proxy} closed the connection before it answered")
            }
        }
    }
}

impl std::error::Error for ProxyError {}

/// Connects to `host:port` through `proxy`: TCP to the proxy, then the proxy
/// protocol. `timeout` limits both together. The returned stream carries the
/// target's bytes and nothing of the proxy protocol.
pub async fn connect_through(
    proxy: &Proxy,
    host: &str,
    port: u16,
    timeout: Duration,
) -> Result<TcpStream, ProxyError> {
    let error = |failure| ProxyError {
        proxy: proxy.display(),
        target: target_name(host, port),
        failure,
    };
    let attempt = async {
        let mut stream = TcpStream::connect((proxy.host.as_str(), proxy.port))
            .await
            .map_err(|e| error(ProxyFailure::Unreachable(e.to_string())))?;
        let _ = stream.set_nodelay(true);
        handshake(&mut stream, proxy, host, port).await?;
        Ok(stream)
    };
    tokio::time::timeout(timeout, attempt)
        .await
        .map_err(|_| error(ProxyFailure::Timeout))?
}

/// Runs the proxy protocol on `stream`, which is already connected to the
/// proxy, until the proxy is connected to `host:port`.
pub async fn handshake<S>(
    stream: &mut S,
    proxy: &Proxy,
    host: &str,
    port: u16,
) -> Result<(), ProxyError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let fail = |failure| ProxyError {
        proxy: proxy.display(),
        target: target_name(host, port),
        failure,
    };
    let result = match proxy.kind {
        ProxyKind::Socks5 => socks5(stream, proxy, host, port).await,
        ProxyKind::Http => http_connect(stream, proxy, host, port).await,
    };
    result.map_err(|failure| {
        fail(match failure {
            // The peer going away mid-answer reads as an end of file.
            ProxyFailure::BadReply(reason) if reason == EOF => ProxyFailure::Closed,
            other => other,
        })
    })
}

fn target_name(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

const EOF: &str = "end of stream";

async fn read_exact<S: AsyncRead + Unpin>(
    stream: &mut S,
    buf: &mut [u8],
) -> Result<(), ProxyFailure> {
    match stream.read_exact(buf).await {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            Err(ProxyFailure::BadReply(EOF.into()))
        }
        Err(e) => Err(ProxyFailure::Unreachable(e.to_string())),
    }
}

async fn write_all<S: AsyncWrite + Unpin>(
    stream: &mut S,
    bytes: &[u8],
) -> Result<(), ProxyFailure> {
    stream
        .write_all(bytes)
        .await
        .and(stream.flush().await)
        .map_err(|e| ProxyFailure::Unreachable(e.to_string()))
}

// ---- SOCKS5 ---------------------------------------------------------------

const SOCKS_VERSION: u8 = 5;
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_PASSWORD: u8 = 0x02;
const METHOD_NONE_ACCEPTABLE: u8 = 0xFF;
const AUTH_VERSION: u8 = 1;

/// The destination as SOCKS5 sends it (address type, address bytes).
fn socks_address(host: &str) -> Result<(u8, Vec<u8>), ProxyFailure> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<Ipv4Addr>() {
        return Ok((0x01, ip.octets().to_vec()));
    }
    if let Ok(ip) = bare.parse::<Ipv6Addr>() {
        return Ok((0x04, ip.octets().to_vec()));
    }
    if host.is_empty() || host.len() > 255 || host.chars().any(char::is_control) {
        return Err(ProxyFailure::Refused(
            "the host name is empty, longer than 255 bytes, or has control characters".into(),
        ));
    }
    let mut bytes = vec![host.len() as u8];
    bytes.extend_from_slice(host.as_bytes());
    Ok((0x03, bytes))
}

fn socks_reply_failure(code: u8) -> ProxyFailure {
    match code {
        0x01 => ProxyFailure::Refused("general SOCKS server failure".into()),
        0x02 => ProxyFailure::Refused("the connection is not allowed by the proxy's rules".into()),
        0x03 => ProxyFailure::TargetUnreachable("network unreachable".into()),
        0x04 => ProxyFailure::TargetUnreachable("host unreachable".into()),
        0x05 => ProxyFailure::TargetUnreachable("connection refused by the target".into()),
        0x06 => ProxyFailure::TargetUnreachable("TTL expired".into()),
        0x07 => ProxyFailure::Refused("the proxy does not support CONNECT".into()),
        0x08 => ProxyFailure::Refused("the proxy does not support this address type".into()),
        other => ProxyFailure::BadReply(format!("unknown reply code {other:#04x}")),
    }
}

async fn socks5<S>(stream: &mut S, proxy: &Proxy, host: &str, port: u16) -> Result<(), ProxyFailure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let credentials = proxy.credentials();
    if let Some((user, password)) = credentials {
        if user.is_empty() || user.len() > 255 || password.len() > 255 {
            return Err(ProxyFailure::Refused(
                "SOCKS5 user names and passwords are limited to 255 bytes".into(),
            ));
        }
    }
    let (atyp, address) = socks_address(host)?;

    // Method selection. Only offer the login when there is one to give.
    let greeting: &[u8] = if credentials.is_some() {
        &[SOCKS_VERSION, 2, METHOD_NO_AUTH, METHOD_PASSWORD]
    } else {
        &[SOCKS_VERSION, 1, METHOD_NO_AUTH]
    };
    write_all(stream, greeting).await?;
    let mut choice = [0u8; 2];
    read_exact(stream, &mut choice).await?;
    if choice[0] != SOCKS_VERSION {
        return Err(ProxyFailure::BadReply(format!(
            "not a SOCKS5 proxy (version byte {})",
            choice[0]
        )));
    }
    match choice[1] {
        METHOD_NO_AUTH => {}
        METHOD_PASSWORD => {
            let Some((user, password)) = credentials else {
                // Not offered, so the proxy is not following the protocol.
                return Err(ProxyFailure::AuthRequired);
            };
            let mut request = vec![AUTH_VERSION, user.len() as u8];
            request.extend_from_slice(user.as_bytes());
            request.push(password.len() as u8);
            request.extend_from_slice(password.as_bytes());
            let request = Zeroizing::new(request);
            write_all(stream, &request).await?;
            let mut status = [0u8; 2];
            read_exact(stream, &mut status).await?;
            if status[0] != AUTH_VERSION {
                return Err(ProxyFailure::BadReply(format!(
                    "bad login reply version {}",
                    status[0]
                )));
            }
            if status[1] != 0 {
                return Err(ProxyFailure::AuthFailed);
            }
        }
        METHOD_NONE_ACCEPTABLE => {
            return Err(if credentials.is_some() {
                ProxyFailure::Refused("the proxy accepts none of the offered login methods".into())
            } else {
                ProxyFailure::AuthRequired
            });
        }
        other => {
            return Err(ProxyFailure::BadReply(format!(
                "the proxy chose method {other:#04x}, which was not offered"
            )));
        }
    }

    // CONNECT request.
    let mut request = vec![SOCKS_VERSION, 0x01, 0x00, atyp];
    request.extend_from_slice(&address);
    request.extend_from_slice(&port.to_be_bytes());
    write_all(stream, &request).await?;

    let mut head = [0u8; 4];
    read_exact(stream, &mut head).await?;
    if head[0] != SOCKS_VERSION {
        return Err(ProxyFailure::BadReply(format!(
            "bad reply version {}",
            head[0]
        )));
    }
    if head[1] != 0 {
        return Err(socks_reply_failure(head[1]));
    }
    // The bound address: read it so the stream starts at the target's data.
    let rest = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut len = [0u8; 1];
            read_exact(stream, &mut len).await?;
            usize::from(len[0]) + 2
        }
        other => {
            return Err(ProxyFailure::BadReply(format!(
                "unknown address type {other:#04x} in the reply"
            )))
        }
    };
    let mut bound = vec![0u8; rest];
    read_exact(stream, &mut bound).await?;
    Ok(())
}

// ---- HTTP CONNECT ---------------------------------------------------------

/// The longest reply head we read before giving up.
const MAX_HEAD: usize = 16 * 1024;

async fn http_connect<S>(
    stream: &mut S,
    proxy: &Proxy,
    host: &str,
    port: u16,
) -> Result<(), ProxyFailure>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    if host.is_empty() || host.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ProxyFailure::Refused(
            "the host name is empty or has spaces or control characters".into(),
        ));
    }
    let authority = target_name(host, port);
    let mut request = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some((user, password)) = proxy.credentials() {
        if user.contains(':') || user.chars().any(char::is_control) {
            return Err(ProxyFailure::Refused(
                "the proxy user name must not contain a colon or control characters".into(),
            ));
        }
        let token = Zeroizing::new(
            base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}")),
        );
        request.push_str(&format!(
            "Proxy-Authorization: Basic {}\r\n",
            token.as_str()
        ));
    }
    request.push_str("Proxy-Connection: keep-alive\r\n\r\n");
    let request = Zeroizing::new(request);
    write_all(stream, request.as_bytes()).await?;

    // Read byte by byte up to the blank line: whatever follows is the SSH
    // server's banner, which belongs to the SSH handshake.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_HEAD {
            return Err(ProxyFailure::BadReply("the reply head is too long".into()));
        }
        read_exact(stream, &mut byte).await?;
        head.push(byte[0]);
    }
    let text = String::from_utf8_lossy(&head);
    let status_line = text.lines().next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    let code = parts.next().and_then(|c| c.parse::<u16>().ok());
    let reason = parts.next().unwrap_or_default().trim();
    let (true, Some(code)) = (version.starts_with("HTTP/1."), code) else {
        return Err(ProxyFailure::BadReply(format!(
            "not an HTTP status line: {}",
            status_line.chars().take(80).collect::<String>()
        )));
    };
    let detail = if reason.is_empty() {
        format!("HTTP {code}")
    } else {
        format!("HTTP {code} {reason}")
    };
    match code {
        200..=299 => Ok(()),
        407 if proxy.credentials().is_some() => Err(ProxyFailure::AuthFailed),
        407 => Err(ProxyFailure::AuthRequired),
        502..=504 => Err(ProxyFailure::TargetUnreachable(detail)),
        _ => Err(ProxyFailure::Refused(detail)),
    }
}

#[cfg(test)]
mod tests;
