use crate::channel::Channel;
use crate::error::{Error, Result};
use crate::runtime::{self, InstanceLock, Paths};
use crate::session::Secret;
use crate::transport::Listener;
use serde_json::{json, Value};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Notify, Semaphore};

/// Called with each authenticated message; `Some(reply)` is sent back.
pub type Handler =
    Arc<dyn Fn(Value) -> Pin<Box<dyn Future<Output = Option<Value>> + Send>> + Send + Sync>;

/// Wrap an async closure as a [`Handler`].
pub fn handler<F, Fut>(f: F) -> Handler
where
    F: Fn(Value) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Option<Value>> + Send + 'static,
{
    Arc::new(move |v| Box::pin(f(v)))
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Frames queued for one connection before it counts as too slow to follow.
const OUTBOX_CAPACITY: usize = 4096;

/// The sending side of one connection, for replies and pushed events.
///
/// Everything goes through one ordered queue. A client that stops reading
/// fills the queue; the next push then closes the connection instead of
/// letting memory grow without bound (the client can reconnect and replay).
#[derive(Clone)]
pub struct Outbox {
    tx: mpsc::Sender<Value>,
    /// Next event `seq`; held across the push so numbering follows queue order.
    seq: Arc<Mutex<u64>>,
    close: Arc<Notify>,
}

impl Outbox {
    fn new(tx: mpsc::Sender<Value>, close: Arc<Notify>) -> Self {
        Outbox {
            tx,
            seq: Arc::new(Mutex::new(0)),
            close,
        }
    }

    /// Queue a message, waiting for room. `false` when the connection is gone.
    pub async fn send(&self, msg: Value) -> bool {
        self.tx.send(msg).await.is_ok()
    }

    /// Queue a message without waiting. A full queue closes the connection.
    /// `false` when the message was not queued.
    pub fn try_send(&self, msg: Value) -> bool {
        match self.tx.try_send(msg) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.close.notify_one();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// Whether the queue is more than three quarters full: the client is
    /// behind. Session output waits (and so slows the program down, as a
    /// terminal does) instead of filling the queue and losing the client.
    pub fn is_congested(&self) -> bool {
        self.tx.capacity() < OUTBOX_CAPACITY / 4
    }

    /// Queue an `Event` envelope with the next per-connection `seq`.
    pub fn event(&self, kind: &str, payload: Value) -> bool {
        let mut seq = self.seq.lock().unwrap_or_else(|p| p.into_inner());
        let ok = self.try_send(json!({"kind": kind, "payload": payload, "seq": *seq}));
        if ok {
            *seq += 1;
        }
        ok
    }

    /// Ask the server to drop this connection.
    pub fn close(&self) {
        self.close.notify_one();
    }
}

/// What a server does with connections. Messages of one connection are
/// handled one at a time, in order; connections run concurrently.
pub trait Service: Send + Sync + 'static {
    /// A client authenticated. `id` is unique for the daemon's lifetime.
    fn connected(&self, id: u64, outbox: &Outbox);
    /// The connection ended (the outbox no longer delivers anything).
    fn disconnected(&self, id: u64);
    /// An authenticated message arrived. Replies and events go to `outbox`.
    fn handle<'a>(&'a self, id: u64, outbox: &'a Outbox, msg: Value) -> BoxFuture<'a, ()>;
}

/// Adapts a request/reply [`Handler`] to a [`Service`].
struct HandlerService(Handler);

impl Service for HandlerService {
    fn connected(&self, _id: u64, _outbox: &Outbox) {}
    fn disconnected(&self, _id: u64) {}
    fn handle<'a>(&'a self, _id: u64, outbox: &'a Outbox, msg: Value) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(reply) = (self.0)(msg).await {
                outbox.send(reply).await;
            }
        })
    }
}

const MAX_CONNECTIONS: usize = 64;

/// A bound, locked and ready daemon endpoint. Dropping it removes the socket,
/// secret and lock files.
pub struct Server {
    paths: Paths,
    secret: Secret,
    listener: Listener,
    service: Arc<dyn Service>,
    next_id: Arc<AtomicU64>,
    _lock: InstanceLock,
}

impl Server {
    /// Prepare the runtime directory, take the single-instance lock, publish a
    /// fresh secret and bind the endpoint.
    pub fn start(runtime_dir: impl Into<PathBuf>, handler: Handler) -> Result<Server> {
        Self::start_service(runtime_dir, Arc::new(HandlerService(handler)))
    }

    /// Like [`Server::start`], for a [`Service`] that can push events.
    pub fn start_service(
        runtime_dir: impl Into<PathBuf>,
        service: Arc<dyn Service>,
    ) -> Result<Server> {
        let paths = Paths::new(runtime_dir)?;
        runtime::prepare_dir(&paths)?;
        let lock = InstanceLock::acquire(&paths)?;
        // From here on we own the directory; clean up if anything below fails.
        let mut guard = CleanupOnDrop(Some(paths.clone()));
        let secret = runtime::write_new_secret(&paths)?;
        let listener = Listener::bind(&paths)?;
        guard.0 = None;
        Ok(Server {
            paths,
            secret,
            listener,
            service,
            next_id: Arc::new(AtomicU64::new(1)),
            _lock: lock,
        })
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Accept connections until `shutdown` completes.
    pub async fn serve(mut self, shutdown: impl Future<Output = ()>) -> Result<()> {
        let limit = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => return Ok(()),
                accepted = self.listener.accept() => {
                    let conn = match accepted {
                        Ok(c) => c,
                        // A rejected peer or a transient accept error must not stop the daemon.
                        Err(_) => continue,
                    };
                    let Ok(permit) = limit.clone().try_acquire_owned() else {
                        continue; // over the limit: drop the connection
                    };
                    let secret = self.secret;
                    let service = self.service.clone();
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed);
                    tokio::spawn(async move {
                        let _permit = permit;
                        let _ = serve_connection(conn, &secret, service, id).await;
                    });
                }
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Still holding the instance lock here: it is released after this runs.
        runtime::cleanup(&self.paths);
    }
}

async fn serve_connection(
    conn: crate::transport::Conn,
    secret: &Secret,
    service: Arc<dyn Service>,
    id: u64,
) -> Result<()> {
    let (ch, first) = Channel::server_handshake(conn, secret).await?;
    let (mut reader, mut writer) = ch.into_split();
    let (tx, mut rx) = mpsc::channel::<Value>(OUTBOX_CAPACITY);
    let close = Arc::new(Notify::new());
    let outbox = Outbox::new(tx, close.clone());
    service.connected(id, &outbox);
    let mut write_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if writer.send(&msg).await.is_err() {
                break;
            }
        }
    });
    let result: Result<()> = tokio::select! {
        r = async {
            service.handle(id, &outbox, first).await;
            loop {
                let msg = reader.recv().await?;
                service.handle(id, &outbox, msg).await;
            }
        } => r,
        _ = close.notified() => Err(Error::Closed),
        _ = &mut write_task => Err(Error::Closed),
    };
    write_task.abort();
    service.disconnected(id);
    result
}

struct CleanupOnDrop(Option<Paths>);
impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            runtime::cleanup(p);
        }
    }
}
