//! Port forwards: local (-L), remote (-R) and dynamic SOCKS5 (-D).

use crate::error::{Error, Result};
use crate::session::Session;
use crate::socks5;
use russh::client::{ChannelOpenHandle, Msg};
use russh::{Channel, ChannelOpenFailure};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
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

/// Connections a forward accepted but could not pass on: how many, and why
/// the last one failed ("127.0.0.1:1: connect failed", "administratively
/// prohibited", ...). The forward itself keeps running.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ForwardFailures {
    pub count: u64,
    pub last: Option<String>,
}

type Failures = Arc<watch::Sender<ForwardFailures>>;

fn record(failures: &watch::Sender<ForwardFailures>, message: String) {
    tracing::debug!("forward: {message}");
    failures.send_modify(|f| {
        f.count += 1;
        f.last = Some(message);
    });
}

/// Why the server would not open a channel to `host:port`, in a few words.
fn open_failure(host: &str, port: u16, error: &russh::Error) -> String {
    let reason = match error {
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed) => {
            "connect failed".to_owned()
        }
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited) => {
            "administratively prohibited".to_owned()
        }
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::ResourceShortage) => {
            "the server is short of resources".to_owned()
        }
        russh::Error::ChannelOpenFailure(ChannelOpenFailure::UnknownChannelType) => {
            "the server does not forward TCP".to_owned()
        }
        other => other.to_string(),
    };
    format!("{} {reason}", target_label(host, port))
}

fn target_label(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}:")
    } else {
        format!("{host}:{port}:")
    }
}

/// A running forward. Call [`Forward::stop`] to end it; dropping it also
/// stops local and dynamic forwards (a remote one stays registered on the
/// server until `stop` or until the connection ends).
pub struct Forward {
    kind: ForwardKind,
    local_addr: Option<SocketAddr>,
    remote: Option<(Session, String, u32)>,
    stop: watch::Sender<bool>,
    task: Option<JoinHandle<()>>,
    failures: Failures,
}

impl Forward {
    /// Connections that could not be passed on so far; changes as they happen.
    pub fn failures(&self) -> watch::Receiver<ForwardFailures> {
        self.failures.subscribe()
    }

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

    /// Stops listening, closes the connections going through the forward
    /// and returns once the listener is gone.
    pub async fn stop(mut self) -> Result<()> {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
        if let Some((session, bind, port)) = self.remote.take() {
            session.remote_forwards().remove(port);
            session.cancel_remote_forward(&bind, port).await?;
        }
        Ok(())
    }
}

impl Drop for Forward {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
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
    host: String,
    port: u16,
    stop: watch::Receiver<bool>,
    failures: Failures,
}

impl RemoteTarget {
    pub(crate) async fn serve(self, channel: Channel<Msg>, reply: ChannelOpenHandle) {
        let connect = TcpStream::connect((self.host.as_str(), self.port));
        let reason = match tokio::time::timeout(Duration::from_secs(10), connect).await {
            Ok(Ok(tcp)) => {
                reply.accept().await;
                bridge(tcp, channel.into_stream(), self.stop).await;
                return;
            }
            Ok(Err(e)) => e.to_string(),
            Err(_) => "timed out".to_owned(),
        };
        record(
            &self.failures,
            format!("{} {reason}", target_label(&self.host, self.port)),
        );
        reply.reject(ChannelOpenFailure::ConnectFailed).await;
    }
}

/// The remote forwards of one connection, by the port the server listens on.
#[derive(Default)]
pub(crate) struct RemoteForwards {
    targets: Mutex<HashMap<u32, RemoteTarget>>,
}

impl RemoteForwards {
    pub(crate) fn get(&self, port: u32) -> Option<RemoteTarget> {
        self.targets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&port)
            .cloned()
    }

    fn insert(&self, port: u32, target: RemoteTarget) {
        self.targets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(port, target);
    }

    pub(crate) fn remove(&self, port: u32) {
        self.targets
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&port);
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
        let listener = listen(bind_host, bind_port).await?;
        let local_addr = listener.local_addr().ok();
        let (stop_tx, stop_rx) = watch::channel(false);
        let failures: Failures = Arc::new(watch::channel(ForwardFailures::default()).0);
        let session = self.clone();
        let target_host = target_host.to_owned();
        let failed = failures.clone();
        let task = tokio::spawn(accept_loop(listener, stop_rx, move |tcp, peer, stop| {
            let session = session.clone();
            let target_host = target_host.clone();
            let failed = failed.clone();
            async move {
                match session
                    .open_direct_tcpip(&target_host, target_port, peer)
                    .await
                {
                    Ok(channel) => bridge(tcp, channel.into_stream(), stop).await,
                    Err(e) => record(&failed, open_failure(&target_host, target_port, &e)),
                }
            }
        }));
        Ok(Forward {
            kind: ForwardKind::Local,
            local_addr,
            remote: None,
            stop: stop_tx,
            task: Some(task),
            failures,
        })
    }

    /// `-D`: a SOCKS5 proxy on `bind_host:bind_port` whose connections leave
    /// from the server.
    pub async fn forward_dynamic(&self, bind_host: &str, bind_port: u16) -> Result<Forward> {
        let listener = listen(bind_host, bind_port).await?;
        let local_addr = listener.local_addr().ok();
        let (stop_tx, stop_rx) = watch::channel(false);
        let failures: Failures = Arc::new(watch::channel(ForwardFailures::default()).0);
        let session = self.clone();
        let failed = failures.clone();
        let task = tokio::spawn(accept_loop(
            listener,
            stop_rx,
            move |mut tcp, peer, stop| {
                let session = session.clone();
                let failed = failed.clone();
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
                            if socks5::send_reply(&mut tcp, socks5::reply::SUCCEEDED)
                                .await
                                .is_ok()
                            {
                                bridge(tcp, channel.into_stream(), stop).await;
                            }
                        }
                        Err(e) => {
                            record(&failed, open_failure(&request.host, request.port, &e));
                            let code = match &e {
                                russh::Error::ChannelOpenFailure(
                                    ChannelOpenFailure::ConnectFailed,
                                ) => socks5::reply::CONNECTION_REFUSED,
                                russh::Error::ChannelOpenFailure(
                                    ChannelOpenFailure::AdministrativelyProhibited,
                                ) => socks5::reply::NOT_ALLOWED,
                                _ => socks5::reply::GENERAL_FAILURE,
                            };
                            let _ = socks5::send_reply(&mut tcp, code).await;
                        }
                    }
                }
            },
        ));
        Ok(Forward {
            kind: ForwardKind::Dynamic,
            local_addr,
            remote: None,
            stop: stop_tx,
            task: Some(task),
            failures,
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
        let failures: Failures = Arc::new(watch::channel(ForwardFailures::default()).0);
        let target = RemoteTarget {
            host: target_host.to_owned(),
            port: target_port,
            stop: stop_rx,
            failures: failures.clone(),
        };
        let table = self.remote_forwards();
        // Registered first so no early connection finds nothing.
        if bind_port != 0 {
            table.insert(u32::from(bind_port), target.clone());
        }
        let port = match self.request_remote_forward(bind_host, bind_port).await {
            Ok(port) => port,
            Err(e) => {
                table.remove(u32::from(bind_port));
                return Err(e);
            }
        };
        if bind_port == 0 {
            table.insert(port, target);
        }
        Ok(Forward {
            kind: ForwardKind::Remote,
            local_addr: None,
            remote: Some((self.clone(), bind_host.to_owned(), port)),
            stop: stop_tx,
            task: None,
            failures,
        })
    }
}

async fn listen(host: &str, port: u16) -> Result<TcpListener> {
    TcpListener::bind((host, port))
        .await
        .map_err(|e| Error::Forward(format!("cannot listen on {host}:{port}: {e}")))
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

    #[test]
    fn failures_are_counted_with_the_last_reason() {
        let (tx, rx) = watch::channel(ForwardFailures::default());
        record(
            &tx,
            open_failure(
                "127.0.0.1",
                1,
                &russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed),
            ),
        );
        record(
            &tx,
            open_failure(
                "::1",
                22,
                &russh::Error::ChannelOpenFailure(ChannelOpenFailure::AdministrativelyProhibited),
            ),
        );
        let seen = rx.borrow().clone();
        assert_eq!(seen.count, 2);
        assert_eq!(
            seen.last.as_deref(),
            Some("[::1]:22: administratively prohibited")
        );
        assert_eq!(
            open_failure(
                "db",
                5432,
                &russh::Error::ChannelOpenFailure(ChannelOpenFailure::ConnectFailed)
            ),
            "db:5432: connect failed"
        );
    }
}
