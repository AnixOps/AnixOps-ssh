//! Keeps an app connected to the daemon.
//!
//! A [`Supervisor`] owns the app's [`Link`]. When the connection drops it
//! reconnects with backoff (0.5 s doubling to 10 s), starting the daemon again
//! when none is running, and re-attaches every live session with
//! `from_seq` = the offset after the last byte the session's consumer saw, so
//! nothing is lost or repeated. Sessions the restarted daemon no longer knows
//! are reported as ended.
//!
//! It also handles the build handshake. A daemon from another app build is
//! asked to drain (`daemon.shutdown_when_idle`): with no running sessions it
//! goes away and the matching daemon is started; otherwise the old daemon
//! keeps the sessions it has, new ones are refused (see
//! [`Supervisor::accepts_new_sessions`]), and the next reconnect, after the
//! old daemon exits, brings up the matching one.
//!
//! Consumers hand over a [`SupervisedSink`] and see, besides the usual session
//! messages, [`SupervisedMsg::Reconnecting`] and [`SupervisedMsg::Resumed`].

use crate::launch::connect_or_spawn_with;
use crate::link::{
    AskpassPrompt, ExitReason, Link, LinkError, LinkHandler, LinkOptions, SessionMsg, SessionSink,
};
use easyssh_proto as proto;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

/// Delay before reconnect attempt `n` (from 0): `first`, doubling up to `max`.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub first: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Backoff {
            first: Duration::from_millis(500),
            max: Duration::from_secs(10),
        }
    }
}

impl Backoff {
    pub fn delay(&self, attempt: u32) -> Duration {
        let factor = 1u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX);
        self.first.saturating_mul(factor).min(self.max)
    }
}

/// What the consumer of a supervised session receives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisedMsg {
    /// Output or the end of the session, as from a plain link. A lost
    /// connection is never reported as an exit: the supervisor reconnects.
    Session(SessionMsg),
    /// The connection dropped; the session is re-attached when it is back.
    Reconnecting,
    /// The session was re-attached; output continues where it stopped.
    Resumed,
}

/// Receives a supervised session's messages; returns `false` when its
/// consumer is gone (the session keeps running in the daemon).
pub type SupervisedSink = Box<dyn FnMut(SupervisedMsg) -> bool + Send>;

/// Starts a daemon process (detached). Called when none answers.
pub type Spawn = Arc<dyn Fn() -> std::io::Result<()> + Send + Sync>;

#[derive(Clone)]
pub struct SupervisorConfig {
    pub runtime_dir: PathBuf,
    /// This app's build version; a daemon that reports another one is drained.
    pub app_version: String,
    pub spawn: Spawn,
    /// How long one attempt waits for a freshly started daemon.
    pub start_wait: Duration,
    pub backoff: Backoff,
}

/// How long a build handover waits for the old daemon to leave.
const HANDOVER_WAIT: Duration = Duration::from_secs(15);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn unavailable() -> LinkError {
    LinkError {
        code: "unavailable".into(),
        message: "the EasySSH daemon is not connected".into(),
    }
}

/// One session the supervisor re-attaches after a reconnect.
#[derive(Clone)]
struct Tracked {
    /// Offset after the last byte delivered to the consumer.
    next_seq: Arc<AtomicU64>,
    sink: Arc<Mutex<SupervisedSink>>,
    /// The consumer saw the end of the session or went away.
    done: Arc<AtomicBool>,
}

impl Tracked {
    fn new(sink: SupervisedSink) -> Tracked {
        Tracked {
            next_seq: Arc::new(AtomicU64::new(0)),
            sink: Arc::new(Mutex::new(sink)),
            done: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The sink to give a link whose liveness is `alive`: it keeps the
    /// offset, swallows the `Exit` a link sends to every sink when the
    /// connection is lost (the supervisor handles that) and ends tracking on a
    /// real exit or a gone consumer.
    fn link_sink(&self, alive: Arc<AtomicBool>) -> SessionSink {
        let this = self.clone();
        Box::new(move |msg| {
            match &msg {
                SessionMsg::Exit { code: None, .. } if !alive.load(Ordering::SeqCst) => {
                    return true
                }
                SessionMsg::Data { seq, data } => {
                    this.next_seq
                        .fetch_max(seq + data.len() as u64, Ordering::SeqCst);
                }
                _ => {}
            }
            let exit = matches!(msg, SessionMsg::Exit { .. });
            this.deliver(SupervisedMsg::Session(msg)) && !exit
        })
    }

    /// Hand `msg` to the consumer; `false` once it is done with the session.
    fn deliver(&self, msg: SupervisedMsg) -> bool {
        if self.done.load(Ordering::SeqCst) {
            return false;
        }
        let exit = matches!(msg, SupervisedMsg::Session(SessionMsg::Exit { .. }));
        let keep = (lock(&self.sink))(msg);
        if exit || !keep {
            self.done.store(true, Ordering::SeqCst);
        }
        keep
    }

    fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

#[derive(Default)]
struct State {
    /// The published link: connected, sessions re-attached.
    link: Option<Link>,
    /// Id of the link whose loss this supervisor reacts to.
    link_id: u64,
    reconnecting: bool,
    tracked: HashMap<String, Tracked>,
    /// Sessions closed while disconnected; closed in the daemon on resume.
    pending_close: Vec<String>,
}

pub struct Supervisor {
    cfg: SupervisorConfig,
    user: Arc<dyn LinkHandler>,
    state: Mutex<State>,
    next_link_id: AtomicU64,
    me: Weak<Supervisor>,
}

/// The handler each link gets: forwards to the app's, and tells the
/// supervisor when this link ends.
struct Proxy {
    sup: Weak<Supervisor>,
    user: Arc<dyn LinkHandler>,
    id: u64,
}

impl LinkHandler for Proxy {
    fn askpass_prompt(&self, prompt: AskpassPrompt) {
        self.user.askpass_prompt(prompt);
    }
    fn askpass_resolved(&self, id: u64) {
        self.user.askpass_resolved(id);
    }
    fn hosts_changed(&self) {
        self.user.hosts_changed();
    }
    fn hostkey_prompt(&self, prompt: crate::link::HostKeyPrompt) {
        self.user.hostkey_prompt(prompt);
    }
    fn hostkey_resolved(&self, id: u64) {
        self.user.hostkey_resolved(id);
    }
    fn transfer_progress(&self, transfer_id: &str, bytes: u64) {
        self.user.transfer_progress(transfer_id, bytes);
    }
    fn transfer_done(&self, transfer_id: &str, status: &str, error: Option<&str>) {
        self.user.transfer_done(transfer_id, status, error);
    }
    fn tunnel_status(&self, status: crate::link::TunnelStatus) {
        self.user.tunnel_status(status);
    }
    fn lost(&self) {
        self.user.lost();
        if let Some(sup) = self.sup.upgrade() {
            sup.on_lost(self.id);
        }
    }
}

impl Supervisor {
    pub fn new(cfg: SupervisorConfig, user: Arc<dyn LinkHandler>) -> Arc<Supervisor> {
        Arc::new_cyclic(|me| Supervisor {
            cfg,
            user,
            state: Mutex::new(State::default()),
            next_link_id: AtomicU64::new(1),
            me: me.clone(),
        })
    }

    /// Connect once (starting the daemon when needed). When that fails, the
    /// reason is returned and reconnecting continues in the background.
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        match self.establish().await {
            Ok((link, id)) => {
                let mut st = lock(&self.state);
                st.link = Some(link);
                st.link_id = id;
                Ok(())
            }
            Err(reason) => {
                lock(&self.state).reconnecting = true;
                self.spawn_reconnect();
                Err(reason)
            }
        }
    }

    /// The connected link, `None` while reconnecting or never connected.
    pub fn link(&self) -> Option<Link> {
        let link = lock(&self.state).link.clone()?;
        link.is_alive().then_some(link)
    }

    pub fn is_reconnecting(&self) -> bool {
        lock(&self.state).reconnecting
    }

    /// Whether `session.open` can go to the daemon: connected and not draining
    /// (a daemon of another build that only finishes its sessions).
    pub fn accepts_new_sessions(&self) -> bool {
        self.link().is_some_and(|link| !link.is_draining())
    }

    // ---- sessions ------------------------------------------------------

    pub async fn open_session(
        &self,
        host_id: Option<&str>,
        cols: u16,
        rows: u16,
        sink: SupervisedSink,
    ) -> Result<String, LinkError> {
        Ok(self
            .open_session_info(host_id, cols, rows, sink)
            .await?
            .session_id)
    }

    /// [`Supervisor::open_session`] that also reports the engine.
    pub async fn open_session_info(
        &self,
        host_id: Option<&str>,
        cols: u16,
        rows: u16,
        sink: SupervisedSink,
    ) -> Result<crate::link::OpenedSession, LinkError> {
        let link = self.link().ok_or_else(unavailable)?;
        let tracked = Tracked::new(sink);
        let opened = link
            .open_session_info(host_id, cols, rows, tracked.link_sink(link.alive_flag()))
            .await?;
        if !tracked.is_done() {
            lock(&self.state)
                .tracked
                .insert(opened.session_id.clone(), tracked);
        }
        Ok(opened)
    }

    /// `session.attach`; `from_seq` `None` replays everything buffered.
    pub async fn attach_session(
        &self,
        session_id: &str,
        from_seq: Option<u64>,
        sink: SupervisedSink,
    ) -> Result<proto::SessionAttachResult, LinkError> {
        let link = self.link().ok_or_else(unavailable)?;
        let tracked = Tracked::new(sink);
        tracked
            .next_seq
            .store(from_seq.unwrap_or(0), Ordering::SeqCst);
        // Replaces the session's earlier registration, if any.
        lock(&self.state)
            .tracked
            .insert(session_id.to_owned(), tracked.clone());
        match link
            .attach_session(session_id, from_seq, tracked.link_sink(link.alive_flag()))
            .await
        {
            Ok(result) => Ok(result),
            Err(e) => {
                lock(&self.state).tracked.remove(session_id);
                Err(e)
            }
        }
    }

    pub fn detach_session(&self, session_id: &str) {
        lock(&self.state).tracked.remove(session_id);
        if let Some(link) = self.link() {
            link.detach_session(session_id);
        }
    }

    pub fn close_session(&self, session_id: &str) {
        let mut st = lock(&self.state);
        st.tracked.remove(session_id);
        match st.link.clone().filter(Link::is_alive) {
            Some(link) => {
                drop(st);
                link.close_session(session_id);
            }
            // Closed in the daemon once the connection is back.
            None => st.pending_close.push(session_id.to_owned()),
        }
    }

    pub fn write_session(&self, session_id: &str, data: &str) -> Result<(), LinkError> {
        self.link()
            .ok_or_else(unavailable)?
            .write_session(session_id, data)
    }

    pub fn resize_session(&self, session_id: &str, cols: u16, rows: u16) -> Result<(), LinkError> {
        self.link()
            .ok_or_else(unavailable)?
            .resize_session(session_id, cols, rows)
    }

    pub async fn list_sessions(&self) -> Result<Vec<proto::SessionInfo>, LinkError> {
        self.link().ok_or_else(unavailable)?.list_sessions().await
    }

    pub fn askpass_reply(&self, id: u64, answer: Option<String>) -> Result<(), LinkError> {
        self.link()
            .ok_or_else(unavailable)?
            .askpass_reply(id, answer)
    }

    /// `"<port> <token>"` for OpenSSH children; `None` while disconnected.
    pub fn askpass_endpoint(&self) -> Option<String> {
        self.link().map(|l| l.askpass_endpoint().to_owned())
    }

    /// A command that is neither a session nor an askpass call (`hosts.*`).
    pub async fn request(&self, kind: &str, payload: Value) -> Result<Value, LinkError> {
        self.link()
            .ok_or_else(unavailable)?
            .request(kind, payload)
            .await
    }

    // ---- connecting ----------------------------------------------------

    fn options(&self) -> LinkOptions {
        LinkOptions {
            app_version: Some(self.cfg.app_version.clone()),
        }
    }

    async fn connect_once(&self) -> Result<(Link, u64), String> {
        let id = self.next_link_id.fetch_add(1, Ordering::SeqCst);
        let handler = Arc::new(Proxy {
            sup: self.me.clone(),
            user: self.user.clone(),
            id,
        });
        let spawn = self.cfg.spawn.clone();
        let link = connect_or_spawn_with(
            &self.cfg.runtime_dir,
            handler,
            self.options(),
            move || spawn(),
            self.cfg.start_wait,
        )
        .await?;
        Ok((link, id))
    }

    /// A link to a daemon of this app's build, handing over from an older one
    /// when needed (see the module docs).
    async fn establish(&self) -> Result<(Link, u64), String> {
        let deadline = Instant::now() + HANDOVER_WAIT;
        loop {
            let (link, id) = self.connect_once().await?;
            if link.daemon_app_version() == Some(self.cfg.app_version.as_str()) {
                return Ok((link, id));
            }
            match link.shutdown_when_idle().await {
                // Nothing left to protect: it exits now. Start the matching
                // one. An idle daemon can exit before its reply reaches us, so
                // a lost connection here means the same thing.
                Ok(0) => {}
                Err(e) if e.code == "disconnected" => {}
                // It keeps its sessions until they end; stay attached to them.
                Ok(_) => return Ok((link, id)),
                // A daemon too old to drain: keep using it.
                Err(e) if e.code == "unknown_command" || e.code == "invalid_command" => {
                    return Ok((link, id))
                }
                Err(e) => return Err(e.message),
            }
            drop(link);
            if Instant::now() >= deadline {
                return Err("the previous daemon build did not exit".into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn on_lost(self: &Arc<Self>, id: u64) {
        let tracked: Vec<Tracked> = {
            let mut st = lock(&self.state);
            if st.link_id != id || st.reconnecting {
                return;
            }
            st.reconnecting = true;
            st.link = None;
            st.tracked.retain(|_, t| !t.is_done());
            st.tracked.values().cloned().collect()
        };
        for t in tracked {
            t.deliver(SupervisedMsg::Reconnecting);
        }
        self.spawn_reconnect();
    }

    fn spawn_reconnect(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move { this.reconnect_loop().await });
    }

    async fn reconnect_loop(self: Arc<Self>) {
        let mut attempt = 0u32;
        loop {
            tokio::time::sleep(self.cfg.backoff.delay(attempt)).await;
            attempt = attempt.saturating_add(1);
            match self.establish().await {
                Ok((link, id)) => {
                    if self.resume(link, id).await {
                        return;
                    }
                }
                Err(reason) => eprintln!("easysshd: reconnect failed: {reason}"),
            }
        }
    }

    /// Re-attach every tracked session on `link`, then publish it. `false`
    /// when the link died meanwhile.
    async fn resume(&self, link: Link, id: u64) -> bool {
        lock(&self.state).link_id = id;
        let mut attached: HashSet<String> = HashSet::new();
        loop {
            let todo: Vec<(String, Tracked)> = {
                let mut st = lock(&self.state);
                st.tracked.retain(|_, t| !t.is_done());
                let todo: Vec<_> = st
                    .tracked
                    .iter()
                    .filter(|(sid, _)| !attached.contains(*sid))
                    .map(|(sid, t)| (sid.clone(), t.clone()))
                    .collect();
                if todo.is_empty() {
                    // Nothing left to attach: publish, unless the link died.
                    if !link.is_alive() {
                        return false;
                    }
                    for sid in st.pending_close.drain(..) {
                        link.close_session(&sid);
                    }
                    st.link = Some(link);
                    st.reconnecting = false;
                    drop(st);
                    self.user.resumed();
                    return true;
                }
                todo
            };
            for (sid, tracked) in todo {
                attached.insert(sid.clone());
                let from = tracked.next_seq.load(Ordering::SeqCst);
                match link
                    .attach_session(&sid, Some(from), tracked.link_sink(link.alive_flag()))
                    .await
                {
                    Ok(_) => {
                        tracked.deliver(SupervisedMsg::Resumed);
                    }
                    Err(e) if e.code == "disconnected" => return false,
                    Err(_) => {
                        // The daemon restarted (or dropped the session): the
                        // process is gone, so the tab shows it as ended.
                        tracked.deliver(SupervisedMsg::Session(SessionMsg::Exit {
                            code: None,
                            reason: ExitReason::Disconnected,
                        }));
                        lock(&self.state).tracked.remove(&sid);
                    }
                }
            }
            if !link.is_alive() {
                return false;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_to_the_cap() {
        let b = Backoff::default();
        let secs: Vec<f64> = (0..8).map(|n| b.delay(n).as_secs_f64()).collect();
        assert_eq!(secs, [0.5, 1.0, 2.0, 4.0, 8.0, 10.0, 10.0, 10.0]);
        assert_eq!(b.delay(1000), Duration::from_secs(10));
    }
}
