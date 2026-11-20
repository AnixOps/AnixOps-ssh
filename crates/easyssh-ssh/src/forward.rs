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

/// A running forward. Call [`Forward::stop`] to end it; dropping it also
/// stops local and dynamic forwards (a remote one stays registered on the
/// server until `stop` or until the connection ends).
pub struct Forward {
    kind: ForwardKind,
    local_addr: Option<SocketAddr>,
    remote: Option<(Session, String, u32)>,
    /// A remote forward of a Unix socket on the server (`-R /path:...`).
    remote_socket: Option<(Session, String)>,
    /// The socket file a local forward listens on, removed when it stops.
    local_socket: Option<PathBuf>,
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
        self.remote.as_ref().map(|(_, _, port)| *port)
    }

    /// The path a local forward listens on when it is a Unix socket.
    pub fn local_socket(&self) -> Option<&std::path::Path> {
        self.local_socket.as_deref()
    }

    /// Stops listening, closes the connections going through the forward
    /// and returns once the listener is gone.
    pub async fn stop(mut self) -> Result<()> {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        if let Some((session, bind, port)) = self.remote.take() {
            session.remote_forwards().remove(&bind, port);
            session.cancel_remote_forward(&bind, port).await?;
        }
        if let Some((session, path)) = self.remote_socket.take() {
            session.remote_forwards().remove_socket(&path);
            session.cancel_remote_socket_forward(&path).await?;
        }
        if let Some(path) = self.local_socket.take() {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(path) = self.local_socket.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Where a forwarded connection ends up as seen from the server (`-L`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dest {
    Tcp {
        host: String,
        port: u16,
    },
    /// A Unix domain socket path on the server (`direct-streamlocal`).
    Socket(String),
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
    host: String,
    port: u16,
    /// A Unix socket on this machine instead of `host:port`.
    unix: Option<PathBuf>,
    stop: watch::Receiver<bool>,
}

impl RemoteTarget {
    pub(crate) async fn serve(self, channel: Channel<Msg>, reply: ChannelOpenHandle) {
        if let Some(path) = &self.unix {
            #[cfg(unix)]
            {
                let connect = tokio::net::UnixStream::connect(path);
                match tokio::time::timeout(Duration::from_secs(10), connect).await {
                    Ok(Ok(stream)) => {
                        reply.accept().await;
                        bridge(stream, channel.into_stream(), self.stop).await;
                    }
                    _ => reply.reject(ChannelOpenFailure::ConnectFailed).await,
                }
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
            }
            return;
        }
        let connect = TcpStream::connect((self.host.as_str(), self.port));
        match tokio::time::timeout(Duration::from_secs(10), connect).await {
            Ok(Ok(tcp)) => {
                reply.accept().await;
                bridge(tcp, channel.into_stream(), self.stop).await;
            }
            _ => reply.reject(ChannelOpenFailure::ConnectFailed).await,
        }
    }
}

/// The remote forwards of one connection, by the address and port the
/// server listens on: one port can be forwarded on two addresses (say
/// `127.0.0.1:8080` and `10.0.0.5:8080`) to different targets.
#[derive(Default)]
pub(crate) struct RemoteForwards {
    targets: Mutex<HashMap<(String, u32), RemoteTarget>>,
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

    pub(crate) fn get_socket(&self, path: &str) -> Option<RemoteTarget> {
        let sockets = self.sockets.lock().unwrap_or_else(|p| p.into_inner());
        sockets.get(path).cloned()
    }

    fn insert_socket(&self, path: &str, target: RemoteTarget) {
        let mut sockets = self.sockets.lock().unwrap_or_else(|p| p.into_inner());
        sockets.insert(path.to_owned(), target);
    }

    pub(crate) fn remove_socket(&self, path: &str) {
        let mut sockets = self.sockets.lock().unwrap_or_else(|p| p.into_inner());
        sockets.remove(path);
    }

    fn insert(&self, address: &str, port: u32, target: RemoteTarget) {
        self.targets().insert((address.to_owned(), port), target);
    }

    pub(crate) fn remove(&self, address: &str, port: u32) {
        self.targets().remove(&(address.to_owned(), port));
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
        let listeners = listen(bind_host, bind_port).await?;
        let local_addr = listeners[0].local_addr().ok();
        let (stop_tx, stop_rx) = watch::channel(false);
        let session = self.clone();
        let target_host = target_host.to_owned();
        let task = tokio::spawn(accept_all(listeners, stop_rx, move |tcp, peer, stop| {
            let session = session.clone();
            let target_host = target_host.clone();
            async move {
                match session
                    .open_direct_tcpip(&target_host, target_port, peer)
                    .await
                {
                    Ok(channel) => bridge(tcp, channel.into_stream(), stop).await,
                    Err(e) => tracing::debug!("-L to {target_host}:{target_port} failed: {e}"),
                }
            }
        }));
        Ok(Forward {
            kind: ForwardKind::Local,
            local_addr,
            remote: None,
            remote_socket: None,
            local_socket: None,
            stop: stop_tx,
            task: Some(task),
        })
    }

    /// `-L` with a Unix socket on either side: listens on `bind_socket`
    /// (a path here) when given, else on `bind_host:bind_port`, and forwards
    /// each connection to `dest` as seen from the server.
    pub async fn forward_local_to(
        &self,
        bind_socket: Option<&str>,
        bind_host: &str,
        bind_port: u16,
        dest: Dest,
    ) -> Result<Forward> {
        let Some(path) = bind_socket else {
            return match dest {
                Dest::Tcp { host, port } => {
                    self.forward_local(bind_host, bind_port, &host, port).await
                }
                Dest::Socket(socket) => {
                    let listeners = listen(bind_host, bind_port).await?;
                    let local_addr = listeners[0].local_addr().ok();
                    let (stop_tx, stop_rx) = watch::channel(false);
                    let session = self.clone();
                    let task = tokio::spawn(accept_all(listeners, stop_rx, move |tcp, _, stop| {
                        let session = session.clone();
                        let socket = socket.clone();
                        async move { serve_socket(session, tcp, &socket, stop).await }
                    }));
                    Ok(Forward {
                        kind: ForwardKind::Local,
                        local_addr,
                        remote: None,
                        remote_socket: None,
                        local_socket: None,
                        stop: stop_tx,
                        task: Some(task),
                    })
                }
            };
        };
        #[cfg(unix)]
        {
            let listener = bind_unix(path)?;
            let (stop_tx, mut stop) = watch::channel(false);
            let session = self.clone();
            let task = tokio::spawn(async move {
                let stop_rx = stop.clone();
                loop {
                    tokio::select! {
                        _ = stopped(&mut stop) => break,
                        accepted = listener.accept() => match accepted {
                            Ok((stream, _)) => {
                                let session = session.clone();
                                let dest = dest.clone();
                                let stop = stop_rx.clone();
                                tokio::spawn(async move {
                                    serve_dest(session, stream, &dest, stop).await
                                });
                            }
                            Err(e) => {
                                tracing::debug!("accept failed: {e}");
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                        },
                    }
                }
            });
            Ok(Forward {
                kind: ForwardKind::Local,
                local_addr: None,
                remote: None,
                remote_socket: None,
                local_socket: Some(PathBuf::from(path)),
                stop: stop_tx,
                task: Some(task),
            })
        }
        #[cfg(not(unix))]
        {
            let _ = (path, dest);
            Err(Error::Unsupported(
                "listening on a Unix socket needs OpenSSH on this platform".into(),
            ))
        }
    }

    /// `-R` with a Unix socket on either side: the server listens on
    /// `bind_socket` (a path there) when given, else on `bind_host:bind_port`,
    /// and each connection goes to `local_socket` (a path here) when given,
    /// else to `target_host:target_port`.
    pub async fn forward_remote_to(
        &self,
        bind_socket: Option<&str>,
        bind_host: &str,
        bind_port: u16,
        target_host: &str,
        target_port: u16,
        local_socket: Option<&str>,
    ) -> Result<Forward> {
        if bind_socket.is_none() && local_socket.is_none() {
            return self
                .forward_remote(bind_host, bind_port, target_host, target_port)
                .await;
        }
        #[cfg(not(unix))]
        if local_socket.is_some() {
            return Err(Error::Unsupported(
                "connecting to a Unix socket needs OpenSSH on this platform".into(),
            ));
        }
        let (stop_tx, stop_rx) = watch::channel(false);
        let target = RemoteTarget {
            host: target_host.to_owned(),
            port: target_port,
            unix: local_socket.map(PathBuf::from),
            stop: stop_rx,
        };
        let table = self.remote_forwards();
        if let Some(path) = bind_socket {
            table.insert_socket(path, target);
            if let Err(e) = self.request_remote_socket_forward(path).await {
                table.remove_socket(path);
                return Err(e);
            }
            return Ok(Forward {
                kind: ForwardKind::Remote,
                local_addr: None,
                remote: None,
                remote_socket: Some((self.clone(), path.to_owned())),
                local_socket: None,
                stop: stop_tx,
                task: None,
            });
        }
        if bind_port != 0 {
            table.insert(bind_host, u32::from(bind_port), target.clone());
        }
        let port = match self.request_remote_forward(bind_host, bind_port).await {
            Ok(port) => port,
            Err(e) => {
                table.remove(bind_host, u32::from(bind_port));
                return Err(e);
            }
        };
        if bind_port == 0 {
            table.insert(bind_host, port, target);
        }
        Ok(Forward {
            kind: ForwardKind::Remote,
            local_addr: None,
            remote: Some((self.clone(), bind_host.to_owned(), port)),
            remote_socket: None,
            local_socket: None,
            stop: stop_tx,
            task: None,
        })
    }

    /// `-D`: a SOCKS5 proxy on `bind_host:bind_port` whose connections leave
    /// from the server.
    pub async fn forward_dynamic(&self, bind_host: &str, bind_port: u16) -> Result<Forward> {
        let listeners = listen(bind_host, bind_port).await?;
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
            remote_socket: None,
            local_socket: None,
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
        let (stop_tx, stop_rx) = watch::channel(false);
        let target = RemoteTarget {
            host: target_host.to_owned(),
            port: target_port,
            unix: None,
            stop: stop_rx,
        };
        let table = self.remote_forwards();
        // Registered first so no early connection finds nothing.
        if bind_port != 0 {
            table.insert(bind_host, u32::from(bind_port), target.clone());
        }
        let port = match self.request_remote_forward(bind_host, bind_port).await {
            Ok(port) => port,
            Err(e) => {
                table.remove(bind_host, u32::from(bind_port));
                return Err(e);
            }
        };
        if bind_port == 0 {
            table.insert(bind_host, port, target);
        }
        Ok(Forward {
            kind: ForwardKind::Remote,
            local_addr: None,
            remote: Some((self.clone(), bind_host.to_owned(), port)),
            remote_socket: None,
            local_socket: None,
            stop: stop_tx,
            task: None,
        })
    }
}

/// Opens the channel for one accepted connection and copies both ways.
#[cfg(unix)]
async fn serve_dest<S>(session: Session, stream: S, dest: &Dest, stop: watch::Receiver<bool>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match dest {
        Dest::Socket(socket) => serve_socket(session, stream, socket, stop).await,
        Dest::Tcp { host, port } => {
            let origin = SocketAddr::from(([127, 0, 0, 1], 0));
            match session.open_direct_tcpip(host, *port, origin).await {
                Ok(channel) => bridge(stream, channel.into_stream(), stop).await,
                Err(e) => tracing::debug!("-L to {host}:{port} failed: {e}"),
            }
        }
    }
}

async fn serve_socket<S>(session: Session, stream: S, socket: &str, stop: watch::Receiver<bool>)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    match session.open_direct_streamlocal(socket).await {
        Ok(channel) => bridge(stream, channel.into_stream(), stop).await,
        Err(e) => tracing::debug!("-L to {socket} failed: {e}"),
    }
}

/// Binds a listening socket like OpenSSH: a stale socket file left by a
/// crashed run is replaced, a live one is an error.
#[cfg(unix)]
fn bind_unix(path: &str) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::FileTypeExt;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_socket() {
            if std::os::unix::net::UnixStream::connect(path).is_ok() {
                return Err(Error::Forward(format!("{path} is already in use")));
            }
            let _ = std::fs::remove_file(path);
        }
    }
    let listener = tokio::net::UnixListener::bind(path)
        .map_err(|e| Error::Forward(format!("cannot listen on {path}: {e}")))?;
    // Like OpenSSH (umask 077 for its sockets): only the owner connects.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    Ok(listener)
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
async fn listen(host: &str, port: u16) -> Result<Vec<TcpListener>> {
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
            host: host.into(),
            port: 1,
            unix: None,
            stop: watch::channel(false).1,
        }
    }

    #[test]
    fn remote_targets_are_found_by_address_and_port() {
        let table = RemoteForwards::default();
        table.insert("127.0.0.1", 8080, target("a"));
        table.insert("10.0.0.5", 8080, target("b"));
        table.insert("localhost", 9090, target("c"));
        assert_eq!(table.get("127.0.0.1", 8080).unwrap().host, "a");
        assert_eq!(table.get("10.0.0.5", 8080).unwrap().host, "b");
        // Two forwards on 8080: an address that is neither is not guessed.
        assert!(table.get("192.168.1.1", 8080).is_none());
        // One forward on 9090: the server's name for the address may differ.
        assert_eq!(table.get("127.0.0.1", 9090).unwrap().host, "c");
        // After one of the two is removed, the other is the only one on 8080.
        table.remove("127.0.0.1", 8080);
        assert_eq!(table.get("10.0.0.5", 8080).unwrap().host, "b");
        assert_eq!(table.get("127.0.0.1", 8080).unwrap().host, "b");
        assert!(table.get("x", 1).is_none());
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
        let listeners = listen("localhost", 0).await.unwrap();
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
        assert_eq!(listen("127.0.0.1", 0).await.unwrap().len(), 1);
    }
}
