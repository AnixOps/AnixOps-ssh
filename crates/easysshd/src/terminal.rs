//! Terminal sessions: a PTY running a command (the system `ssh` or a local
//! shell), a scrollback ring buffer, and the clients attached to it.
//!
//! Output is decoded to UTF-8 text once, as the PTY produces it, and every
//! byte of that text has an offset (`seq`) in the session's output stream.
//! Live events, the scrollback and `from_seq` all use these offsets, so a
//! client that attaches late replays exactly what it missed.

use crate::server::Outbox;
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::Duration;

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

/// Turns a byte stream into UTF-8 text, keeping a multi-byte character that
/// is split across reads until its remaining bytes arrive.
#[derive(Default)]
pub struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(valid) => {
                    text.push_str(valid);
                    self.pending.clear();
                    return text;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(&String::from_utf8_lossy(&self.pending[..valid]));
                    match error.error_len() {
                        Some(invalid) => {
                            text.push(char::REPLACEMENT_CHARACTER);
                            self.pending.drain(..valid + invalid);
                        }
                        None => {
                            self.pending.drain(..valid);
                            return text;
                        }
                    }
                }
            }
        }
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

struct Subscriber {
    conn: u64,
    outbox: Outbox,
}

struct State {
    scrollback: Scrollback,
    subscribers: Vec<Subscriber>,
    /// `Some(code)` once the process has ended.
    exit: Option<Option<u32>>,
    cols: u16,
    rows: u16,
}

struct Io {
    master: Option<Box<dyn MasterPty + Send>>,
    writer: Option<Box<dyn Write + Send>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
}

/// Called once with the session id and exit code when the process has ended.
pub type ExitHook = Box<dyn Fn(&str, Option<u32>) + Send + Sync>;

/// One PTY session.
pub struct TermSession {
    on_exit: ExitHook,
    id: String,
    host_id: Option<String>,
    state: Mutex<State>,
    io: Mutex<Io>,
}

impl TermSession {
    /// Start `command` on a new PTY and begin buffering its output.
    pub fn spawn(
        id: String,
        host_id: Option<String>,
        command: CommandBuilder,
        cols: u16,
        rows: u16,
        scrollback: usize,
        on_exit: ExitHook,
    ) -> Result<Arc<TermSession>, String> {
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
            state: Mutex::new(State {
                scrollback: Scrollback::new(scrollback),
                subscribers: Vec::new(),
                exit: None,
                cols: size.cols,
                rows: size.rows,
            }),
            io: Mutex::new(Io {
                master: Some(pair.master),
                writer: Some(writer),
                killer,
            }),
        });

        let (exit_tx, exit_rx) = mpsc::channel();
        let waiter = session.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().map(|status| status.exit_code());
            let _ = exit_tx.send(code);
            // On Windows the ConPTY keeps the reader open after the child
            // exits; dropping the PTY ends it. Elsewhere the reader has its
            // own handle and still drains what is buffered.
            let mut io = lock(&waiter.io);
            io.writer = None;
            io.master = None;
        });
        let pump = session.clone();
        std::thread::spawn(move || pump.pump(reader, &exit_rx));
        Ok(session)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    fn pump(&self, mut reader: Box<dyn Read + Send>, exit: &mpsc::Receiver<Option<u32>>) {
        let mut buffer = [0u8; 16 * 1024];
        let mut decoder = Utf8Stream::default();
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let text = decoder.push(&buffer[..read]);
                    if !text.is_empty() {
                        self.output(&text);
                    }
                }
            }
        }
        let code = exit.recv_timeout(Duration::from_secs(3)).ok().flatten();
        self.finish(code);
    }

    fn output(&self, text: &str) {
        let mut state = lock(&self.state);
        let seq = state.scrollback.push(text);
        let payload = json!({"session_id": self.id, "data": text, "seq": seq});
        state
            .subscribers
            .retain(|s| s.outbox.event("session.output", payload.clone()));
    }

    fn finish(&self, code: Option<u32>) {
        let mut state = lock(&self.state);
        if state.exit.is_some() {
            return;
        }
        state.exit = Some(code);
        (self.on_exit)(&self.id, code);
        let payload = json!({"session_id": self.id, "code": code});
        for s in state.subscribers.drain(..) {
            s.outbox.event("session.exit", payload.clone());
        }
    }

    /// `session.write`: raw bytes to the child's terminal input.
    pub fn write(&self, data: &[u8]) -> Result<(), String> {
        let mut io = lock(&self.io);
        let writer = io.writer.as_mut().ok_or("session has ended")?;
        writer
            .write_all(data)
            .and_then(|()| writer.flush())
            .map_err(|e| e.to_string())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        let size = pty_size(cols, rows);
        {
            let io = lock(&self.io);
            let Some(master) = io.master.as_ref() else {
                return Ok(());
            };
            master.resize(size).map_err(|e| e.to_string())?;
        }
        let mut state = lock(&self.state);
        state.cols = size.cols;
        state.rows = size.rows;
        Ok(())
    }

    /// Kill the child. The exit event follows when the process is gone.
    pub fn kill(&self) {
        let _ = lock(&self.io).killer.kill();
    }

    pub fn is_running(&self) -> bool {
        lock(&self.state).exit.is_none()
    }

    /// The `SessionInfo` entity.
    pub fn info(&self) -> Value {
        let state = lock(&self.state);
        json!({
            "session_id": self.id,
            "host_id": self.host_id,
            "cols": state.cols,
            "rows": state.rows,
            "running": state.exit.is_none(),
            "exit_code": state.exit.flatten(),
            "first_seq": state.scrollback.first(),
            "next_seq": state.scrollback.next(),
        })
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
        match state.exit {
            Some(code) => {
                ok &= outbox.event("session.exit", json!({"session_id": self.id, "code": code}));
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(stream.push(b"a\xffb"), "a\u{FFFD}b");
    }
}
