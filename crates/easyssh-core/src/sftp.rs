//! Remote file access through the system OpenSSH `sftp` client.
//!
//! Browsing uses one long-lived `sftp` process per host with piped stdio. In
//! that mode `sftp` echoes every command as `sftp> <command>` and line-buffers
//! its output, so each command is followed by a marker (`lpwd`, which we never
//! send otherwise) and its output is everything before the marker's echo.
//! Transfers run in their own `sftp -oBatchMode=no -b -` process so they can be cancelled.
//!
//! These processes have no terminal, so OpenSSH asks for passwords and
//! host-key confirmations through `SSH_ASKPASS`; callers pass that
//! environment in. Answers go straight to OpenSSH and are never stored.

use crate::domain::{Connection, ConnectionTarget};
use crate::openssh::{OpenSsh, OpenSshError};
use crate::process::hide_window;
use crate::security::{validate_alias, validate_connection, ValidationError};
use serde::Serialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

const MARKER: &str = "lpwd";
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a listing with no lines waits for `sftp` to report an error.
const LATE_ERROR_WAIT: Duration = Duration::from_millis(500);
/// Long enough to type a password into the prompt.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteFileKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFile {
    pub name: String,
    pub kind: RemoteFileKind,
    pub size: u64,
    /// As printed by the server, e.g. `Oct 14 09:12` or `Jan  3  2025`.
    pub modified: String,
    pub permissions: String,
}

/// Arguments shared by browsing and transfer processes.
pub fn sftp_args(connection: &Connection, batch: bool) -> Result<Vec<String>, OpenSshError> {
    validate_connection(connection)?;
    let mut args: Vec<String> = vec!["-q".into()];
    if batch {
        // `-b` makes sftp append `-obatchmode yes` to the ssh arguments at the
        // position of `-b`, and ssh keeps the first value it sees for an
        // option. `BatchMode=no` must therefore come before `-b`, otherwise
        // ssh never asks for a password and `SSH_ASKPASS` is not used.
        args.extend(["-oBatchMode=no".into(), "-b".into(), "-".into()]);
    }
    if let Some(jump) = &connection.proxy_jump {
        validate_alias(jump)?;
        args.extend(["-J".into(), jump.clone()]);
    }
    match &connection.target {
        ConnectionTarget::Alias { alias } => args.push(alias.clone()),
        ConnectionTarget::Endpoint {
            hostname,
            username,
            port,
        } => {
            args.extend(["-P".into(), port.to_string()]);
            let host = if hostname.contains(':') {
                format!("[{hostname}]")
            } else {
                hostname.clone()
            };
            match username.as_deref().filter(|user| !user.is_empty()) {
                Some(user) => args.push(format!("{user}@{host}")),
                None => args.push(host),
            }
        }
        ConnectionTarget::Serial(_) => return Err(ValidationError::NotSsh.into()),
    }
    Ok(args)
}

/// Quotes a path as one `sftp` argument. Inside double quotes `sftp` treats
/// glob characters literally, so only `"` and `\` need escaping. Line breaks
/// and other control characters are rejected because they would end the
/// command and start another.
pub fn quote(path: &str) -> Result<String, OpenSshError> {
    if path.is_empty() || path.chars().any(char::is_control) {
        return Err(OpenSshError::Failed(format!("unsupported path: {path:?}")));
    }
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for ch in path.chars() {
        if ch == '"' || ch == '\\' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    Ok(quoted)
}

/// Joins a remote directory and a name with `/`.
pub fn join_remote(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parses `ls -la` output, skipping `.` and `..`.
pub fn parse_listing(output: &str) -> Vec<RemoteFile> {
    parse_listing_in("", output)
}

/// Parses `ls -la dir` output. Newer `sftp` clients format the lines
/// themselves and print each name with `dir/` in front; that is removed.
fn parse_listing_in(dir: &str, output: &str) -> Vec<RemoteFile> {
    let prefix = if dir.is_empty() || dir.ends_with('/') {
        dir.to_owned()
    } else {
        format!("{dir}/")
    };
    output
        .lines()
        .filter_map(|line| parse_listing_line(line, &prefix))
        .collect()
}

/// The eight leading fields of an `ls -l` line and the name after them.
fn listing_fields(line: &str) -> Option<(Vec<&str>, &str)> {
    // permissions links owner group size month day time-or-year name
    let mut rest = line;
    let mut fields = Vec::with_capacity(8);
    for _ in 0..8 {
        rest = rest.trim_start_matches(' ');
        let end = rest.find(' ')?;
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    let name = rest.strip_prefix(' ')?;
    Some((fields, name))
}

/// Checks that `ls -la dir` listed a folder. `sftp` lists a file as itself
/// rather than failing, and a folder always includes its own `.` entry.
pub fn folder_listing(dir: &str, output: &str) -> Result<Vec<RemoteFile>, String> {
    let lines: Vec<_> = output.lines().filter_map(listing_fields).collect();
    let is_folder = lines
        .iter()
        .any(|(fields, name)| fields[0].starts_with('d') && (*name == "." || name.ends_with("/.")));
    if !is_folder {
        if let [(fields, name)] = lines.as_slice() {
            let base = dir.trim_end_matches('/').rsplit('/').next().unwrap_or(dir);
            if !fields[0].starts_with('d') && (*name == dir || *name == base) {
                return Err(format!("{dir} is not a folder"));
            }
        }
    }
    Ok(parse_listing_in(dir, output))
}

fn parse_listing_line(line: &str, prefix: &str) -> Option<RemoteFile> {
    let (fields, name) = listing_fields(line)?;
    let name = match name.strip_prefix(prefix) {
        Some(rest) if !prefix.is_empty() => rest,
        _ => name,
    };
    let permissions = fields[0];
    if permissions.len() < 10 || name.is_empty() || name == "." || name == ".." {
        return None;
    }
    let kind = match permissions.as_bytes()[0] {
        b'd' => RemoteFileKind::Directory,
        b'-' => RemoteFileKind::File,
        b'l' => RemoteFileKind::Symlink,
        _ => RemoteFileKind::Other,
    };
    let name = match kind {
        RemoteFileKind::Symlink => name.split(" -> ").next().unwrap_or(name),
        _ => name,
    };
    Some(RemoteFile {
        name: name.to_owned(),
        kind,
        size: fields[4].parse().unwrap_or(0),
        modified: format!("{} {} {}", fields[5], fields[6], fields[7]),
        permissions: permissions.to_owned(),
    })
}

/// Parses the reply to `pwd`.
pub fn parse_pwd(output: &str) -> Option<String> {
    output
        .lines()
        .find_map(|line| line.strip_prefix("Remote working directory: "))
        .map(str::to_owned)
}

enum Line {
    Out(String),
    Err(String),
}

/// A long-lived `sftp` process for browsing one host.
pub struct SftpSession {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<Line>,
    home: String,
}

impl SftpSession {
    /// Starts `sftp` and waits until it answers, so an authentication
    /// failure is reported here rather than on the first listing.
    pub fn connect(
        openssh: &OpenSsh,
        connection: &Connection,
        env: &[(String, String)],
    ) -> Result<Self, OpenSshError> {
        let mut child = hide_window(&mut Command::new(openssh.sftp_path()?))
            .args(sftp_args(connection, false)?)
            .envs(env.iter().cloned())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(OpenSshError::Failed("sftp pipes are unavailable".into()));
        };
        let (sender, lines) = mpsc::channel();
        forward_lines(stdout, sender.clone(), Line::Out);
        forward_lines(stderr, sender, Line::Err);
        let mut session = Self {
            child,
            stdin,
            lines,
            home: String::new(),
        };
        let output = session.run_for("pwd", LOGIN_TIMEOUT)?;
        session.home = parse_pwd(&output)
            .ok_or_else(|| OpenSshError::Failed("sftp did not report a directory".into()))?;
        Ok(session)
    }

    pub fn home(&self) -> &str {
        &self.home
    }

    pub fn list(&mut self, dir: &str) -> Result<Vec<RemoteFile>, OpenSshError> {
        let output = self.run(&format!("ls -la {}", quote(dir)?))?;
        if output.trim().is_empty() {
            // `ls` of a missing folder says so on standard error, which can
            // arrive well after the reply; an empty folder still lists `.`.
            if let Some(error) = self.late_error(LATE_ERROR_WAIT) {
                return Err(OpenSshError::Failed(error));
            }
        }
        folder_listing(dir, &output).map_err(OpenSshError::Failed)
    }

    /// A standard-error line that arrives within `wait`.
    fn late_error(&mut self, wait: Duration) -> Option<String> {
        let deadline = std::time::Instant::now() + wait;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(Line::Err(line)) if !line.trim().is_empty() => {
                    return Some(line.trim().to_owned())
                }
                Ok(_) => {}
                Err(_) => return None,
            }
        }
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), OpenSshError> {
        self.run(&format!("mkdir {}", quote(path)?)).map(drop)
    }

    pub fn remove(&mut self, path: &str, directory: bool) -> Result<(), OpenSshError> {
        let command = if directory { "rmdir" } else { "rm" };
        self.run(&format!("{command} {}", quote(path)?)).map(drop)
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), OpenSshError> {
        self.run(&format!("rename {} {}", quote(from)?, quote(to)?))
            .map(drop)
    }

    /// Runs one command and returns its standard output. Anything the
    /// command writes to standard error is treated as a failure.
    fn run(&mut self, command: &str) -> Result<String, OpenSshError> {
        self.run_for(command, REPLY_TIMEOUT)
    }

    fn run_for(&mut self, command: &str, timeout: Duration) -> Result<String, OpenSshError> {
        // Anything still queued belongs to an earlier command, such as an
        // error that arrived after its reply; it must not fail this one.
        while self.lines.try_recv().is_ok() {}
        writeln!(self.stdin, "{command}\n{MARKER}").map_err(|_| self.ended())?;
        self.stdin.flush().map_err(|_| self.ended())?;
        let echo = format!("sftp> {command}");
        let marker = format!("sftp> {MARKER}");
        let mut output = String::new();
        let mut errors = String::new();
        let mut started = false;
        loop {
            match self.lines.recv_timeout(timeout) {
                Ok(Line::Out(line)) if !started => started = line == echo,
                Ok(Line::Out(line)) if line == marker => break,
                Ok(Line::Out(line)) => {
                    output.push_str(&line);
                    output.push('\n');
                }
                Ok(Line::Err(line)) => {
                    errors.push_str(&line);
                    errors.push('\n');
                }
                Err(RecvTimeoutError::Timeout) => {
                    return Err(OpenSshError::Failed("sftp did not respond".into()))
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let detail = errors.trim();
                    return Err(if detail.is_empty() {
                        self.ended()
                    } else {
                        OpenSshError::Failed(detail.to_owned())
                    });
                }
            }
        }
        // The marker's own reply ("Local working directory: ...") follows.
        // The command's error can still arrive before it and must be kept.
        loop {
            match self.lines.recv_timeout(REPLY_TIMEOUT) {
                Ok(Line::Out(_)) | Err(_) => break,
                Ok(Line::Err(line)) => {
                    errors.push_str(&line);
                    errors.push('\n');
                }
            }
        }
        // Give a late standard-error line from the command a moment to arrive.
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(20)) {
            if let Line::Err(line) = line {
                errors.push_str(&line);
                errors.push('\n');
            }
        }
        if errors.trim().is_empty() {
            Ok(output)
        } else {
            Err(OpenSshError::Failed(errors.trim().to_owned()))
        }
    }

    fn ended(&self) -> OpenSshError {
        OpenSshError::Failed("the sftp session has ended".into())
    }
}

impl Drop for SftpSession {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn forward_lines(
    stream: impl Read + Send + 'static,
    sender: mpsc::Sender<Line>,
    wrap: fn(String) -> Line,
) {
    std::thread::spawn(move || {
        each_line(stream, |line| sender.send(wrap(line)).is_ok());
    });
}

/// Calls `line` for every line of `stream`, without the line break, until the
/// stream ends or `line` returns false. Bytes that are not UTF-8 (a name in
/// Latin-1, say) are replaced rather than ending the stream, which would
/// leave the session without replies.
fn each_line(stream: impl Read, mut line: impl FnMut(String) -> bool) {
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        match reader.read_until(b'\n', &mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let text = String::from_utf8_lossy(&buffer);
        let text = text.trim_end_matches('\n').trim_end_matches('\r');
        if !line(text.to_owned()) {
            break;
        }
    }
}

/// Starts a transfer in its own `sftp -oBatchMode=no -b -` process. The caller waits on or
/// kills the returned child; standard error carries the failure reason.
pub fn spawn_transfer(
    openssh: &OpenSsh,
    connection: &Connection,
    upload: bool,
    local: &Path,
    remote: &str,
    env: &[(String, String)],
) -> Result<Child, OpenSshError> {
    let local = local
        .to_str()
        .ok_or_else(|| OpenSshError::Failed("local path is not valid UTF-8".into()))?;
    let command = if upload {
        format!("put -r {} {}\n", quote(local)?, quote(remote)?)
    } else {
        format!("get -r {} {}\n", quote(remote)?, quote(local)?)
    };
    let mut child = hide_window(&mut Command::new(openssh.sftp_path()?))
        .args(sftp_args(connection, true)?)
        .envs(env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(command.as_bytes())?;
    }
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_that_are_not_utf8_do_not_end_the_stream() {
        // "caf\xe9.txt" is Latin-1; the marker after it must still arrive.
        let input: &[u8] = b"-rw-r--r-- 1 a a 1 Oct 13 18:40 caf\xe9.txt\r\nsftp> lpwd\nlast";
        let mut lines = Vec::new();
        each_line(input, |line| {
            lines.push(line);
            true
        });
        assert_eq!(
            lines,
            [
                "-rw-r--r-- 1 a a 1 Oct 13 18:40 caf\u{fffd}.txt",
                "sftp> lpwd",
                "last"
            ]
        );
        let mut first = Vec::new();
        each_line(input, |line| {
            first.push(line);
            false
        });
        assert_eq!(first.len(), 1);
    }

    #[test]
    fn quotes_paths_as_single_arguments() {
        assert_eq!(quote("/srv/my app").unwrap(), "\"/srv/my app\"");
        assert_eq!(quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
        assert!(quote("a\nrm -r /").is_err());
        assert!(quote("").is_err());
    }

    #[test]
    fn parses_long_listing() {
        let output = "\
drwxr-xr-x    5 deploy   deploy       4096 Oct 14 09:12 .
drwxr-xr-x    3 root     root         4096 Jan  3  2025 ..
drwxr-xr-x    2 deploy   deploy       4096 Oct 14 09:12 app
-rw-r--r--    1 deploy   deploy      12034 Oct 13 18:40 deploy notes.md
lrwxrwxrwx    1 deploy   deploy         11 Sep  2 11:05 current -> releases/42
-rwxr-xr-x    1 1001     1001         2048 Jan  3  2025  leading space
";
        let files = parse_listing(output);
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].name, "app");
        assert_eq!(files[0].kind, RemoteFileKind::Directory);
        assert_eq!(files[1].name, "deploy notes.md");
        assert_eq!(files[1].size, 12034);
        assert_eq!(files[1].modified, "Oct 13 18:40");
        assert_eq!(files[2].name, "current");
        assert_eq!(files[2].kind, RemoteFileKind::Symlink);
        assert_eq!(files[3].name, " leading space");
        assert_eq!(files[3].modified, "Jan 3 2025");
    }

    #[test]
    fn a_file_is_not_listed_as_a_folder() {
        let folder = "\
drwxr-xr-x    2 deploy   deploy       4096 Oct 14 09:12 .
drwxr-xr-x    3 root     root         4096 Jan  3  2025 ..
-rw-r--r--    1 deploy   deploy         12 Oct 13 18:40 run.py
";
        assert_eq!(folder_listing("/srv/tools", folder).unwrap().len(), 1);
        // A folder holding one file of its own name is still a folder.
        let same = folder.replace("run.py", "tools");
        assert_eq!(folder_listing("/srv/tools", &same).unwrap().len(), 1);
        let file = "-rw-r--r--    1 deploy   deploy         12 Oct 13 18:40 /srv/tools/run.py\n";
        let error = folder_listing("/srv/tools/run.py", file).unwrap_err();
        assert!(error.contains("is not a folder"), "{error}");
        let file = "-rw-r--r--    1 deploy   deploy         12 Oct 13 18:40 run.py\n";
        assert!(folder_listing("/srv/tools/run.py", file).is_err());
        assert!(folder_listing("/srv/empty", "").unwrap().is_empty());
    }

    #[test]
    fn names_lose_the_folder_newer_sftp_prints() {
        // OpenSSH 9.x `sftp` formats long listings itself, with full paths.
        let output = "\
drwxr-xr-x    ? deploy   deploy       4096 Oct 14 09:12 /srv/my app/.
drwxr-xr-x    ? root     root         4096 Jan  3  2025 /srv/my app/..
-rw-r--r--    ? deploy   deploy         12 Oct 13 18:40 /srv/my app/run.py
lrwxrwxrwx    ? deploy   deploy         11 Sep  2 11:05 /srv/my app/current -> releases/42
";
        for dir in ["/srv/my app", "/srv/my app/"] {
            let files = folder_listing(dir, output).unwrap();
            let names: Vec<_> = files.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(names, ["run.py", "current"], "{dir}");
        }
        let root = "drwxr-xr-x    ? root root 4096 Oct 14 09:12 /.\n-rw-r--r--    ? root root 1 Oct 14 09:12 /etc\n";
        let names: Vec<_> = folder_listing("/", root)
            .unwrap()
            .into_iter()
            .map(|f| f.name)
            .collect();
        assert_eq!(names, ["etc"]);
    }

    #[test]
    fn ignores_noise_lines() {
        assert!(parse_listing("Can't ls: \"/nope\" not found\n\n").is_empty());
    }

    #[test]
    fn parses_pwd_reply() {
        assert_eq!(
            parse_pwd("Remote working directory: /home/deploy\n").as_deref(),
            Some("/home/deploy")
        );
        assert_eq!(parse_pwd("nothing"), None);
    }

    #[test]
    fn joins_remote_paths() {
        assert_eq!(join_remote("/", "etc"), "/etc");
        assert_eq!(join_remote("/home/deploy", "app"), "/home/deploy/app");
    }

    #[test]
    fn batch_transfers_allow_askpass_before_batch_flag() {
        let connection = Connection::alias("x", "prod");
        let args = sftp_args(&connection, true).unwrap();
        let mode = args.iter().position(|a| a == "-oBatchMode=no").unwrap();
        let batch = args.iter().position(|a| a == "-b").unwrap();
        assert!(mode < batch, "{args:?}");
        assert!(!sftp_args(&connection, false)
            .unwrap()
            .contains(&"-b".to_string()));
    }

    #[test]
    fn endpoint_arguments_use_uppercase_port_and_bracket_ipv6() {
        let connection = Connection {
            target: ConnectionTarget::Endpoint {
                hostname: "2001:db8::1".into(),
                username: Some("ops".into()),
                port: 2222,
            },
            ..Connection::alias("x", "unused")
        };
        let args = sftp_args(&connection, true).unwrap();
        assert_eq!(
            args,
            [
                "-q",
                "-oBatchMode=no",
                "-b",
                "-",
                "-P",
                "2222",
                "ops@[2001:db8::1]"
            ]
        );
    }
}
