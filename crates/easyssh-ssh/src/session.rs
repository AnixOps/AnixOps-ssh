//! The connection: TCP (or a jump hop's channel), key exchange with host key
//! verification, authentication, and the handle every channel is opened on.

use crate::auth;
use crate::error::{AuthMethod, Error, Result};
use crate::forward::RemoteForwards;
use crate::interaction::{Decision, HostKeyDetails, Interaction};
use crate::known_hosts::{fingerprint, KnownHostStatus};
use crate::params::ConnectParams;
use russh::client::{self, Msg};
use russh::keys::ssh_key::{Algorithm, PublicKey};
use russh::keys::PublicKeyOrCertificate;
use russh::{Channel, ChannelOpenFailure, Preferred};
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

/// What the russh handler needs the connecting task to decide.
struct HostKeyRequest {
    key: PublicKey,
    reply: oneshot::Sender<Result<()>>,
}

/// russh callbacks. Lives inside russh's session task, so it cannot borrow
/// the caller's `Interaction`; host key questions are passed back to the
/// connecting task over a channel.
pub(crate) struct ClientHandler {
    host_key_requests: mpsc::UnboundedSender<HostKeyRequest>,
    accepted: Option<String>,
    forwards: Arc<RemoteForwards>,
}

impl client::Handler for ClientHandler {
    type Error = Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &PublicKeyOrCertificate,
    ) -> std::result::Result<bool, Error> {
        let key = match server_public_key {
            PublicKeyOrCertificate::PublicKey { key, .. } => key.clone(),
            PublicKeyOrCertificate::Certificate(_) => {
                return Err(Error::Unsupported("host certificates".into()))
            }
        };
        let print = fingerprint(&key);
        if self.accepted.as_deref() == Some(print.as_str()) {
            // A later key exchange with the key we already accepted.
            return Ok(true);
        }
        let (reply, answer) = oneshot::channel();
        self.host_key_requests
            .send(HostKeyRequest { key, reply })
            .map_err(|_| Error::Disconnected)?;
        match answer.await {
            Ok(Ok(())) => {
                self.accepted = Some(print);
                Ok(true)
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(Error::Disconnected),
        }
    }

    async fn server_channel_open_forwarded_tcpip(
        &mut self,
        channel: Channel<Msg>,
        connected_address: &str,
        connected_port: u32,
        originator_address: &str,
        originator_port: u32,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> std::result::Result<(), Error> {
        tracing::debug!(
            "forwarded-tcpip {connected_address}:{connected_port} from {originator_address}:{originator_port}"
        );
        match self.forwards.get(connected_address, connected_port) {
            Some(target) => {
                tokio::spawn(target.serve(channel, reply));
            }
            None => {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await
            }
        }
        Ok(())
    }

    async fn server_channel_open_forwarded_streamlocal(
        &mut self,
        channel: Channel<Msg>,
        socket_path: &str,
        reply: client::ChannelOpenHandle,
        _session: &mut client::Session,
    ) -> std::result::Result<(), Error> {
        tracing::debug!("forwarded-streamlocal {socket_path}");
        match self.forwards.get_socket(socket_path) {
            Some(target) => {
                tokio::spawn(target.serve(channel, reply));
            }
            None => {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await
            }
        }
        Ok(())
    }
}

fn details(
    host: &str,
    port: u16,
    key: &PublicKey,
    fingerprint: &str,
    status: KnownHostStatus,
) -> HostKeyDetails {
    HostKeyDetails {
        host: host.to_owned(),
        port,
        algorithm: key.algorithm().to_string(),
        fingerprint: fingerprint.to_owned(),
        status,
    }
}

/// Decides about a host key: known_hosts first, then the user.
async fn verify_host_key(
    params: &ConnectParams,
    interaction: &dyn Interaction,
    key: &PublicKey,
) -> Result<()> {
    let (host, port) = (params.host.as_str(), params.port);
    let print = fingerprint(key);
    match params.known_hosts.check(host, port, key)? {
        KnownHostStatus::Trusted => Ok(()),
        KnownHostStatus::Revoked => Err(Error::HostKeyRevoked {
            host: host.into(),
            port,
            fingerprint: print,
        }),
        // A key of a type not recorded yet is asked about like a new host;
        // saving it keeps the other types' keys.
        status @ (KnownHostStatus::Unknown | KnownHostStatus::OtherTypes { .. }) => {
            match interaction
                .verify_host_key_details(&details(host, port, key, &print, status))
                .await
            {
                Decision::AcceptOnce => Ok(()),
                Decision::AcceptAndSave | Decision::ReplaceAndSave => {
                    params.known_hosts.add(host, port, key)
                }
                Decision::Reject => Err(Error::HostKeyRejected {
                    host: host.into(),
                    port,
                    fingerprint: print,
                }),
            }
        }
        status @ KnownHostStatus::Changed { .. } => {
            let KnownHostStatus::Changed { expected, .. } = &status else {
                unreachable!()
            };
            let expected = expected.clone();
            // Only an explicit replacement is accepted. Whatever else the
            // Interaction answers, a changed key stays refused.
            match interaction
                .verify_host_key_details(&details(host, port, key, &print, status))
                .await
            {
                Decision::ReplaceAndSave => params.known_hosts.replace(host, port, key),
                _ => Err(Error::HostKeyMismatch {
                    host: host.into(),
                    port,
                    fingerprint: print,
                    expected,
                }),
            }
        }
    }
}

/// Offers the algorithms of keys we have on record first, as OpenSSH does.
/// The set stays russh's default; only the order changes.
fn host_key_preference(recorded: &[Algorithm]) -> Cow<'static, [Algorithm]> {
    let defaults = Preferred::DEFAULT.key;
    if recorded.is_empty() {
        return defaults;
    }
    let known = |algorithm: &Algorithm| {
        recorded
            .iter()
            .any(|r| r == algorithm || (r.clone().is_rsa() && algorithm.clone().is_rsa()))
    };
    let (mut ordered, rest): (Vec<Algorithm>, Vec<Algorithm>) =
        defaults.iter().cloned().partition(known);
    ordered.extend(rest);
    Cow::Owned(ordered)
}

struct Inner {
    handle: client::Handle<ClientHandler>,
    pub(crate) forwards: Arc<RemoteForwards>,
    host: String,
    port: u16,
    username: String,
    auth_method: AuthMethod,
    /// The jump host this session runs through; kept alive with it.
    _via: Option<Session>,
}

/// One authenticated SSH connection. Cheap to clone; every channel
/// (commands, shells, SFTP, forwards) shares it.
#[derive(Clone)]
pub struct Session {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("host", &self.inner.host)
            .field("port", &self.inner.port)
            .field("username", &self.inner.username)
            .field("auth", &self.inner.auth_method)
            .finish()
    }
}

impl Session {
    /// Connects over TCP, verifies the host key, authenticates.
    pub async fn connect(params: ConnectParams, interaction: &dyn Interaction) -> Result<Session> {
        let stream = open_tcp(&params).await?;
        Self::establish(&params, stream, interaction, None).await
    }

    /// Connects to `target` through `hops`: the first hop over TCP, every
    /// further one (and the target) through a `direct-tcpip` channel of the
    /// hop before it. Each hop verifies its own host key and authenticates
    /// with its own parameters. The hops stay connected for as long as the
    /// returned session (or a clone of it) lives.
    pub async fn connect_via(
        hops: &[ConnectParams],
        target: ConnectParams,
        interaction: &dyn Interaction,
    ) -> Result<Session> {
        let mut previous: Option<Session> = None;
        for params in hops.iter().chain(std::iter::once(&target)) {
            let session = match previous.take() {
                None => {
                    let stream = open_tcp(params).await?;
                    Self::establish(params, stream, interaction, None).await?
                }
                Some(via) => {
                    let channel = via
                        .inner
                        .handle
                        .channel_open_direct_tcpip(
                            params.host.clone(),
                            u32::from(params.port),
                            "127.0.0.1",
                            0,
                        )
                        .await
                        .map_err(|e| {
                            Error::Channel(format!(
                                "{} cannot reach {}:{}: {e}",
                                via.inner.host, params.host, params.port
                            ))
                        })?;
                    Self::establish(params, channel.into_stream(), interaction, Some(via)).await?
                }
            };
            previous = Some(session);
        }
        previous.ok_or_else(|| Error::Protocol("no target".into()))
    }

    async fn establish<S>(
        params: &ConnectParams,
        stream: S,
        interaction: &dyn Interaction,
        via: Option<Session>,
    ) -> Result<Session>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let recorded = params
            .known_hosts
            .recorded_algorithms(&params.host, params.port)
            .unwrap_or_default();
        let config = client::Config {
            keepalive_interval: params.keepalive,
            keepalive_max: params.keepalive_max.max(1),
            nodelay: true,
            preferred: Preferred {
                key: host_key_preference(&recorded),
                ..Preferred::default()
            },
            ..client::Config::default()
        };
        let forwards = Arc::new(RemoteForwards::default());
        let (host_key_requests, mut requests) = mpsc::unbounded_channel();
        let handler = ClientHandler {
            host_key_requests,
            accepted: None,
            forwards: forwards.clone(),
        };

        let mut connecting = Box::pin(client::connect_stream(Arc::new(config), stream, handler));
        let mut handle = loop {
            tokio::select! {
                connected = &mut connecting => break connected?,
                Some(request) = requests.recv() => {
                    let verdict = verify_host_key(params, interaction, &request.key).await;
                    let _ = request.reply.send(verdict);
                }
            }
        };

        let auth_method = match auth::authenticate(&mut handle, params, interaction).await {
            Ok(method) => method,
            Err(error) => {
                let _ = handle
                    .disconnect(
                        russh::Disconnect::ByApplication,
                        "authentication failed",
                        "",
                    )
                    .await;
                return Err(error);
            }
        };
        Ok(Session {
            inner: Arc::new(Inner {
                handle,
                forwards,
                host: params.host.clone(),
                port: params.port,
                username: params.username.clone(),
                auth_method,
                _via: via,
            }),
        })
    }

    /// The method that got us in.
    pub fn auth_method(&self) -> &AuthMethod {
        &self.inner.auth_method
    }

    pub fn host(&self) -> &str {
        &self.inner.host
    }

    pub fn port(&self) -> u16 {
        self.inner.port
    }

    pub fn username(&self) -> &str {
        &self.inner.username
    }

    /// Whether the connection has ended (closed, dropped or timed out).
    pub fn is_closed(&self) -> bool {
        self.inner.handle.is_closed()
    }

    /// Sends a keepalive and waits for the server's answer.
    pub async fn ping(&self) -> Result<()> {
        Ok(self.inner.handle.send_ping().await?)
    }

    /// Closes the connection. Channels and forwards on it end.
    pub async fn disconnect(&self) -> Result<()> {
        self.inner
            .handle
            .disconnect(russh::Disconnect::ByApplication, "bye", "")
            .await?;
        Ok(())
    }

    /// A session channel (shell, exec, SFTP). A server that does not answer
    /// within [`CHANNEL_OPEN_TIMEOUT`] (a connection that died without
    /// noticing) is a timeout rather than a wait of minutes; a refusal is
    /// [`Error::ChannelRefused`].
    pub(crate) async fn open_session_channel(&self) -> Result<Channel<Msg>> {
        let open = self.inner.handle.channel_open_session();
        match tokio::time::timeout(CHANNEL_OPEN_TIMEOUT, open).await {
            Ok(Ok(channel)) => Ok(channel),
            Ok(Err(russh::Error::ChannelOpenFailure(reason))) => {
                Err(Error::ChannelRefused(refusal(reason)))
            }
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err(Error::Timeout(format!(
                "the server did not open a channel within {} s",
                CHANNEL_OPEN_TIMEOUT.as_secs()
            ))),
        }
    }

    /// A `direct-tcpip` channel, with the same time limit as
    /// [`Session::open_session_channel`] (a timeout is
    /// `russh::Error::ConnectionTimeout`).
    pub(crate) async fn open_direct_tcpip(
        &self,
        host: &str,
        port: u16,
        origin: std::net::SocketAddr,
    ) -> std::result::Result<Channel<Msg>, russh::Error> {
        let open = self.inner.handle.channel_open_direct_tcpip(
            host.to_owned(),
            u32::from(port),
            origin.ip().to_string(),
            u32::from(origin.port()),
        );
        tokio::time::timeout(CHANNEL_OPEN_TIMEOUT, open)
            .await
            .unwrap_or(Err(russh::Error::ConnectionTimeout))
    }

    pub(crate) async fn open_direct_streamlocal(
        &self,
        socket: &str,
    ) -> std::result::Result<Channel<Msg>, russh::Error> {
        let open = self
            .inner
            .handle
            .channel_open_direct_streamlocal(socket.to_owned());
        tokio::time::timeout(CHANNEL_OPEN_TIMEOUT, open)
            .await
            .unwrap_or(Err(russh::Error::ConnectionTimeout))
    }

    pub(crate) async fn request_remote_socket_forward(&self, socket: &str) -> Result<()> {
        match self
            .inner
            .handle
            .streamlocal_forward(socket.to_owned())
            .await
        {
            Ok(()) => Ok(()),
            Err(russh::Error::RequestDenied) => Err(Error::Forward(format!(
                "the server refused to listen on {socket} (is AllowStreamLocalForwarding off?)"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) async fn cancel_remote_socket_forward(&self, socket: &str) -> Result<()> {
        match self
            .inner
            .handle
            .cancel_streamlocal_forward(socket.to_owned())
            .await
        {
            Ok(()) | Err(russh::Error::RequestDenied) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) async fn request_remote_forward(&self, bind: &str, port: u16) -> Result<u32> {
        match self
            .inner
            .handle
            .tcpip_forward(bind.to_owned(), u32::from(port))
            .await
        {
            Ok(0) => Ok(u32::from(port)),
            Ok(actual) => Ok(actual),
            Err(russh::Error::RequestDenied) => Err(Error::Forward(format!(
                "the server refused to listen on {bind}:{port}"
            ))),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) async fn cancel_remote_forward(&self, bind: &str, port: u32) -> Result<()> {
        match self
            .inner
            .handle
            .cancel_tcpip_forward(bind.to_owned(), port)
            .await
        {
            Ok(()) | Err(russh::Error::RequestDenied) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub(crate) fn remote_forwards(&self) -> &Arc<RemoteForwards> {
        &self.inner.forwards
    }
}

/// How long opening a channel may take before the connection is presumed
/// dead (keepalives would take minutes to notice).
pub const CHANNEL_OPEN_TIMEOUT: Duration = Duration::from_secs(15);

/// A channel-open failure reason in words.
fn refusal(reason: ChannelOpenFailure) -> String {
    match reason {
        ChannelOpenFailure::AdministrativelyProhibited => {
            "administratively prohibited (too many sessions on this connection?)".to_owned()
        }
        ChannelOpenFailure::ConnectFailed => "connect failed".to_owned(),
        ChannelOpenFailure::UnknownChannelType => "unknown channel type".to_owned(),
        ChannelOpenFailure::ResourceShortage => "resource shortage".to_owned(),
        ChannelOpenFailure::Other { code, reason } if reason.is_empty() => format!("code {code}"),
        ChannelOpenFailure::Other { reason, .. } => reason,
    }
}

async fn open_tcp(params: &ConnectParams) -> Result<TcpStream> {
    let address = (params.host.as_str(), params.port);
    let stream = tokio::time::timeout(params.connect_timeout, TcpStream::connect(address))
        .await
        .map_err(|_| {
            Error::Timeout(format!(
                "connecting to {}:{} took longer than {:?}",
                params.host, params.port, params.connect_timeout
            ))
        })?
        .map_err(|e| Error::Network(format!("{}:{}: {e}", params.host, params.port)))?;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_host_key_algorithms_are_offered_first() {
        let default = Preferred::DEFAULT.key;
        assert_eq!(host_key_preference(&[]), default);

        let preferred = host_key_preference(&[Algorithm::Rsa { hash: None }]);
        assert!(preferred.iter().take(3).all(|a| a.clone().is_rsa()));
        // Nothing is added or dropped.
        assert_eq!(preferred.len(), default.len());
        for algorithm in default.iter() {
            assert!(preferred.contains(algorithm));
        }
    }
}
