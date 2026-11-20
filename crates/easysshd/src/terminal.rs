//! Terminal sessions: a PTY running a command (the system `ssh` or a local
//! shell), a scrollback ring buffer, and the clients attached to it.
//!
//! Output is decoded to UTF-8 text once, as the PTY produces it, and every
//! byte of that text has an offset (`seq`) in the session's output stream.
//! Live events, the scrollback and `from_seq` all use these offsets, so a
//! client that attaches late replays exactly what it missed.

use crate::integration::{self, Action, Integration};
use crate::recorder::Recorder;
use crate::server::Outbox;
use easyssh_engine::Utf8Stream;
use easyssh_ssh::ExitStatus;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::mpsc as async_mpsc;

/// Default scrollback per session, in bytes of UTF-8 text.
pub const DEFAULT_SCROLLBACK: usize = 1024 * 1024;

/// Stored chunks are merged until they reach this size, so a session that
/// prints a few bytes at a time still replays in a modest number of events.
const MERGE_LIMIT: usize = 8 * 1024;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// A ring buffer of output text addressed by stream offset.
#[derive(Debug)]
pub struct Scrollback {
    cap: usize,
    /// `(offset of first byte, text)`, contiguous, oldest first.
    chunks: VecDeque<(u64, String)>,
    len: usize,
    /// Offset the next byte will have: total bytes ever pushed.
    next: u64,
}

impl Scrollback {
    pub fn new(cap: usize) -> Self {
        Scrollback {
            cap: cap.max(1),
            chunks: VecDeque::new(),
            len: 0,
            next: 0,
        }
    }

    /// Offset of the oldest byte still held (equals `next` when empty).
    pub fn first(&self) -> u64 {
        self.chunks.front().map_or(self.next, |(start, _)| *start)
    }

    /// Offset the next byte will have.
    pub fn next(&self) -> u64 {
        self.next
    }

    /// Append `data` and return the offset of its first byte.
    pub fn push(&mut self, data: &str) -> u64 {
        let start = self.next;
        if data.is_empty() {
            return start;
        }
        self.next += data.len() as u64;
        match self.chunks.back_mut() {
            Some((_, text)) if text.len() < MERGE_LIMIT => text.push_str(data),
            _ => self.chunks.push_back((start, data.to_owned())),
        }
        self.len += data.len();
        self.trim();
        start
    }

    /// Drop the oldest text until at most `cap` bytes remain, never cutting a
    /// character in half.
    fn trim(&mut self) {
        while self.len > self.cap {
            let excess = self.len - self.cap;
            let Some((start, text)) = self.chunks.front_mut() else {
                break;
            };
            if text.len() <= excess {
                self.len -= text.len();
                self.chunks.pop_front();
                continue;
            }
            let mut cut = excess;
            while !text.is_char_boundary(cut) {
                cut += 1;
            }
            text.drain(..cut);
            *start += cut as u64;
            self.len -= cut;
        }
    }

    /// The buffered text from `from` on, as `(offset, text)` chunks, and
    /// whether bytes before the first returned one were dropped from the
    /// buffer. An offset inside a character starts at the next character.
    pub fn since(&self, from: u64) -> (Vec<(u64, String)>, bool) {
        let first = self.first();
        let truncated = from < first;
        let mut out = Vec::new();
        for (start, text) in &self.chunks {
            let end = start + text.len() as u64;
            if end <= from {
                continue;
            }
            if from <= *start {
                out.push((*start, text.clone()));
            } else {
                let mut cut = (from - start) as usize;
                while !text.is_char_boundary(cut) {
                    cut += 1;
                }
                out.push((start + cut as u64, text[cut..].to_owned()));
            }
        }
        (out, truncated)
    }
}

pub fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.max(1),
        cols: cols.max(1),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// The `session.exit` payload: the code (if any), and why the session ended
/// (`exit`, `signal` with the signal's name, or `disconnected`).
fn exit_payload(session_id: &str, status: &ExitStatus) -> Value {
    let mut payload = json!({
        "session_id": session_id,
        "code": status.code(),
        "reason": status.reason(),
    });
    if let Some(signal) = status.signal() {
        payload["signal"] = json!(signal);
    }
    payload
}

struct Subscriber {
    conn: u64,
    outbox: Outbox,
}

struct State {
    scrollback: Scrollback,
    subscribers: Vec<Subscriber>,
    /// How the process ended, once it has.
    exit: Option<ExitStatus>,
    cols: u16,
    rows: u16,
    /// Present while the opt-in shell integration is being delivered.
    integration: Option<Integration>,
}

struct Io {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

/// Called once with the session id and exit code when the process has ended.
pub type ExitHook = Box<dyn Fn(&str, Option<u32>) + Send + Sync>;

/// What the task that owns a built-in engine shell is told to do.
enum RemoteCmd {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

/// The write side of a serial port, and the flag that tells its reader to stop.
struct SerialIo {
    port: Option<Box<dyn serialport::SerialPort>>,
    closed: Arc<AtomicBool>,
}

enum Backend {
    /// A child process on a PTY (the system `ssh`, or a local shell).
    Pty(Mutex<Io>),
    /// A serial port: the daemon reads and writes the device itself.
    Serial(Mutex<SerialIo>),
    /// A shell channel of the built-in engine, owned by a tokio task.
    Remote(async_mpsc::UnboundedSender<RemoteCmd>),
}

/// Settings of a new session.
pub struct SpawnOptions {
    pub cols: u16,
    pub rows: u16,
    /// Bytes of output kept for replay.
    pub scrollback: usize,
    /// Deliver the opt-in shell integration (see `integration`).
    pub shell_integration: bool,
    /// Writes the output to an encrypted session log when set.
    pub recorder: Option<Arc<Recorder>>,
}

/// One terminal session.
pub struct TermSession {
    on_exit: ExitHook,
    id: String,
    host_id: Option<String>,
    recorder: Option<Arc<Recorder>>,
    state: Mutex<State>,
    io: Backend,
    /// Which engine runs it and why; absent for a local shell.
    engine: Mutex<Option<(String, String)>>,
}

impl TermSession {
    /// Start `command` on a new PTY and begin buffering its output.
    pub fn spawn(
        id: String,
        host_id: Option<String>,
        command: CommandBuilder,
        options: SpawnOptions,
        on_exit: ExitHook,
    ) -> Result<Arc<TermSession>, String> {
        let SpawnOptions {
            cols,
            rows,
            scrollback,
            shell_integration,
            recorder,
        } = options;
        let pair = native_pty_system()
            .openpty(pty_size(cols, rows))
            .map_err(|e| e.to_string())?;
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| e.to_string())?;
        drop(pair.slave);
        let reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
        let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
        let killer = child.clone_killer();
        let size = pty_size(cols, rows);
        let session = Arc::new(TermSession {
            on_exit,
            id,
            host_id,
            recorder,
            state: Mutex::new(State {
                scrollback: Scrollback::new(scrollback),
                subscribers: Vec::new(),
                exit: None,
                cols: size.cols,
                rows: size.rows,
                integration: shell_integration.then(|| Integration::new(Instant::now())),
            }),
            io: Backend::Pty(Mutex::new(Io {
                master: Some(pair.master),
                writer: Some(writer),
                killer,
            })),
            engine: Mutex::new(None),
        });

        let (exit_tx, exit_rx) = mpsc::channel();
        let waiter = session.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code());
            let _ = exit_tx.send(code);
            // On Windows the ConPTY keeps the reader open after the child
            // exits; dropping the PTY ends it. Elsewhere the reader has its
            // own handle and still drains what is buffered.
            if let Backend::Pty(io) = &waiter.io {
                let mut io = lock(io);
                io.writer = None;
                io.master = None;
            }
        });
        let pump = session.clone();
        std::thread::spawn(move || pump.pump(reader, &exit_rx));
        if shell_integration {
            let delivery = session.clone();
            std::thread::spawn(move || delivery.deliver_integration());
        }
        Ok(session)
    }

    /// A session on a serial port. The port is read on its own thread, whose
    /// output takes the same path as a PTY's (scrollback, session log, shell
    /// integration), so everything downstream treats it like any terminal.
    pub fn spawn_serial(
        id: String,
        host_id: Option<String>,
        port: Box<dyn serialport::SerialPort>,
        options: SpawnOptions,
        on_exit: ExitHook,
    ) -> Result<Arc<TermSession>, String> {
        let SpawnOptions {
            cols,
            rows,
            scrollback,
            shell_integration,
            recorder,
        } = options;
        let reader = port.try_clone().map_err(|e| e.to_string())?;
        let closed = Arc::new(AtomicBool::new(false));
        let size = pty_size(cols, rows);
        let session = Arc::new(TermSession {
            on_exit,
            id,
            host_id,
            recorder,
            state: Mutex::new(State {
                scrollback: Scrollback::new(scrollback),
                subscribers: Vec::new(),
                exit: None,
                cols: size.cols,
                rows: size.rows,
                integration: shell_integration.then(|| Integration::new(Instant::now())),
            }),
            io: Backend::Serial(Mutex::new(SerialIo {
                port: Some(port),
                closed: closed.clone(),
            })),
            engine: Mutex::new(None),
        });
        let pump = session.clone();
        std::thread::spawn(move || pump.pump_serial(reader, &closed));
        if shell_integration {
            let delivery = session.clone();
            std::thread::spawn(move || delivery.deliver_integration());
        }
        Ok(session)
    }

    /// A session whose shell is a channel of the built-in engine. A task
    /// owns `shell`: it forwards its output, applies writes and resizes and
    /// ends the session when the shell ends. `keep` is held until then (the
    /// connection lease).
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_remote(
        id: String,
        host_id: Option<String>,
        shell: easyssh_ssh::ShellHandle,
        keep: Box<dyn std::any::Any + Send>,
        options: SpawnOptions,
        on_exit: ExitHook,
    ) -> Arc<TermSession> {
        let SpawnOptions {
            cols,
            rows,
            scrollback,
            shell_integration,
            recorder,
        } = options;
        let (commands, receiver) = async_mpsc::unbounded_channel();
        let size = pty_size(cols, rows);
        let session = Arc::new(TermSession {
            on_exit,
            id,
            host_id,
            recorder,
            state: Mutex::new(State {
                scrollback: Scrollback::new(scrollback),
                subscribers: Vec::new(),
                exit: None,
                cols: size.cols,
                rows: size.rows,
                integration: shell_integration.then(|| Integration::new(Instant::now())),
            }),
            io: Backend::Remote(commands),
            engine: Mutex::new(None),
        });
        let owner = session.clone();
        tokio::spawn(async move {
            let status = owner.run_remote(shell, receiver).await;
            drop(keep);
            owner.finish(status);
        });
        if shell_integration {
            let delivery = session.clone();
            std::thread::spawn(move || delivery.deliver_integration());
        }
        session
    }

    async fn run_remote(
        &self,
        mut shell: easyssh_ssh::ShellHandle,
        mut commands: async_mpsc::UnboundedReceiver<RemoteCmd>,
    ) -> ExitStatus {
        let mut decoder = Utf8Stream::default();
        loop {
            tokio::select! {
                chunk = shell.read() => match chunk {
                    Some(bytes) => {
                        let text = decoder.push(&bytes);
                        if !text.is_empty() {
                            self.output(&text);
                        }
                    }
                    None => break,
                },
                // A held-back lead byte with nothing after it: release it so a
                // binary transfer (rz/sz) never waits on it.
                () = tokio::time::sleep(Duration::from_millis(25)), if decoder.has_pending() => {
                    let text = decoder.flush();
                    if !text.is_empty() {
                        self.output(&text);
                    }
                }
                command = commands.recv() => match command {
                    Some(RemoteCmd::Write(data)) => {
                        if shell.write(&data).await.is_err() {
                            break;
                        }
                    }
                    Some(RemoteCmd::Resize(cols, rows)) => {
                        let _ = shell.resize(u32::from(cols), u32::from(rows)).await;
                    }
                    Some(RemoteCmd::Close) | None => {
                        let _ = shell.close().await;
                        return ExitStatus::Unknown;
                    }
                },
            }
        }
        // A status (exit code or signal) when the server sent one; none
        // means the connection dropped.
        tokio::time::timeout(Duration::from_secs(3), shell.wait_exit())
            .await
            .unwrap_or(ExitStatus::Unknown)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn host_id(&self) -> Option<&str> {
        self.host_id.as_deref()
    }

    /// Records which engine runs the session and why.
    pub fn set_engine(&self, engine: &str, reason: &str) {
        *lock(&self.engine) = Some((engine.to_owned(), reason.to_owned()));
    }

    pub fn engine(&self) -> Option<(String, String)> {
        lock(&self.engine).clone()
    }

    fn pump(&self, mut reader: Box<dyn Read + Send>, exit: &mpsc::Receiver<Option<u32>>) {
        // The blocking read runs on its own thread so a held-back lead byte
        // can be released after a quiet moment (a binary transfer must not
        // wait on it).
        let (chunks, incoming) = mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 16 * 1024];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 || chunks.send(buffer[..read].to_vec()).is_err() {
                    break;
                }
            }
        });
        let mut decoder = Utf8Stream::default();
        loop {
            let wait = if decoder.has_pending() {
                Duration::from_millis(25)
            } else {
                Duration::from_secs(3600)
            };
            match incoming.recv_timeout(wait) {
                Ok(bytes) => {
                    let text = decoder.push(&bytes);
                    if !text.is_empty() {
                        self.output(&text);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let text = decoder.flush();
                    if !text.is_empty() {
                        self.output(&text);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        let code = exit.recv_timeout(Duration::from_secs(3)).ok().flatten();
        self.finish(code.map_or(ExitStatus::Unknown, ExitStatus::Code));
    }

    /// Reads the serial port until it fails (unplugged), reaches its end or
    /// the session is closed. The port has a short read timeout so the
    /// `closed` flag is seen promptly.
    fn pump_serial(&self, mut reader: Box<dyn serialport::SerialPort>, closed: &AtomicBool) {
        let mut buffer = [0u8; 4096];
        let mut decoder = Utf8Stream::default();
        while !closed.load(Ordering::SeqCst) {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let text = decoder.push(&buffer[..read]);
                    if !text.is_empty() {
                        self.output(&text);
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::TimedOut
                            | std::io::ErrorKind::WouldBlock
                            | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => break,
            }
        }
        // A serial line has no exit status.
        self.finish(ExitStatus::Unknown);
    }

    fn output(&self, text: &str) {
        let mut state = lock(&self.state);
        let shown = match state.integration.as_mut() {
            Some(delivery) => {
                let shown = delivery.filter(text, Instant::now());
                if delivery.is_done() {
                    state.integration = None;
                }
                shown
            }
            None => text.to_owned(),
        };
        self.publish(&mut state, &shown);
    }

    fn publish(&self, state: &mut State, text: &str) {
        if text.is_empty() {
            return;
        }
        let seq = state.scrollback.push(text);
        if let Some(recorder) = &self.recorder {
            recorder.write(text);
        }
        let payload = json!({"session_id": self.id, "data": text, "seq": seq});
        state
            .subscribers
            .retain(|s| s.outbox.event("session.output", payload.clone()));
    }

    /// Runs beside the pump while shell integration is pending: waits for a
    /// prompt, types the script (hidden by `output`) and watches for it to
    /// stall. See `integration` for the whole exchange.
    fn deliver_integration(&self) {
        loop {
            std::thread::sleep(Duration::from_millis(50));
            let mut state = lock(&self.state);
            if state.exit.is_some() {
                return;
            }
            let Some(delivery) = state.integration.as_mut() else {
                return;
            };
            let now = Instant::now();
            if let Some(released) = delivery.flush_stalled(now) {
                state.integration = None;
                self.publish(&mut state, &released);
                return;
            }
            match delivery.tick(now) {
                Action::Nothing => {
                    if delivery.is_done() {
                        state.integration = None;
                        return;
                    }
                }
                Action::Inject => {
                    // Wipe the prompt that is on screen: the shell prints a
                    // fresh one when the script has run.
                    self.publish(&mut state, "\r\x1b[2K");
                    drop(state);
                    for line in integration::bootstrap_lines() {
                        if self.write_raw(line.as_bytes()).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }

    fn finish(&self, status: ExitStatus) {
        let mut state = lock(&self.state);
        if state.exit.is_some() {
            return;
        }
        let code = status.code();
        let payload = exit_payload(&self.id, &status);
        state.exit = Some(status);
        if let Some(recorder) = &self.recorder {
            recorder.finish();
        }
        (self.on_exit)(&self.id, code);
        for s in state.subscribers.drain(..) {
            s.outbox.event("session.exit", payload.clone());
        }
    }

    /// `session.write`: raw bytes to the child's terminal input.
    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        if let Some(delivery) = lock(&self.state).integration.as_mut() {
            delivery.note_input();
        }
        self.write_raw(data)
    }

    fn write_raw(&self, data: &[u8]) -> Result<(), String> {
        let io = match &self.io {
            Backend::Pty(io) => io,
            Backend::Serial(serial) => {
                let mut serial = lock(serial);
                let port = serial.port.as_mut().ok_or("session has ended")?;
                return write_serial(port.as_mut(), data);
            }
            Backend::Remote(commands) => {
                return commands
                    .send(RemoteCmd::Write(data.to_vec()))
                    .map_err(|_| "session has ended".to_owned());
            }
        };
        let mut io = lock(io);
        let writer = io.writer.as_mut().ok_or("session has ended")?;
        writer
            .write_all(data)
            .and_then(|()| writer.flush())
            .map_err(|e| e.to_string())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let size = pty_size(cols, rows);
        match &self.io {
            Backend::Pty(io) => {
                let io = lock(io);
                let Some(master) = io.master.as_ref() else {
                    return Ok(());
                };
                master.resize(size).map_err(|e| e.to_string())?;
            }
            // A serial line has no window size; the terminal still wraps.
            Backend::Serial(_) => {}
            Backend::Remote(commands) => {
                // A shell that ended no longer cares.
                let _ = commands.send(RemoteCmd::Resize(size.cols, size.rows));
            }
        }
        let mut state = lock(&self.state);
        state.cols = size.cols;
        state.rows = size.rows;
        Ok(())
    }

    /// Kill the child. The exit event follows when the process is gone.
    pub fn kill(&self) {
        match &self.io {
            Backend::Pty(io) => {
                let _ = lock(io).killer.kill();
            }
            Backend::Serial(serial) => {
                let mut serial = lock(serial);
                serial.closed.store(true, Ordering::SeqCst);
                serial.port = None;
            }
            Backend::Remote(commands) => {
                let _ = commands.send(RemoteCmd::Close);
            }
        }
    }

    pub fn is_running(&self) -> bool {
        lock(&self.state).exit.is_none()
    }

    /// The `SessionInfo` entity.
    pub fn info(&self) -> Value {
        let engine = self.engine();
        let state = lock(&self.state);
        let mut info = json!({
            "session_id": self.id,
            "host_id": self.host_id,
            "cols": state.cols,
            "rows": state.rows,
            "running": state.exit.is_none(),
            "exit_code": state.exit.as_ref().and_then(ExitStatus::code),
            "first_seq": state.scrollback.first(),
            "next_seq": state.scrollback.next(),
        });
        if let Some((engine, reason)) = engine {
            info["engine"] = json!(engine);
            info["engine_reason"] = json!(reason);
        }
        info
    }

    /// Subscribe `conn` to this session. Under one lock it queues, in this
    /// order: the reply to `request_id` (when given), the buffered output from
    /// `from_seq`, `session.exit` if the session already ended, and from then
    /// on every live event. Attaching again replaces the connection's earlier
    /// subscription. `false` when the connection could not take it all.
    pub fn attach(
        &self,
        conn: u64,
        outbox: &Outbox,
        request_id: Option<&str>,
        from_seq: Option<u64>,
    ) -> bool {
        let mut state = lock(&self.state);
        state.subscribers.retain(|s| s.conn != conn);
        let (chunks, truncated) = state.scrollback.since(from_seq.unwrap_or(0));
        let mut ok = true;
        if let Some(id) = request_id {
            let result = json!({
                "first_seq": state.scrollback.first(),
                "next_seq": state.scrollback.next(),
                "truncated": truncated,
                "running": state.exit.is_none(),
            });
            ok &= outbox.try_send(json!({"id": id, "ok": result}));
        }
        for (seq, data) in chunks {
            ok &= outbox.event(
                "session.output",
                json!({"session_id": self.id, "data": data, "seq": seq}),
            );
        }
        match &state.exit {
            Some(status) => {
                ok &= outbox.event("session.exit", exit_payload(&self.id, status));
            }
            None if ok => state.subscribers.push(Subscriber {
                conn,
                outbox: outbox.clone(),
            }),
            None => {}
        }
        ok
    }

    pub fn detach(&self, conn: u64) {
        lock(&self.state).subscribers.retain(|s| s.conn != conn);
    }
}

/// Writes all of `data`, riding out short stalls of a flow-controlled line.
fn write_serial(port: &mut dyn serialport::SerialPort, mut data: &[u8]) -> Result<(), String> {
    let mut stalls = 0;
    while !data.is_empty() {
        match port.write(data) {
            Ok(0) => return Err("the serial port stopped accepting data".to_owned()),
            Ok(written) => {
                data = &data[written..];
                stalls = 0;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) && stalls < 50 =>
            {
                stalls += 1;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    port.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exit_payload_says_why_the_session_ended() {
        assert_eq!(
            exit_payload("s", &ExitStatus::Code(2)),
            json!({"session_id": "s", "code": 2, "reason": "exit"})
        );
        assert_eq!(
            exit_payload("s", &ExitStatus::Signal("KILL".into())),
            json!({"session_id": "s", "code": null, "reason": "signal", "signal": "KILL"})
        );
        assert_eq!(
            exit_payload("s", &ExitStatus::Unknown),
            json!({"session_id": "s", "code": null, "reason": "disconnected"})
        );
    }

    #[test]
    fn push_offsets_and_replay() {
        let mut s = Scrollback::new(100);
        assert_eq!(s.push("hello "), 0);
        assert_eq!(s.push("world"), 6);
        assert_eq!(s.first(), 0);
        assert_eq!(s.next(), 11);
        let (chunks, truncated) = s.since(0);
        assert!(!truncated);
        let all: String = chunks.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(all, "hello world");
        let (chunks, truncated) = s.since(6);
        assert!(!truncated);
        assert_eq!(chunks, vec![(6, "world".to_owned())]);
        assert_eq!(s.since(11).0, vec![]);
    }

    #[test]
    fn drops_oldest_at_the_cap() {
        let mut s = Scrollback::new(10);
        s.push("0123456789");
        s.push("abcde");
        assert_eq!(s.first(), 5);
        assert_eq!(s.next(), 15);
        let (chunks, truncated) = s.since(0);
        assert!(truncated);
        let all: String = chunks.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(all, "56789abcde");
        assert_eq!(chunks[0].0, 5);
        // Asking from inside the retained range is not truncated.
        assert!(!s.since(7).1);
    }

    #[test]
    fn never_splits_a_character() {
        let mut s = Scrollback::new(4);
        s.push("a主机b"); // 1 + 3 + 3 + 1 bytes
        let (chunks, _) = s.since(0);
        let all: String = chunks.iter().map(|(_, t)| t.as_str()).collect();
        assert!(all.len() <= 4 + 3, "{all:?}");
        assert!(all.ends_with('b'));
        // An offset inside a character resumes at the next one.
        let mut s = Scrollback::new(100);
        s.push("主机");
        let (chunks, _) = s.since(1);
        assert_eq!(chunks, vec![(3, "机".to_owned())]);
    }

    #[test]
    fn small_writes_are_merged() {
        let mut s = Scrollback::new(1 << 20);
        for _ in 0..1000 {
            s.push("x");
        }
        assert_eq!(s.chunks.len(), 1);
        assert_eq!(s.next(), 1000);
    }

    #[test]
    fn utf8_stream_keeps_split_characters() {
        let mut stream = Utf8Stream::default();
        let bytes = "主机".as_bytes();
        assert_eq!(stream.push(&bytes[..2]), "");
        assert_eq!(stream.push(&bytes[2..4]), "主");
        assert_eq!(stream.push(&bytes[4..]), "机");
        assert_eq!(
            easyssh_engine::text_to_bytes(&stream.push(b"a\xffb")),
            b"a\xffb"
        );
    }
}
