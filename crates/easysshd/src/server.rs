use crate::channel::Channel;
use crate::error::Result;
use crate::runtime::{self, InstanceLock, Paths};
use crate::session::Secret;
use crate::transport::Listener;
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use tokio::sync::Semaphore;

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

const MAX_CONNECTIONS: usize = 64;

/// A bound, locked and ready daemon endpoint. Dropping it removes the socket,
/// secret and lock files.
pub struct Server {
    paths: Paths,
    secret: Secret,
    listener: Listener,
    handler: Handler,
    _lock: InstanceLock,
}

impl Server {
    /// Prepare the runtime directory, take the single-instance lock, publish a
    /// fresh secret and bind the endpoint.
    pub fn start(runtime_dir: impl Into<PathBuf>, handler: Handler) -> Result<Server> {
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
            handler,
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
                    let handler = self.handler.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        let _ = serve_connection(conn, &secret, handler).await;
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

async fn serve_connection(conn: crate::transport::Conn, secret: &Secret, h: Handler) -> Result<()> {
    let (mut ch, mut msg) = Channel::server_handshake(conn, secret).await?;
    loop {
        if let Some(reply) = h(msg).await {
            ch.send(&reply).await?;
        }
        msg = ch.recv().await?;
    }
}

struct CleanupOnDrop(Option<Paths>);
impl Drop for CleanupOnDrop {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            runtime::cleanup(p);
        }
    }
}
