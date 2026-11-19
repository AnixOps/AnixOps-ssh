//! Command execution and interactive shells on a session.

use crate::error::{Error, Result};
use crate::session::Session;
use russh::client::Msg;
use russh::{ChannelMsg, ChannelWriteHalf};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// How a remote command or shell ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitStatus {
    Code(u32),
    Signal(String),
    /// The channel closed without the server reporting a status.
    Unknown,
}

impl ExitStatus {
    pub fn code(&self) -> Option<u32> {
        match self {
            ExitStatus::Code(code) => Some(*code),
            _ => None,
        }
    }

    pub fn success(&self) -> bool {
        self.code() == Some(0)
    }

    /// How the end is told to the UI: `exit` (the shell reported a status),
    /// `signal` (it was killed) or `disconnected` (no status: the
    /// connection dropped or the channel was closed).
    pub fn reason(&self) -> &'static str {
        match self {
            ExitStatus::Code(_) => "exit",
            ExitStatus::Signal(_) => "signal",
            ExitStatus::Unknown => "disconnected",
        }
    }

    /// The signal's name without the `SIG` prefix (`TERM`), for `Signal`.
    pub fn signal(&self) -> Option<&str> {
        match self {
            ExitStatus::Signal(name) => Some(name),
            _ => None,
        }
    }
}

/// A signal's name as the SSH protocol spells it (`TERM`, not `SIGTERM`).
fn signal_name(signal: &russh::Sig) -> String {
    match signal {
        russh::Sig::Custom(name) => name.trim_start_matches("SIG").to_owned(),
        other => format!("{other:?}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit: ExitStatus,
}

impl ExecOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

#[derive(Debug, Clone)]
pub struct ShellOptions {
    pub term: String,
    pub cols: u32,
    pub rows: u32,
}

impl Default for ShellOptions {
    fn default() -> Self {
        Self {
            term: "xterm-256color".into(),
            cols: 80,
            rows: 24,
        }
    }
}

/// Waits for the server's yes or no to a request sent with `want_reply`.
async fn expect_reply(channel: &mut russh::Channel<Msg>, what: &str) -> Result<()> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Failure) => {
                return Err(Error::Channel(format!("the server refused {what}")))
            }
            Some(ChannelMsg::Close) | None => {
                return Err(Error::Channel(format!("channel closed during {what}")))
            }
            Some(_) => {}
        }
    }
}

impl Session {
    /// Runs a command and collects its output. Has no time limit of its
    /// own; wrap it in `tokio::time::timeout` if needed.
    pub async fn exec(&self, command: &str) -> Result<ExecOutput> {
        let mut channel = self.open_session_channel().await?;
        channel.exec(true, command.as_bytes().to_vec()).await?;
        let mut output = ExecOutput {
            stdout: Vec::new(),
            stderr: Vec::new(),
            exit: ExitStatus::Unknown,
        };
        let mut requested = false;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Success) => requested = true,
                Some(ChannelMsg::Failure) => {
                    return Err(Error::Channel(
                        "the server refused to run the command".into(),
                    ))
                }
                Some(ChannelMsg::Data { data }) => output.stdout.extend_from_slice(&data),
                Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                    output.stderr.extend_from_slice(&data)
                }
                Some(ChannelMsg::ExitStatus { exit_status }) => {
                    output.exit = ExitStatus::Code(exit_status)
                }
                Some(ChannelMsg::ExitSignal { signal_name, .. }) => {
                    output.exit = ExitStatus::Signal(self::signal_name(&signal_name))
                }
                Some(ChannelMsg::Close) | None => break,
                Some(_) => {}
            }
        }
        let _ = requested;
        Ok(output)
    }

    /// Opens an interactive shell on a pseudo-terminal.
    pub async fn shell(&self, options: ShellOptions) -> Result<ShellHandle> {
        let mut channel = self.open_session_channel().await?;
        channel
            .request_pty(true, &options.term, options.cols, options.rows, 0, 0, &[])
            .await?;
        expect_reply(&mut channel, "the terminal").await?;
        channel.request_shell(true).await?;
        expect_reply(&mut channel, "the shell").await?;

        let (mut reader, writer) = channel.split();
        let (output_tx, output) = mpsc::channel::<Vec<u8>>(64);
        let (exit_tx, exit) = watch::channel(None);
        let task = tokio::spawn(async move {
            let mut status = ExitStatus::Unknown;
            while let Some(message) = reader.wait().await {
                match message {
                    ChannelMsg::Data { data } | ChannelMsg::ExtendedData { data, .. } => {
                        if output_tx.send(data.to_vec()).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::ExitStatus { exit_status } => {
                        status = ExitStatus::Code(exit_status)
                    }
                    ChannelMsg::ExitSignal { signal_name, .. } => {
                        status = ExitStatus::Signal(self::signal_name(&signal_name))
                    }
                    ChannelMsg::Close => break,
                    _ => {}
                }
            }
            let _ = exit_tx.send(Some(status));
        });
        Ok(ShellHandle {
            writer,
            output,
            exit,
            task,
        })
    }
}

/// An interactive shell. Output of the terminal (stdout and stderr merged,
/// as on a PTY) arrives through [`ShellHandle::read`].
pub struct ShellHandle {
    writer: ChannelWriteHalf<Msg>,
    output: mpsc::Receiver<Vec<u8>>,
    exit: watch::Receiver<Option<ExitStatus>>,
    task: JoinHandle<()>,
}

impl ShellHandle {
    /// Sends keystrokes or pasted text.
    pub async fn write(&self, data: &[u8]) -> Result<()> {
        self.writer.data(data).await?;
        Ok(())
    }

    /// Tells the server the terminal changed size.
    pub async fn resize(&self, cols: u32, rows: u32) -> Result<()> {
        self.writer.window_change(cols, rows, 0, 0).await?;
        Ok(())
    }

    /// The next chunk of terminal output; `None` once the shell has ended
    /// and everything was read.
    pub async fn read(&mut self) -> Option<Vec<u8>> {
        self.output.recv().await
    }

    /// Waits for the shell to end and returns how.
    pub async fn wait_exit(&mut self) -> ExitStatus {
        match self.exit.wait_for(Option::is_some).await {
            Ok(status) => status.clone().unwrap_or(ExitStatus::Unknown),
            Err(_) => ExitStatus::Unknown,
        }
    }

    /// Ends the shell by closing its channel.
    pub async fn close(&self) -> Result<()> {
        self.writer.eof().await?;
        self.writer.close().await?;
        Ok(())
    }
}

impl Drop for ShellHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::{signal_name, ExitStatus};

    #[test]
    fn tells_an_exit_from_a_signal_from_a_lost_connection() {
        assert_eq!(ExitStatus::Code(3).reason(), "exit");
        assert_eq!(ExitStatus::Code(3).signal(), None);
        let killed = ExitStatus::Signal("KILL".into());
        assert_eq!(killed.reason(), "signal");
        assert_eq!(killed.signal(), Some("KILL"));
        assert_eq!(killed.code(), None);
        assert_eq!(ExitStatus::Unknown.reason(), "disconnected");
    }

    #[test]
    fn names_signals_without_the_sig_prefix() {
        assert_eq!(signal_name(&russh::Sig::TERM), "TERM");
        assert_eq!(signal_name(&russh::Sig::Custom("SIGWINCH".into())), "WINCH");
        assert_eq!(signal_name(&russh::Sig::Custom("XCPU".into())), "XCPU");
    }
}
