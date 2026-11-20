//! Port forwards: local (-L), remote (-R) and dynamic SOCKS5 (-D).

use crate::error::{Error, Result};
use crate::session::Session;
use crate::socks5;
use russh::client::{ChannelOpenHandle, Msg};
use russh::{Channel, ChannelOpenFailure};
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardKind {
    Local,
    Remote,
    Dynamic,
}

/// One end of a forward that is a TCP address or a Unix socket path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Endpoint {
    Tcp {
        host: String,
        port: u16,
    },
    /// A Unix domain socket: on the server for a local forward's target and
    /// a remote forward's listen side; on this machine for a local
    /// forward's listen side and a remote forward's target (Unix only).
    Unix(String),
}

impl Endpoint {
    pub fn tcp(host: &str, port: u16) -> Self {
        Endpoint::Tcp {
            host: host.to_owned(),
            port,
        }
    }
}

impl std::fmt::Display for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Tcp { host, port } => write!(f, "{host}:{port}"),
            Endpoint::Unix(path) => f.write_str(path),
        }
    }
}

/// What a remote forward registered on the server, to cancel on stop.
enum RemoteRegistration {
    Tcp(String, u32),
    Socket(String),
}

/// A running forward. Call [`Forward::stop`] to end it; dropping it also
/// stops local and dynamic forwards (a remote one stays registered on the
/// server until `stop` or until the connection ends).
pub struct Forward {
    kind: ForwardKind,
    local_addr: Option<SocketAddr>,
    remote: Option<(Session, RemoteRegistration)>,
    /// The socket file a local Unix listener made; removed on stop.
    socket_file: Option<PathBuf>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
}

impl Forward {
    pub fn kind(&self) -> ForwardKind {
        self.kind
    }

    /// The address a local or dynamic forward listens on.
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.local_addr
    }

    /// The port the server listens on for a remote forward.
    pub fn remote_port(&self) -> Option<u32> {
        match self.remote.as_ref() {
            Some((_, RemoteRegistration::Tcp(_, port))) => Some(*port),
            _ => None,
        }
    }

    /// Stops listening, closes the connections going through the forward
    /// and returns once the listener is gone.
    pub async fn stop(mut self) -> Result<()> {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        if let Some(path) = self.socket_file.take() {
            let _ = std::fs::remove_file(path);
        }
        match self.remote.take() {
            Some((session, RemoteRegistration::Tcp(bind, port))) => {
                session.remote_forwards().remove(&bind, port);
                session.cancel_remote_forward(&bind, port).await?;
            }
            Some((session, RemoteRegistration::Socket(path))) => {
                session.remote_forwards().remove_socket(&path);
                session.cancel_remote_socket_forward(&path).await?;
            }
            None => {}
        }
        Ok(())
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(path) = self.socket_file.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

/// Copies both ways until one side ends or the forward is stopped.
async fn bridge<A, B>(mut a: A, mut b: B, mut stop: watch::Receiver<bool>)
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    tokio::select! {
        _ = tokio::io::copy_bidirectional(&mut a, &mut b) => {}
        _ = stopped(&mut stop) => {}
    }
}

/// Where connections the server accepts for a remote forward go.
#[derive(Clone)]
pub(crate) struct RemoteTarget {
    target: Endpoint,
    stop: watch::Receiver<bool>,
}

impl RemoteTarget {
    pub(crate) async fn serve(self, channel: Channel<Msg>, reply: ChannelOpenHandle) {
        let limit = Duration::from_secs(10);
        match &self.target {
            Endpoint::Tcp { host, port } => {
                match tokio::time::timeout(limit, TcpStream::connect((host.as_str(), *port))).await
                {
                    Ok(Ok(tcp)) => {
                        reply.accept().await;
                        bridge(tcp, channel.into_stream(), self.stop).await;
                    }
                    _ => reply.reject(ChannelOpenFailure::ConnectFailed).await,
                }
            }
            #[cfg(unix)]
            Endpoint::Unix(path) => {
                match tokio::time::timeout(limit, tokio::net::UnixStream::connect(path)).await {
                    Ok(Ok(unix)) => {
                        reply.accept().await;
                        bridge(unix, channel.into_stream(), self.stop).await;
                    }
                    _ => reply.reject(ChannelOpenFailure::ConnectFailed).await,
                }
            }
            #[cfg(not(unix))]
            Endpoint::Unix(_) => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
    }
}

/// The remote forwards of one connection, by the address and port the
/// server listens on: one port can be forwarded on two addresses (say
/// `127.0.0.1:8080` and `10.0.0.5:8080`) to different targets.
#[derive(Default)]
pub(crate) struct RemoteForwards {
    targets: Mutex<HashMap<(String, u32), RemoteTarget>>,
    /// Remote forwards of a server socket path.
    sockets: Mutex<HashMap<String, RemoteTarget>>,
}

impl RemoteForwards {
    fn targets(&self) -> std::sync::MutexGuard<'_, HashMap<(String, u32), RemoteTarget>> {
        self.targets.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The target for a connection the server accepted on
    /// `address:port`. The address is the one the forward asked for; when
    /// the server names it differently, the only forward on that port is it.
    pub(crate) fn get(&self, address: &str, port: u32) -> Option<RemoteTarget> {
        let targets = self.targets();
        if let Some(target) = targets.get(&(address.to_owned(), port)) {
            return Some(target.clone());
        }
        let mut on_port = targets.iter().filter(|((_, p), _)| *p == port);
        match (on_port.next(), on_port.next()) {
            (Some((_, target)), None) => Some(target.clone()),
            _ => None,
        }
    }

    fn insert(&self, address: &str, port: u32, target: RemoteTarget) {
        self.targets().insert((address.to_owned(), port), target);
    }

    pub(crate) fn remove(&self, address: &str, port: u32) {
        self.targets().remove(&(address.to_owned(), port));
    }

    fn sockets(&self) -> std::sync::MutexGuard<'_, HashMap<String, RemoteTarget>> {
        self.sockets.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The target for a connection the server accepted on its socket `path`.
    pub(crate) fn get_socket(&self, path: &str) -> Option<RemoteTarget> {
        self.sockets().get(path).cloned()
    }

    fn insert_socket(&self, path: &str, target: RemoteTarget) {
        self.sockets().insert(path.to_owned(), target);
    }

    pub(crate) fn remove_socket(&self, path: &str) {
        self.sockets().remove(path);
    }
}

impl Session {
    /// `-L`: listens on `bind_host:bind_port` here and forwards each
    /// connection to `target_host:target_port` as seen from the server.
    /// Port 0 picks a free port (see [`Forward::local_addr`]).
    pub async fn forward_local(
        &self,
        bind_host: &str,
        bind_port: u16,
        target_host: &str,
        target_port: u16,
    ) -> Result<Forward> {
        self.forward_local_to(
            Endpoint::tcp(bind_host, bind_port),
            Endpoint::tcp(target_host, target_port),
        )
        .await
    }

    /// `-L` with either end a Unix socket: `listen` is where to listen here
    /// (a Unix socket path only where the platform has them) and `target`
    /// what the server connects to (`direct-streamlocal@openssh.com` for a
    /// socket path).
    pub async fn forward_local_to(&self, listen: Endpoint, target: Endpoint) -> Result<Forward> {
        let (stop_tx, stop_rx) = watch::channel(false);
        let session = self.clone();
        let serve = move |client: ClientStream, origin: SocketAddr, stop| {
            let session = session.clone();
            let target = target.clone();
            async move {
                match session.open_endpoint(&target, origin).await {
                    Ok(channel) => match client {
                        ClientStream::Tcp(tcp) => bridge(tcp, channel.into_stream(), stop).await,
                        #[cfg(unix)]
                        ClientStream::Unix(unix) => bridge(unix, channel.into_stream(), stop).await,
                    },
                    Err(e) => tracing::debug!("-L to {target} failed: {e}"),
                }
            }
        };
        match listen {
            Endpoint::Tcp { host, port } => {
                let listeners = listen_tcp(&host, port).await?;
                let local_addr = listeners[0].local_addr().ok();
                let task = tokio::spawn(accept_all(listeners, stop_rx, move |tcp, peer, stop| {
                    serve(ClientStream::Tcp(tcp), peer, stop)
                }));
                Ok(Forward {
                    kind: ForwardKind::Local,
                    local_addr,
                    remote: None,
                    socket_file: None,
                    stop: stop_tx,
                    task: Some(task),
                })
            }
            #[cfg(unix)]
            Endpoint::Unix(path) => {
                let listener = bind_unix(&path)?;
                let task = tokio::spawn(accept_unix(listener, stop_rx, move |unix, stop| {
                    let origin = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
                    serve(ClientStream::Unix(unix), origin, stop)
                }));
                Ok(Forward {
                    kind: ForwardKind::Local,
                    local_addr: None,
                    remote: None,
                    socket_file: Some(PathBuf::from(path)),
                    stop: stop_tx,
                    task: Some(task),
                })
            }
            #[cfg(not(unix))]
            Endpoint::Unix(_) => Err(Error::Forward(
                "listening on a Unix socket is not supported on this platform".into(),
            )),
        }
    }

    /// `-D`: a SOCKS5 proxy on `bind_host:bind_port` whose connections leave
    /// from the server.
    pub async fn forward_dynamic(&self, bind_host: &str, bind_port: u16) -> Result<Forward> {
        let listeners = listen_tcp(bind_host, bind_port).await?;
        let local_addr = listeners[0].local_addr().ok();
        let (stop_tx, stop_rx) = watch::channel(false);
        let session = self.clone();
        let task = tokio::spawn(accept_all(
            listeners,
            stop_rx,
            move |mut tcp, peer, stop| {
                let session = session.clone();
                async move {
                    let request = tokio::time::timeout(
                        Duration::from_secs(15),
                        socks5::accept_request(&mut tcp),
                    )
                    .await;
                    let request = match request {
                        Ok(Ok(request)) => request,
                        Ok(Err(e)) => {
                            tracing::debug!("-D handshake failed: {e}");
                            return;
                        }
                        Err(_) => return,
                    };
                    match session
                        .open_direct_tcpip(&request.host, request.port, peer)
                        .await
                    {
                        Ok(channel) => {
                            if socks5::reply_to(&mut tcp, &request, socks5::reply::SUCCEEDED)
                                .await
                                .is_ok()
                            {
                                bridge(tcp, channel.into_stream(), stop).await;
                            }
                        }
                        Err(e) => {
                            let code = match &e {
                                russh::Error::ChannelOpenFailure(
                                    ChannelOpenFailure::ConnectFailed,
                                ) => socks5::reply::CONNECTION_REFUSED,
                                russh::Error::ChannelOpenFailure(
                                    ChannelOpenFailure::AdministrativelyProhibited,
                                ) => socks5::reply::NOT_ALLOWED,
                                _ => socks5::reply::GENERAL_FAILURE,
                            };
                            let _ = socks5::reply_to(&mut tcp, &request, code).await;
                        }
                    }
                }
            },
        ));
        Ok(Forward {
            kind: ForwardKind::Dynamic,
            local_addr,
            remote: None,
            socket_file: None,
            stop: stop_tx,
            task: Some(task),
        })
    }

    /// `-R`: the server listens on `bind_host:bind_port` (port 0: the server
    /// picks, see [`Forward::remote_port`]) and each connection is forwarded
    /// to `target_host:target_port` as seen from this machine.
    pub async fn forward_remote(
        &self,
        bind_host: &str,
        bind_port: u16,
        target_host: &str,
        target_port: u16,
    ) -> Result<Forward> {
        self.forward_remote_to(
            Endpoint::tcp(bind_host, bind_port),
            Endpoint::tcp(target_host, target_port),
        )
        .await
    }

    /// `-R` with either end a Unix socket: `bind` is what the server
    /// listens on (`streamlocal-forward@openssh.com` for a socket path) and
    /// `target` what this machine connects to (a Unix socket path only
    /// where the platform has them).
    pub async fn forward_remote_to(&self, bind: Endpoint, target: Endpoint) -> Result<Forward> {
        let (stop_tx, stop_rx) = watch::channel(false);
        let target = RemoteTarget {
            target,
            stop: stop_rx,
        };
        let table = self.remote_forwards();
        let registration = match bind {
            Endpoint::Tcp {
                host: bind_host,
                port: bind_port,
            } => {
                // Registered first so no early connection finds nothing.
                if bind_port != 0 {
                    table.insert(&bind_host, u32::from(bind_port), target.clone());
                }
                let port = match self.request_remote_forward(&bind_host, bind_port).await {
                    Ok(port) => port,
                    Err(e) => {
                        table.remove(&bind_host, u32::from(bind_port));
                        return Err(e);
                    }
                };
                if bind_port == 0 {
                    table.insert(&bind_host, port, target);
                }
                RemoteRegistration::Tcp(bind_host, port)
            }
            Endpoint::Unix(path) => {
                table.insert_socket(&path, target);
                if let Err(e) = self.request_remote_socket_forward(&path).await {
                    table.remove_socket(&path);
                    return Err(e);
                }
                RemoteRegistration::Socket(path)
            }
        };
        Ok(Forward {
            kind: ForwardKind::Remote,
            local_addr: None,
            remote: Some((self.clone(), registration)),
            socket_file: None,
            stop: stop_tx,
            task: None,
        })
    }
}

/// A connection a local forward accepted.
enum ClientStream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
}

/// What to listen on for `host`: like OpenSSH, `localhost` is 127.0.0.1
/// and ::1, and `*` (or empty) every IPv4 and IPv6 address. The second
/// address is best effort (a machine without IPv6, a port taken there).
fn bind_addresses(host: &str) -> (&str, Option<Ipv6Addr>) {
    match host {
        "localhost" => ("127.0.0.1", Some(Ipv6Addr::LOCALHOST)),
        "*" | "" => ("0.0.0.0", Some(Ipv6Addr::UNSPECIFIED)),
        other => (other, None),
    }
}

/// Listens on `host:port` (see [`bind_addresses`]); the first listener is
/// the main one, and its port is the one the others use.
async fn listen_tcp(host: &str, port: u16) -> Result<Vec<TcpListener>> {
    let (first, second) = bind_addresses(host);
    let primary = TcpListener::bind((first, port))
        .await
        .map_err(|e| Error::Forward(format!("cannot listen on {host}:{port}: {e}")))?;
    let mut listeners = vec![primary];
    if let Some(ip) = second {
        let port = listeners[0].local_addr().map_or(port, |a| a.port());
        match listen_v6_only(SocketAddr::from((ip, port))) {
            Ok(listener) => listeners.push(listener),
            Err(e) => tracing::debug!("not listening on [{ip}]:{port} too: {e}"),
        }
    }
    Ok(listeners)
}

/// An IPv6 listener that leaves IPv4 to the other one (a dual-stack socket
/// would clash with it on the same port).
fn listen_v6_only(address: SocketAddr) -> std::io::Result<TcpListener> {
    use socket2::{Domain, Protocol, Socket, Type};
    let socket = Socket::new(Domain::IPV6, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_only_v6(true)?;
    // As tokio's own bind does: a restarted forward can take its port back
    // while old connections linger in TIME_WAIT.
    #[cfg(not(windows))]
    socket.set_reuse_address(true)?;
    socket.bind(&address.into())?;
    socket.listen(1024)?;
    socket.set_nonblocking(true)?;
    TcpListener::from_std(socket.into())
}

/// Binds a Unix socket at `path`. A socket file left by a crashed run (one
/// nothing answers on) is replaced; anything else in the way is an error.
#[cfg(unix)]
fn bind_unix(path: &str) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::FileTypeExt;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let stale =
            meta.file_type().is_socket() && std::os::unix::net::UnixStream::connect(path).is_err();
        if !stale {
            return Err(Error::Forward(format!(
                "cannot listen on {path}: the path is already in use"
            )));
        }
        let _ = std::fs::remove_file(path);
    }
    tokio::net::UnixListener::bind(path)
        .map_err(|e| Error::Forward(format!("cannot listen on {path}: {e}")))
}

#[cfg(unix)]
async fn accept_unix<F, Fut>(
    listener: tokio::net::UnixListener,
    mut stop: watch::Receiver<bool>,
    handler: F,
) where
    F: Fn(tokio::net::UnixStream, watch::Receiver<bool>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => break,
            accepted = listener.accept() => match accepted {
                Ok((unix, _)) => {
                    tokio::spawn(handler(unix, stop.clone()));
                }
                Err(e) => {
                    tracing::debug!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
}

/// Runs [`accept_loop`] on every listener until the forward stops.
async fn accept_all<F, Fut>(listeners: Vec<TcpListener>, stop: watch::Receiver<bool>, handler: F)
where
    F: Fn(TcpStream, SocketAddr, watch::Receiver<bool>) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let mut loops = tokio::task::JoinSet::new();
    for listener in listeners {
        loops.spawn(accept_loop(listener, stop.clone(), handler.clone()));
    }
    while loops.join_next().await.is_some() {}
}

async fn accept_loop<F, Fut>(listener: TcpListener, mut stop: watch::Receiver<bool>, handler: F)
where
    F: Fn(TcpStream, SocketAddr, watch::Receiver<bool>) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    loop {
        tokio::select! {
            _ = stopped(&mut stop) => break,
            accepted = listener.accept() => match accepted {
                Ok((tcp, peer)) => {
                    let _ = tcp.set_nodelay(true);
                    tokio::spawn(handler(tcp, peer, stop.clone()));
                }
                Err(e) => {
                    tracing::debug!("accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(host: &str) -> RemoteTarget {
        RemoteTarget {
            target: Endpoint::tcp(host, 1),
            stop: watch::channel(false).1,
        }
    }

    #[test]
    fn remote_targets_are_found_by_address_and_port() {
        let table = RemoteForwards::default();
        table.insert("127.0.0.1", 8080, target("a"));
        table.insert("10.0.0.5", 8080, target("b"));
        table.insert("localhost", 9090, target("c"));
        assert_eq!(
            table.get("127.0.0.1", 8080).unwrap().target,
            Endpoint::tcp("a", 1)
        );
        assert_eq!(
            table.get("10.0.0.5", 8080).unwrap().target,
            Endpoint::tcp("b", 1)
        );
        // Two forwards on 8080: an address that is neither is not guessed.
        assert!(table.get("192.168.1.1", 8080).is_none());
        // One forward on 9090: the server's name for the address may differ.
        assert_eq!(
            table.get("127.0.0.1", 9090).unwrap().target,
            Endpoint::tcp("c", 1)
        );
        // After one of the two is removed, the other is the only one on 8080.
        table.remove("127.0.0.1", 8080);
        assert_eq!(
            table.get("10.0.0.5", 8080).unwrap().target,
            Endpoint::tcp("b", 1)
        );
        assert_eq!(
            table.get("127.0.0.1", 8080).unwrap().target,
            Endpoint::tcp("b", 1)
        );
        assert!(table.get("x", 1).is_none());
    }

    #[test]
    fn remote_socket_targets_are_found_by_path() {
        let table = RemoteForwards::default();
        table.insert_socket("/run/a.sock", target("a"));
        assert_eq!(
            table.get_socket("/run/a.sock").unwrap().target,
            Endpoint::tcp("a", 1)
        );
        assert!(table.get_socket("/run/b.sock").is_none());
        table.remove_socket("/run/a.sock");
        assert!(table.get_socket("/run/a.sock").is_none());
        assert_eq!(Endpoint::Unix("/x".into()).to_string(), "/x");
        assert_eq!(Endpoint::tcp("h", 2).to_string(), "h:2");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_local_socket_path_is_bound_and_a_stale_file_is_replaced() {
        let dir = std::env::temp_dir().join(format!("easyssh-fwd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("l.sock");
        let path = path.to_str().unwrap();
        let first = bind_unix(path).unwrap();
        // Something answers on it: not replaced.
        assert!(bind_unix(path).is_err());
        drop(first);
        // The file stays after a crash, nothing answers: replaced.
        assert!(std::path::Path::new(path).exists());
        let _again = bind_unix(path).unwrap();
        // A regular file is never replaced.
        let plain = dir.join("plain");
        std::fs::write(&plain, b"x").unwrap();
        assert!(bind_unix(plain.to_str().unwrap()).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn localhost_and_the_wildcard_mean_both_address_families() {
        assert_eq!(
            bind_addresses("localhost"),
            ("127.0.0.1", Some(Ipv6Addr::LOCALHOST))
        );
        assert_eq!(
            bind_addresses("*"),
            ("0.0.0.0", Some(Ipv6Addr::UNSPECIFIED))
        );
        assert_eq!(bind_addresses("127.0.0.1"), ("127.0.0.1", None));
        assert_eq!(bind_addresses("::1"), ("::1", None));
    }

    #[tokio::test]
    async fn localhost_listens_on_one_port_for_both_families() {
        let listeners = listen_tcp("localhost", 0).await.unwrap();
        let first = listeners[0].local_addr().unwrap();
        assert_eq!(first.ip(), std::net::Ipv4Addr::LOCALHOST);
        assert_ne!(first.port(), 0);
        // Where the machine has IPv6, the same port listens on ::1 as well.
        if let Some(second) = listeners.get(1) {
            let second = second.local_addr().unwrap();
            assert_eq!(second.ip(), Ipv6Addr::LOCALHOST);
            assert_eq!(second.port(), first.port());
            TcpStream::connect(second).await.unwrap();
        }
        TcpStream::connect(first).await.unwrap();
        // An address of its own is only that address.
        assert_eq!(listen_tcp("127.0.0.1", 0).await.unwrap().len(), 1);
    }
}
