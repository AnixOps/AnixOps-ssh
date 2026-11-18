//! Sessions that survive a dropped connection: attach to tmux or screen when
//! the shell opens, and reconnect with backoff when the connection dies.
//!
//! SSH has no resume, so a phone that changes network or wakes from sleep has
//! lost its TCP connection and the shell on the far end with it. What survives
//! is a multiplexer on the host: with `tmux new -A -s anixops` as the first
//! thing the shell runs, a new connection lands in the same session, with the
//! same programs and screen. The [`Supervisor`] does that and the reconnecting:
//!
//! - it opens the shell through a [`ShellBackend`] (the [`Engine`]) and, when
//!   the host opted in, types the attach command;
//! - when the connection drops without the shell exiting, and the session is
//!   attached, it retries with growing delays ([`ReconnectPolicy`]), telling
//!   the UI through [`SupervisedSink::status`], and types the attach command
//!   again on the new shell;
//! - [`Supervisor::reconnect`] does the same on demand (the app came back from
//!   the background and the old socket cannot be trusted);
//! - a session that is not attached never reconnects by itself: a new shell
//!   would silently be a different one, so the UI offers it instead.
//!
//! The public id stays the same across reconnects. Everything is generic over
//! the backend so the logic is tested without a network.

use crate::{Engine, OpenError, ShellSink, Target};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A terminal multiplexer on the host to attach to when the shell opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Attach {
    /// A plain shell.
    #[default]
    None,
    /// `tmux new -A -s anixops`: attach to the session, or create it.
    Tmux,
    /// `screen -dRR anixops`: reattach, detaching it elsewhere, or create it.
    Screen,
}

impl Attach {
    /// Parses the setting the UI stores (`"tmux"`, `"screen"`, anything else is none).
    pub fn parse(value: &str) -> Attach {
        match value {
            "tmux" => Attach::Tmux,
            "screen" => Attach::Screen,
            _ => Attach::None,
        }
    }

    /// What is typed into a fresh shell. The leading space keeps it out of the
    /// history of shells that honour `ignorespace`; `exec` replaces the login
    /// shell, so leaving the multiplexer ends the connection, and a host
    /// without it just prints "not found" and stays in the shell.
    pub fn command(self) -> Option<&'static str> {
        match self {
            Attach::None => None,
            Attach::Tmux => Some(" exec tmux new -A -s anixops\r"),
            Attach::Screen => Some(" exec screen -dRR anixops\r"),
        }
    }
}

/// How hard to try to get a dropped connection back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    /// Attempts before the session is given up (the UI then offers "Reconnect").
    pub max_attempts: u32,
    /// The delay before the first attempt; each next one doubles it.
    pub base: Duration,
    /// The longest delay between attempts.
    pub cap: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        ReconnectPolicy {
            max_attempts: 8,
            base: Duration::from_millis(500),
            cap: Duration::from_secs(30),
        }
    }
}

impl ReconnectPolicy {
    /// The wait before attempt `attempt` (1 is the first).
    pub fn delay(&self, attempt: u32) -> Duration {
        let doublings = attempt.saturating_sub(1).min(16);
        self.base.saturating_mul(1u32 << doublings).min(self.cap)
    }
}

/// A connect failure that retrying cannot fix: the person said no, the
/// credentials are wrong, or the host key is not trusted.
pub fn is_fatal(code: &str) -> bool {
    matches!(
        code,
        "auth_failed"
            | "cancelled"
            | "hostkey_rejected"
            | "hostkey_changed"
            | "hostkey_revoked"
            | "vault_locked"
            | "not_found"
    )
}

/// What the UI is told besides terminal output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Waiting `delay` before attempt number `attempt`.
    Reconnecting { attempt: u32, delay: Duration },
    /// A new connection is up and attached.
    Resumed,
}

/// A [`ShellSink`] that also hears about reconnects.
pub trait SupervisedSink: ShellSink {
    fn status(&self, status: Status);
}

/// Per-session choices.
#[derive(Debug, Clone, Copy, Default)]
pub struct SessionOptions {
    pub attach: Attach,
    /// Reconnect by itself when the connection drops. Only honoured with an
    /// attach: see the module notes.
    pub reconnect: bool,
    pub policy: ReconnectPolicy,
}

/// What the supervisor needs from the engine; the [`Engine`] is the real one.
#[async_trait]
pub trait ShellBackend: Send + Sync + 'static {
    async fn open(
        &self,
        target: Target,
        cols: u16,
        rows: u16,
        sink: Arc<dyn ShellSink>,
    ) -> Result<String, OpenError>;
    fn write(&self, id: &str, data: &[u8]) -> Result<(), String>;
    fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String>;
    fn close(&self, id: &str);
}

#[async_trait]
impl ShellBackend for Engine {
    async fn open(
        &self,
        target: Target,
        cols: u16,
        rows: u16,
        sink: Arc<dyn ShellSink>,
    ) -> Result<String, OpenError> {
        self.open_shell(target, cols, rows, sink).await
    }
    fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
        Engine::write(self, id, data)
    }
    fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        Engine::resize(self, id, cols, rows)
    }
    fn close(&self, id: &str) {
        Engine::close(self, id)
    }
}

struct State {
    /// The backend's id of the live shell; `None` while reconnecting.
    inner: Option<String>,
    /// Bumped at every (re)connect so an old shell's end is told apart from
    /// the current one's.
    generation: u64,
    cols: u16,
    rows: u16,
    closing: bool,
    /// A reconnect loop is running; a second one must not start.
    reconnecting: bool,
}

struct Session<B: ShellBackend> {
    backend: Arc<B>,
    registry: Arc<Mutex<HashMap<String, Arc<Session<B>>>>>,
    public_id: String,
    target: Target,
    options: SessionOptions,
    sink: Arc<dyn SupervisedSink>,
    state: Mutex<State>,
}

/// The shell of one generation: forwards output, and decides what the end of
/// the shell means.
struct InnerSink<B: ShellBackend> {
    session: Arc<Session<B>>,
    generation: u64,
}

impl<B: ShellBackend> ShellSink for InnerSink<B> {
    fn data(&self, text: String) {
        if lock(&self.session.state).generation == self.generation {
            self.session.sink.data(text);
        }
    }

    fn exit(&self, code: Option<u32>) {
        let session = self.session.clone();
        let (stale, closing) = {
            let mut state = lock(&session.state);
            let stale = state.generation != self.generation;
            if !stale {
                state.inner = None;
            }
            (stale, state.closing)
        };
        if stale {
            return;
        }
        // A shell that ended (even with an error status), or one the person
        // closed, is over. Only a dropped connection (no status) is retried.
        let retry = !closing
            && code.is_none()
            && session.options.reconnect
            && session.options.attach != Attach::None;
        if retry {
            Session::start_reconnect(&session, false);
        } else {
            session.finish(code);
        }
    }
}

impl<B: ShellBackend> Session<B> {
    /// Opens one connection and attaches. Records it as the live shell.
    async fn connect_once(self: &Arc<Self>) -> Result<(), OpenError> {
        let (cols, rows, generation) = {
            let mut state = lock(&self.state);
            state.generation += 1;
            (state.cols, state.rows, state.generation)
        };
        let sink = Arc::new(InnerSink {
            session: self.clone(),
            generation,
        });
        let id = self
            .backend
            .open(self.target.clone(), cols, rows, sink)
            .await?;
        let superseded = {
            let mut state = lock(&self.state);
            if state.closing || state.generation != generation {
                true
            } else {
                state.inner = Some(id.clone());
                false
            }
        };
        if superseded {
            self.backend.close(&id);
            return Ok(());
        }
        if let Some(command) = self.options.attach.command() {
            let _ = self.backend.write(&id, command.as_bytes());
        }
        Ok(())
    }

    /// Tells the UI the session is over and forgets it.
    fn finish(&self, code: Option<u32>) {
        lock(&self.registry).remove(&self.public_id);
        self.sink.exit(code);
    }

    /// Starts the retry loop in the background. `immediate` skips the first
    /// delay (the caller already knows the connection is dead).
    fn start_reconnect(this: &Arc<Self>, immediate: bool) {
        {
            let mut state = lock(&this.state);
            if state.reconnecting {
                return;
            }
            state.reconnecting = true;
        }
        let session = this.clone();
        tokio::spawn(async move {
            session.reconnect_loop(immediate).await;
            lock(&session.state).reconnecting = false;
        });
    }

    async fn reconnect_loop(self: &Arc<Self>, immediate: bool) {
        let policy = self.options.policy;
        for attempt in 1..=policy.max_attempts {
            let delay = if immediate && attempt == 1 {
                Duration::ZERO
            } else {
                policy.delay(attempt)
            };
            self.sink.status(Status::Reconnecting { attempt, delay });
            tokio::time::sleep(delay).await;
            if lock(&self.state).closing {
                return;
            }
            match self.connect_once().await {
                Ok(()) => {
                    if !lock(&self.state).closing {
                        self.sink.status(Status::Resumed);
                    }
                    return;
                }
                Err(error) if is_fatal(error.code) => {
                    tracing::debug!("reconnect stopped: {error}");
                    break;
                }
                Err(error) => tracing::debug!("reconnect attempt {attempt} failed: {error}"),
            }
        }
        if !lock(&self.state).closing {
            self.finish(None);
        }
    }
}

/// Opens shells that attach and reconnect, and keeps them by a stable id.
pub struct Supervisor<B: ShellBackend = Engine> {
    backend: Arc<B>,
    sessions: Arc<Mutex<HashMap<String, Arc<Session<B>>>>>,
    next: AtomicU64,
}

impl<B: ShellBackend> Supervisor<B> {
    pub fn new(backend: Arc<B>) -> Self {
        Supervisor {
            backend,
            sessions: Arc::default(),
            next: AtomicU64::new(1),
        }
    }

    /// Connects, opens the shell, attaches if asked, and returns the session
    /// id the UI keeps for the life of the screen.
    pub async fn open(
        &self,
        target: Target,
        cols: u16,
        rows: u16,
        sink: Arc<dyn SupervisedSink>,
        options: SessionOptions,
    ) -> Result<String, OpenError> {
        let public_id = format!("s{}", self.next.fetch_add(1, Ordering::Relaxed));
        let session = Arc::new(Session {
            backend: self.backend.clone(),
            registry: self.sessions.clone(),
            public_id: public_id.clone(),
            target,
            options,
            sink,
            state: Mutex::new(State {
                inner: None,
                generation: 0,
                cols: cols.max(1),
                rows: rows.max(1),
                closing: false,
                reconnecting: false,
            }),
        });
        // Registered first so a shell that ends at once can find itself.
        lock(&self.sessions).insert(public_id.clone(), session.clone());
        if let Err(error) = session.connect_once().await {
            lock(&self.sessions).remove(&public_id);
            return Err(error);
        }
        Ok(public_id)
    }

    fn session(&self, id: &str) -> Result<Arc<Session<B>>, String> {
        lock(&self.sessions)
            .get(id)
            .cloned()
            .ok_or_else(|| format!("unknown session: {id}"))
    }

    pub fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
        let session = self.session(id)?;
        let inner = lock(&session.state).inner.clone();
        match inner {
            Some(inner) => self.backend.write(&inner, data),
            None => Err("reconnecting".to_owned()),
        }
    }

    /// Resizes the shell; the size is kept for the next connection.
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let session = self.session(id)?;
        let inner = {
            let mut state = lock(&session.state);
            state.cols = cols.max(1);
            state.rows = rows.max(1);
            state.inner.clone()
        };
        match inner {
            Some(inner) => self.backend.resize(&inner, cols, rows),
            None => Ok(()),
        }
    }

    /// Drops the current connection and connects again at once, without
    /// waiting for the old socket to fail. For an app that just came back
    /// from the background. Ends the session when it cannot reconnect (the
    /// UI hears `exit`). Needs an attached session: anything else would
    /// replace the person's shell with a different one.
    pub fn reconnect(&self, id: &str) -> Result<(), String> {
        let session = self.session(id)?;
        if session.options.attach == Attach::None {
            return Err("only an attached session can reconnect".to_owned());
        }
        let old = {
            let mut state = lock(&session.state);
            if state.closing {
                return Err("the session is closing".to_owned());
            }
            state.generation += 1;
            state.inner.take()
        };
        if let Some(old) = old {
            self.backend.close(&old);
        }
        Session::start_reconnect(&session, true);
        Ok(())
    }

    /// Ends the session for good. Closing an unknown or ended id is fine.
    pub fn close(&self, id: &str) {
        let Some(session) = lock(&self.sessions).get(id).cloned() else {
            return;
        };
        let inner = {
            let mut state = lock(&session.state);
            state.closing = true;
            state.inner.take()
        };
        match inner {
            Some(inner) => {
                // The backend answers with `exit`, which reaches the UI.
                self.backend.close(&inner);
            }
            None => {
                // Reconnecting: nothing to close; the loop sees `closing`.
                session.finish(None);
            }
        }
    }

    pub fn close_all(&self) {
        let ids: Vec<String> = lock(&self.sessions).keys().cloned().collect();
        for id in ids {
            self.close(&id);
        }
    }

    pub fn open_ids(&self) -> Vec<String> {
        lock(&self.sessions).keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Fake {
        opens: Mutex<u32>,
        /// Results for the next opens; once empty, opens succeed.
        script: Mutex<VecDeque<Result<(), &'static str>>>,
        writes: Mutex<Vec<(String, String)>>,
        closed: Mutex<Vec<String>>,
        sinks: Mutex<Vec<Arc<dyn ShellSink>>>,
    }

    #[async_trait]
    impl ShellBackend for Fake {
        async fn open(
            &self,
            _target: Target,
            _cols: u16,
            _rows: u16,
            sink: Arc<dyn ShellSink>,
        ) -> Result<String, OpenError> {
            let n = {
                let mut opens = lock(&self.opens);
                *opens += 1;
                *opens
            };
            if let Some(Err(code)) = lock(&self.script).pop_front() {
                return Err(OpenError {
                    code,
                    message: code.to_owned(),
                });
            }
            lock(&self.sinks).push(sink);
            Ok(format!("i{n}"))
        }
        fn write(&self, id: &str, data: &[u8]) -> Result<(), String> {
            lock(&self.writes).push((id.to_owned(), String::from_utf8_lossy(data).into_owned()));
            Ok(())
        }
        fn resize(&self, _id: &str, _cols: u16, _rows: u16) -> Result<(), String> {
            Ok(())
        }
        fn close(&self, id: &str) {
            lock(&self.closed).push(id.to_owned());
        }
    }

    impl Fake {
        fn opens(&self) -> u32 {
            *lock(&self.opens)
        }
        fn sink(&self, index: usize) -> Arc<dyn ShellSink> {
            lock(&self.sinks)[index].clone()
        }
        fn writes(&self) -> Vec<(String, String)> {
            lock(&self.writes).clone()
        }
    }

    #[derive(Default)]
    struct Recorder {
        log: Mutex<Vec<String>>,
    }

    impl ShellSink for Recorder {
        fn data(&self, text: String) {
            lock(&self.log).push(format!("data:{text}"));
        }
        fn exit(&self, code: Option<u32>) {
            lock(&self.log).push(format!("exit:{code:?}"));
        }
    }

    impl SupervisedSink for Recorder {
        fn status(&self, status: Status) {
            lock(&self.log).push(match status {
                Status::Reconnecting { attempt, delay } => {
                    format!("reconnecting:{attempt}:{}ms", delay.as_millis())
                }
                Status::Resumed => "resumed".to_owned(),
            });
        }
    }

    impl Recorder {
        fn log(&self) -> Vec<String> {
            lock(&self.log).clone()
        }
    }

    fn target() -> Target {
        Target {
            label: "t".into(),
            host: "h".into(),
            port: 22,
            user: "u".into(),
            identity: None,
        }
    }

    fn attached() -> SessionOptions {
        SessionOptions {
            attach: Attach::Tmux,
            reconnect: true,
            policy: ReconnectPolicy::default(),
        }
    }

    async fn settle() {
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
    }

    fn rig() -> (Arc<Fake>, Supervisor<Fake>, Arc<Recorder>) {
        let fake = Arc::new(Fake::default());
        (fake.clone(), Supervisor::new(fake), Arc::default())
    }

    #[test]
    fn delays_double_up_to_the_cap() {
        let policy = ReconnectPolicy::default();
        let delays: Vec<u64> = (1..=8)
            .map(|n| policy.delay(n).as_millis() as u64)
            .collect();
        assert_eq!(delays, [500, 1000, 2000, 4000, 8000, 16000, 30000, 30000]);
        assert_eq!(policy.delay(0), Duration::from_millis(500));
        assert_eq!(policy.delay(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn attach_commands_and_settings() {
        assert_eq!(Attach::parse("tmux"), Attach::Tmux);
        assert_eq!(Attach::parse("screen"), Attach::Screen);
        assert_eq!(Attach::parse("off"), Attach::None);
        assert_eq!(Attach::None.command(), None);
        assert_eq!(
            Attach::Tmux.command(),
            Some(" exec tmux new -A -s anixops\r")
        );
        assert_eq!(
            Attach::Screen.command(),
            Some(" exec screen -dRR anixops\r")
        );
    }

    #[test]
    fn only_connect_failures_that_retrying_cannot_fix_are_fatal() {
        for code in [
            "auth_failed",
            "cancelled",
            "hostkey_changed",
            "hostkey_rejected",
        ] {
            assert!(is_fatal(code), "{code}");
        }
        for code in ["network", "ssh_error", "unsupported"] {
            assert!(!is_fatal(code), "{code}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_attached_session_types_the_attach_command_once_the_shell_is_up() {
        let (fake, supervisor, sink) = rig();
        let id = supervisor
            .open(target(), 80, 24, sink.clone(), attached())
            .await
            .unwrap();
        assert_eq!(
            fake.writes(),
            vec![("i1".into(), " exec tmux new -A -s anixops\r".into())]
        );
        supervisor.write(&id, b"ls\r").unwrap();
        assert_eq!(fake.writes().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_plain_session_types_nothing() {
        let (fake, supervisor, sink) = rig();
        supervisor
            .open(target(), 80, 24, sink, SessionOptions::default())
            .await
            .unwrap();
        assert!(fake.writes().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_connection_reconnects_attaches_again_and_keeps_the_id() {
        let (fake, supervisor, sink) = rig();
        let id = supervisor
            .open(target(), 80, 24, sink.clone(), attached())
            .await
            .unwrap();
        fake.sink(0).data("before".into());
        fake.sink(0).exit(None);
        settle().await;
        assert_eq!(sink.log(), ["data:before", "reconnecting:1:500ms"]);
        assert!(
            supervisor.write(&id, b"x").is_err(),
            "no shell mid-reconnect"
        );

        tokio::time::advance(Duration::from_millis(500)).await;
        settle().await;
        assert_eq!(fake.opens(), 2);
        assert_eq!(
            sink.log(),
            ["data:before", "reconnecting:1:500ms", "resumed"]
        );
        assert_eq!(
            fake.writes()[1],
            ("i2".into(), " exec tmux new -A -s anixops\r".into())
        );
        supervisor.write(&id, b"ls").unwrap();
        assert_eq!(fake.writes()[2].0, "i2");
        fake.sink(1).data("after".into());
        assert_eq!(sink.log().last().unwrap(), "data:after");
        assert_eq!(supervisor.open_ids(), vec![id]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_clean_exit_ends_the_session_without_reconnecting() {
        let (fake, supervisor, sink) = rig();
        supervisor
            .open(target(), 80, 24, sink.clone(), attached())
            .await
            .unwrap();
        fake.sink(0).exit(Some(0));
        settle().await;
        assert_eq!(sink.log(), ["exit:Some(0)"]);
        assert!(supervisor.open_ids().is_empty());
        assert_eq!(fake.opens(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_plain_session_does_not_reconnect_by_itself() {
        let (fake, supervisor, sink) = rig();
        let options = SessionOptions {
            reconnect: true,
            ..SessionOptions::default()
        };
        supervisor
            .open(target(), 80, 24, sink.clone(), options)
            .await
            .unwrap();
        fake.sink(0).exit(None);
        settle().await;
        assert_eq!(sink.log(), ["exit:None"]);
        assert_eq!(fake.opens(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_attempts_back_off_and_the_session_ends_after_the_last() {
        let (fake, supervisor, sink) = rig();
        let mut options = attached();
        options.policy.max_attempts = 3;
        supervisor
            .open(target(), 80, 24, sink.clone(), options)
            .await
            .unwrap();
        lock(&fake.script).extend([Err("network"), Err("network"), Err("network")]);
        fake.sink(0).exit(None);
        for _ in 0..3 {
            settle().await;
            tokio::time::advance(Duration::from_secs(30)).await;
        }
        settle().await;
        assert_eq!(
            sink.log(),
            [
                "reconnecting:1:500ms",
                "reconnecting:2:1000ms",
                "reconnecting:3:2000ms",
                "exit:None"
            ]
        );
        assert_eq!(fake.opens(), 4);
        assert!(supervisor.open_ids().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_password_stops_the_retries_at_once() {
        let (fake, supervisor, sink) = rig();
        supervisor
            .open(target(), 80, 24, sink.clone(), attached())
            .await
            .unwrap();
        lock(&fake.script).push_back(Err("auth_failed"));
        fake.sink(0).exit(None);
        settle().await;
        tokio::time::advance(Duration::from_millis(500)).await;
        settle().await;
        assert_eq!(sink.log(), ["reconnecting:1:500ms", "exit:None"]);
        assert_eq!(fake.opens(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn closing_while_waiting_cancels_the_reconnect_and_ends_once() {
        let (fake, supervisor, sink) = rig();
        let id = supervisor
            .open(target(), 80, 24, sink.clone(), attached())
            .await
            .unwrap();
        fake.sink(0).exit(None);
        settle().await;
        supervisor.close(&id);
        tokio::time::advance(Duration::from_secs(60)).await;
        settle().await;
        assert_eq!(sink.log(), ["reconnecting:1:500ms", "exit:None"]);
        assert_eq!(fake.opens(), 1);
        assert!(supervisor.open_ids().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn closing_a_live_session_closes_the_shell_and_the_exit_reaches_the_ui() {
        let (fake, supervisor, sink) = rig();
        let id = supervisor
            .open(target(), 80, 24, sink.clone(), attached())
            .await
            .unwrap();
        supervisor.close(&id);
        assert_eq!(*lock(&fake.closed), ["i1"]);
        // The engine answers a close with exit(None); it must not reconnect.
        fake.sink(0).exit(None);
        settle().await;
        assert_eq!(sink.log(), ["exit:None"]);
        assert_eq!(fake.opens(), 1);
        supervisor.close(&id);
        supervisor.close("never-existed");
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_on_demand_replaces_the_shell_without_waiting_and_ignores_the_old_one() {
        let (fake, supervisor, sink) = rig();
        let id = supervisor
            .open(target(), 80, 24, sink.clone(), attached())
            .await
            .unwrap();
        supervisor.reconnect(&id).unwrap();
        settle().await;
        assert_eq!(*lock(&fake.closed), ["i1"]);
        assert_eq!(fake.opens(), 2);
        assert_eq!(sink.log(), ["reconnecting:1:0ms", "resumed"]);
        // The old shell finishing late must not end or restart anything.
        fake.sink(0).data("stale".into());
        fake.sink(0).exit(None);
        settle().await;
        assert_eq!(fake.opens(), 2);
        assert_eq!(sink.log(), ["reconnecting:1:0ms", "resumed"]);
    }

    #[tokio::test(start_paused = true)]
    async fn only_an_attached_session_can_reconnect_on_demand() {
        let (_fake, supervisor, sink) = rig();
        let id = supervisor
            .open(target(), 80, 24, sink, SessionOptions::default())
            .await
            .unwrap();
        assert!(supervisor.reconnect(&id).is_err());
        assert!(supervisor.reconnect("nope").is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_first_connect_is_an_error_and_leaves_nothing_behind() {
        let (fake, supervisor, sink) = rig();
        lock(&fake.script).push_back(Err("network"));
        let error = supervisor
            .open(target(), 80, 24, sink, attached())
            .await
            .unwrap_err();
        assert_eq!(error.code, "network");
        assert!(supervisor.open_ids().is_empty());
    }
}
