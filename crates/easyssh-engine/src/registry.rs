//! One connection per host, shared by the terminal, the file pane and
//! tunnels.
//!
//! A [`Registry`] hands out [`Lease`]s on pooled connections keyed by
//! (host id, `user@host:port`, jump chain):
//!
//! - Callers that ask for the same key while a connection is being made wait
//!   at a per-key async gate, so there is one connect and one set of prompts.
//!   They also share its failure: when the connect they waited for failed,
//!   they get that error instead of prompting again.
//! - A lease is a counted reference. When the last one is released the
//!   connection lingers (30 s by default) and is closed afterwards unless
//!   somebody leased it again.
//! - A connection that died is replaced on the next request.
//! - `opened()` counts every connection ever made; the debug command
//!   `engine.connections` reports it, which is how the end-to-end tests prove
//!   that a terminal, the files and a tunnel share one connection.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

/// How long a connection stays up after its last lease was released.
pub const DEFAULT_LINGER: Duration = Duration::from_secs(30);

/// What a pooled connection must offer.
pub trait Pooled: Clone + Send + Sync + 'static {
    /// Whether the connection can still carry channels.
    fn is_alive(&self) -> bool;
    /// Ends the connection (called when it lingered unused, or at shutdown).
    fn close(&self) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

impl Pooled for easyssh_ssh::Session {
    fn is_alive(&self) -> bool {
        !self.is_closed()
    }

    fn close(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let session = self.clone();
        Box::pin(async move {
            let _ = session.disconnect().await;
        })
    }
}

/// What identifies a connection.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub host_id: String,
    /// `user@host:port` of the target.
    pub target: String,
    /// The jump hops, in order, as `user@host:port`.
    pub jump: Vec<String>,
    /// The outbound proxy of the first hop (`socks5://host:port`), so a host
    /// whose proxy was edited does not reuse a connection made another way.
    pub via: Option<String>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

struct Entry<S, E> {
    /// The gate: held while connecting, so concurrent callers queue behind it.
    slot: tokio::sync::Mutex<Option<S>>,
    state: Mutex<EntryState<E>>,
}

struct EntryState<E> {
    leases: usize,
    /// Bumped whenever the connection is released to the linger timer or
    /// leased again, so an old timer can tell it is stale.
    epoch: u64,
    /// The last connect failure and the attempt number it ended.
    failure: Option<(u64, E)>,
    /// Connect attempts that have finished (successfully or not).
    attempts: u64,
}

struct Inner<S, E> {
    entries: Mutex<HashMap<Key, Arc<Entry<S, E>>>>,
    opened: AtomicU64,
    linger_ms: AtomicU64,
}

/// See the module documentation.
pub struct Registry<S: Pooled, E: Clone + Send + 'static> {
    inner: Arc<Inner<S, E>>,
}

impl<S: Pooled, E: Clone + Send + 'static> Clone for Registry<S, E> {
    fn clone(&self) -> Self {
        Registry {
            inner: self.inner.clone(),
        }
    }
}

/// One snapshot row of [`Registry::connections`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub key: Key,
    pub leases: usize,
}

impl<S: Pooled, E: Clone + Send + 'static> Registry<S, E> {
    pub fn new(linger: Duration) -> Self {
        Registry {
            inner: Arc::new(Inner {
                entries: Mutex::new(HashMap::new()),
                opened: AtomicU64::new(0),
                linger_ms: AtomicU64::new(linger.as_millis() as u64),
            }),
        }
    }

    pub fn set_linger(&self, linger: Duration) {
        self.inner
            .linger_ms
            .store(linger.as_millis() as u64, Ordering::SeqCst);
    }

    pub fn linger(&self) -> Duration {
        Duration::from_millis(self.inner.linger_ms.load(Ordering::SeqCst))
    }

    /// Connections opened since the registry was created.
    pub fn opened(&self) -> u64 {
        self.inner.opened.load(Ordering::SeqCst)
    }

    /// Connections that are open now (leased or lingering).
    pub async fn connections(&self) -> Vec<Info> {
        let entries: Vec<(Key, Arc<Entry<S, E>>)> = lock(&self.inner.entries)
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut out = Vec::new();
        for (key, entry) in entries {
            // A connect in progress holds the gate; it is not a connection yet.
            let Ok(slot) = entry.slot.try_lock() else {
                continue;
            };
            if slot.as_ref().is_some_and(Pooled::is_alive) {
                out.push(Info {
                    key,
                    leases: lock(&entry.state).leases,
                });
            }
        }
        out.sort_by(|a, b| (&a.key.host_id, &a.key.target).cmp(&(&b.key.host_id, &b.key.target)));
        out
    }

    /// The `engine.connections` report: how many connections were opened and
    /// which ones are up now.
    pub async fn report(&self) -> serde_json::Value {
        let rows = self.connections().await;
        serde_json::json!({
            "opened": self.opened(),
            "live": rows.len(),
            "connections": rows.iter().map(|row| serde_json::json!({
                "host_id": row.key.host_id,
                "target": row.key.target,
                "jump": row.key.jump,
                "leases": row.leases,
            })).collect::<Vec<_>>(),
        })
    }

    /// A lease on the connection for `key`, connecting with `connect` when
    /// there is none (or it died). `connect` runs at most once for callers
    /// that ask at the same time.
    pub async fn acquire<F, Fut>(&self, key: Key, connect: F) -> Result<Lease<S, E>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<S, E>>,
    {
        let entry = lock(&self.inner.entries)
            .entry(key.clone())
            .or_insert_with(|| {
                Arc::new(Entry {
                    slot: tokio::sync::Mutex::new(None),
                    state: Mutex::new(EntryState {
                        leases: 0,
                        epoch: 0,
                        failure: None,
                        attempts: 0,
                    }),
                })
            })
            .clone();
        let attempts_before = lock(&entry.state).attempts;
        let mut slot = entry.slot.lock().await;
        if let Some(session) = slot.as_ref().filter(|s| s.is_alive()) {
            let session = session.clone();
            let mut state = lock(&entry.state);
            state.leases += 1;
            state.epoch += 1;
            drop(state);
            return Ok(self.lease(entry.clone(), session));
        }
        // Somebody else's connect finished while we waited at the gate and
        // failed: share its failure rather than asking the user again.
        {
            let state = lock(&entry.state);
            if let Some((attempt, error)) = &state.failure {
                if *attempt > attempts_before {
                    return Err(error.clone());
                }
            }
        }
        let result = connect().await;
        let mut state = lock(&entry.state);
        state.attempts += 1;
        match result {
            Ok(session) => {
                self.inner.opened.fetch_add(1, Ordering::SeqCst);
                *slot = Some(session.clone());
                state.leases += 1;
                state.epoch += 1;
                state.failure = None;
                drop(state);
                Ok(self.lease(entry.clone(), session))
            }
            Err(error) => {
                state.failure = Some((state.attempts, error.clone()));
                Err(error)
            }
        }
    }

    fn lease(&self, entry: Arc<Entry<S, E>>, session: S) -> Lease<S, E> {
        Lease {
            session: Some(session),
            entry,
            inner: self.inner.clone(),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// Closes the connection for `key` now, whoever still uses it, so the next
    /// [`Registry::acquire`] connects anew. Leases already out keep counting.
    pub async fn evict(&self, key: &Key) {
        let entry = lock(&self.inner.entries).get(key).cloned();
        let Some(entry) = entry else { return };
        let mut slot = entry.slot.lock().await;
        if let Some(session) = slot.take() {
            session.close().await;
        }
    }

    /// Closes every connection now (daemon shutdown).
    pub async fn close_all(&self) {
        let entries: Vec<Arc<Entry<S, E>>> = lock(&self.inner.entries).values().cloned().collect();
        for entry in entries {
            let mut slot = entry.slot.lock().await;
            if let Some(session) = slot.take() {
                session.close().await;
            }
        }
    }
}

/// A counted reference to a pooled connection. Dropping it releases the
/// reference; the connection lingers and is then closed when it was the last.
pub struct Lease<S: Pooled, E: Clone + Send + 'static> {
    session: Option<S>,
    entry: Arc<Entry<S, E>>,
    inner: Arc<Inner<S, E>>,
    runtime: tokio::runtime::Handle,
}

impl<S: Pooled, E: Clone + Send + 'static> Lease<S, E> {
    pub fn session(&self) -> &S {
        self.session.as_ref().expect("lease holds a session")
    }
}

impl<S: Pooled, E: Clone + Send + 'static> std::ops::Deref for Lease<S, E> {
    type Target = S;
    fn deref(&self) -> &S {
        self.session()
    }
}

impl<S: Pooled, E: Clone + Send + 'static> Drop for Lease<S, E> {
    fn drop(&mut self) {
        let epoch = {
            let mut state = lock(&self.entry.state);
            state.leases = state.leases.saturating_sub(1);
            if state.leases > 0 {
                return;
            }
            state.epoch += 1;
            state.epoch
        };
        let entry = self.entry.clone();
        let linger = Duration::from_millis(self.inner.linger_ms.load(Ordering::SeqCst));
        self.runtime.spawn(async move {
            tokio::time::sleep(linger).await;
            let mut slot = entry.slot.lock().await;
            {
                let state = lock(&entry.state);
                if state.leases > 0 || state.epoch != epoch {
                    return;
                }
            }
            if let Some(session) = slot.take() {
                session.close().await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[derive(Clone)]
    struct Fake {
        alive: Arc<AtomicBool>,
        closed: Arc<AtomicU64>,
        n: u64,
    }

    impl Pooled for Fake {
        fn is_alive(&self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }
        fn close(&self) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            self.alive.store(false, Ordering::SeqCst);
            self.closed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {})
        }
    }

    fn key(host: &str) -> Key {
        Key {
            host_id: host.into(),
            target: format!("u@{host}:22"),
            jump: vec![],
            via: None,
        }
    }

    fn maker(count: Arc<AtomicU64>, closed: Arc<AtomicU64>, delay: u64) -> Fake {
        let n = count.fetch_add(1, Ordering::SeqCst) + 1;
        let _ = delay;
        Fake {
            alive: Arc::new(AtomicBool::new(true)),
            closed,
            n,
        }
    }

    type Reg = Registry<Fake, String>;

    #[tokio::test]
    async fn concurrent_callers_share_one_connect() {
        let reg: Reg = Registry::new(Duration::from_secs(30));
        let made = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicU64::new(0));
        let mut tasks = Vec::new();
        for _ in 0..8 {
            let (reg, made, closed) = (reg.clone(), made.clone(), closed.clone());
            tasks.push(tokio::spawn(async move {
                reg.acquire(key("a"), || async move {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok(maker(made, closed, 0))
                })
                .await
                .map_err(|e| e.to_string())
                .map(|lease| lease.n)
            }));
        }
        let mut leases = Vec::new();
        for task in tasks {
            leases.push(task.await.unwrap().unwrap());
        }
        assert_eq!(made.load(Ordering::SeqCst), 1);
        assert_eq!(reg.opened(), 1);
        assert!(leases.iter().all(|n| *n == 1));
    }

    #[tokio::test]
    async fn waiters_share_a_failed_connect() {
        let reg: Reg = Registry::new(Duration::from_secs(30));
        let attempts = Arc::new(AtomicU64::new(0));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let (reg, attempts) = (reg.clone(), attempts.clone());
            tasks.push(tokio::spawn(async move {
                reg.acquire(key("a"), || async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Err::<Fake, _>("no password".to_owned())
                })
                .await
                .err()
            }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap().as_deref(), Some("no password"));
        }
        // One prompt round, not four.
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert_eq!(reg.opened(), 0);
        // A later request tries again.
        let again = reg
            .acquire(key("a"), || async {
                Ok(maker(Arc::default(), Arc::default(), 0))
            })
            .await;
        assert!(again.is_ok());
    }

    #[tokio::test]
    async fn different_keys_do_not_share() {
        let reg: Reg = Registry::new(Duration::from_secs(30));
        let made = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicU64::new(0));
        let a = reg
            .acquire(key("a"), || async {
                Ok(maker(made.clone(), closed.clone(), 0))
            })
            .await
            .map_err(drop)
            .unwrap();
        let b = reg
            .acquire(key("b"), || async {
                Ok(maker(made.clone(), closed.clone(), 0))
            })
            .await
            .map_err(drop)
            .unwrap();
        assert_ne!(a.n, b.n);
        assert_eq!(reg.opened(), 2);
        let rows = reg.connections().await;
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.leases == 1));
    }

    #[tokio::test]
    async fn leases_are_counted_and_the_connection_lingers() {
        let reg: Reg = Registry::new(Duration::from_millis(150));
        let made = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicU64::new(0));
        let connect = || async { Ok(maker(made.clone(), closed.clone(), 0)) };
        let first = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        let second = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        assert_eq!(first.n, second.n);
        assert_eq!(reg.connections().await[0].leases, 2);

        drop(first);
        tokio::time::sleep(Duration::from_millis(300)).await;
        // One lease left: nothing closed.
        assert_eq!(closed.load(Ordering::SeqCst), 0);

        drop(second);
        // Lingering: still open and reusable within the window.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(closed.load(Ordering::SeqCst), 0);
        let third = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        assert_eq!(made.load(Ordering::SeqCst), 1, "reused during the linger");
        // The old timer is stale: wait past it.
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert_eq!(closed.load(Ordering::SeqCst), 0);

        drop(third);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1, "closed after the linger");
        assert!(reg.connections().await.is_empty());
        // Asking again connects anew.
        let fourth = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        assert_eq!(made.load(Ordering::SeqCst), 2);
        assert_eq!(reg.opened(), 2);
        drop(fourth);
    }

    #[tokio::test]
    async fn an_evicted_connection_is_closed_and_the_next_caller_connects_anew() {
        let reg: Reg = Registry::new(Duration::from_secs(30));
        let made = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicU64::new(0));
        let connect = || async { Ok(maker(made.clone(), closed.clone(), 0)) };
        let old = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        reg.evict(&key("a")).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        // Evicting what is not there is fine.
        reg.evict(&key("nobody")).await;
        let new = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        assert_ne!(old.n, new.n);
        assert_eq!(
            reg.connections().await[0].leases,
            2,
            "old lease still counts"
        );
        drop(old);
        drop(new);
    }

    #[tokio::test]
    async fn a_dead_connection_is_replaced() {
        let reg: Reg = Registry::new(Duration::from_secs(30));
        let made = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicU64::new(0));
        let connect = || async { Ok(maker(made.clone(), closed.clone(), 0)) };
        let first = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        first.alive.store(false, Ordering::SeqCst);
        let second = reg.acquire(key("a"), connect).await.map_err(drop).unwrap();
        assert_ne!(first.n, second.n);
        assert_eq!(reg.opened(), 2);
    }

    #[tokio::test]
    async fn close_all_ends_everything() {
        let reg: Reg = Registry::new(Duration::from_secs(30));
        let made = Arc::new(AtomicU64::new(0));
        let closed = Arc::new(AtomicU64::new(0));
        let lease = reg
            .acquire(key("a"), || async {
                Ok(maker(made.clone(), closed.clone(), 0))
            })
            .await
            .map_err(drop)
            .unwrap();
        reg.close_all().await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(!lease.is_alive());
    }
}
