use crate::domain::{Connection, ConnectionTarget};
use crate::process::hide_window;
use crate::security::{validate_alias, validate_connection, validate_host, ValidationError};
use std::env;
#[cfg(windows)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum OpenSshError {
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error("OpenSSH executable '{0}' was not found on PATH")]
    NotFound(&'static str),
    #[error("OpenSSH process failed to start: {0}")]
    Process(#[from] std::io::Error),
    #[error("OpenSSH command failed: {0}")]
    Failed(String),
    /// The host has an outbound proxy, which only the built-in engine can
    /// use. There is no safe `ProxyCommand` for it (`nc`/`ncat` differ by
    /// system and a proxy password cannot go on a command line), and running
    /// `ssh` without the proxy would connect directly without a word.
    #[error(
        "this host connects through the proxy {0}, which only the built-in engine supports; \
         OpenSSH would connect directly. Use the built-in engine for this host, or remove the proxy"
    )]
    ProxyUnsupported(String),
}

fn refuse_proxy(connection: &Connection) -> Result<(), OpenSshError> {
    match &connection.proxy {
        Some(proxy) => Err(OpenSshError::ProxyUnsupported(proxy.display())),
        None => Ok(()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshInvocation {
    pub executable: PathBuf,
    pub args: Vec<String>,
}

impl SshInvocation {
    pub fn for_connection(
        openssh: &OpenSsh,
        connection: &Connection,
    ) -> Result<Self, OpenSshError> {
        Self::for_connection_with_verbosity(openssh, connection, false)
    }

    /// Builds a one-shot diagnostic invocation when `verbose` is requested.
    /// Normal sessions deliberately keep OpenSSH output quiet.
    pub fn for_connection_with_verbosity(
        openssh: &OpenSsh,
        connection: &Connection,
        verbose: bool,
    ) -> Result<Self, OpenSshError> {
        validate_connection(connection)?;
        refuse_proxy(connection)?;
        let mut args = Vec::new();
        if verbose {
            args.push("-vvv".to_owned());
        }
        match &connection.target {
            ConnectionTarget::Alias { alias } => args.push(alias.clone()),
            ConnectionTarget::Endpoint {
                hostname,
                username,
                port,
            } => {
                args.extend(["-p".into(), port.to_string()]);
                if let Some(username) = username {
                    args.extend(["-l".into(), username.clone()]);
                }
                args.push(hostname.clone());
            }
            ConnectionTarget::Serial(_) => return Err(ValidationError::NotSsh.into()),
        }
        if let Some(jump) = &connection.proxy_jump {
            validate_alias(jump)?;
            args.splice(0..0, ["-J".into(), jump.clone()]);
        }
        for forward in &connection.local_forwards {
            args.splice(0..0, ["-L".into(), validate_forward(forward)?]);
        }
        for forward in &connection.remote_forwards {
            args.splice(0..0, ["-R".into(), validate_forward(forward)?]);
        }
        for forward in &connection.dynamic_forwards {
            args.splice(0..0, ["-D".into(), validate_forward(forward)?]);
        }
        if let Some(command) = &connection.remote_command {
            if command.chars().any(char::is_control) {
                return Err(OpenSshError::Validation(ValidationError::Unsafe {
                    field: "remote command",
                }));
            }
            args.push(command.clone());
        }
        Ok(Self {
            executable: openssh.ssh_path()?,
            args,
        })
    }

    /// The options that make an interactive `ssh` the master of a control
    /// socket at `control` (`-M -S`), so the monitor can run commands on its
    /// connection. No `ControlPersist`: the connection ends with the session.
    pub fn master_args(control: &Path) -> Vec<String> {
        vec!["-M".into(), "-S".into(), control.display().to_string()]
    }

    /// Builds an invocation that runs `command` over the connection whose
    /// master listens on `control`, never prompting (`BatchMode`). The
    /// command is core-owned (the monitor script); it is sent as one argument.
    pub fn for_control_command(
        openssh: &OpenSsh,
        connection: &Connection,
        control: &Path,
        command: &str,
    ) -> Result<Self, OpenSshError> {
        validate_connection(connection)?;
        if command.is_empty() || command.chars().any(|c| c == '\0') {
            return Err(OpenSshError::Validation(ValidationError::Unsafe {
                field: "remote command",
            }));
        }
        let mut args: Vec<String> = vec![
            "-S".into(),
            control.display().to_string(),
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "ConnectTimeout=5".into(),
            "-T".into(),
        ];
        match &connection.target {
            ConnectionTarget::Alias { alias } => args.push(alias.clone()),
            ConnectionTarget::Endpoint {
                hostname,
                username,
                port,
            } => {
                args.extend(["-p".into(), port.to_string()]);
                if let Some(username) = username {
                    args.extend(["-l".into(), username.clone()]);
                }
                args.push(hostname.clone());
            }
            ConnectionTarget::Serial(_) => return Err(ValidationError::NotSsh.into()),
        }
        args.push(command.to_owned());
        Ok(Self {
            executable: openssh.ssh_path()?,
            args,
        })
    }

    /// Builds an invocation for a core-owned, fixed remote command. This path
    /// deliberately excludes a connection's interactive remote command.
    pub fn for_fixed_remote_command(
        openssh: &OpenSsh,
        connection: &Connection,
        command: &'static str,
    ) -> Result<Self, OpenSshError> {
        validate_connection(connection)?;
        refuse_proxy(connection)?;
        let mut args = Vec::new();
        match &connection.target {
            ConnectionTarget::Alias { alias } => args.push(alias.clone()),
            ConnectionTarget::Endpoint {
                hostname,
                username,
                port,
            } => {
                args.extend(["-p".into(), port.to_string()]);
                if let Some(username) = username {
                    args.extend(["-l".into(), username.clone()]);
                }
                args.push(hostname.clone());
            }
            ConnectionTarget::Serial(_) => return Err(ValidationError::NotSsh.into()),
        }
        if let Some(jump) = &connection.proxy_jump {
            validate_alias(jump)?;
            args.splice(0..0, ["-J".to_owned(), jump.clone()]);
        }
        args.push(command.to_owned());
        Ok(Self {
            executable: openssh.ssh_path()?,
            args,
        })
    }
}

fn validate_forward(value: &str) -> Result<String, OpenSshError> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        return Err(OpenSshError::Validation(ValidationError::Unsafe {
            field: "forward",
        }));
    }
    Ok(value.to_string())
}

#[derive(Debug, Clone, Default)]
pub struct OpenSsh;
impl OpenSsh {
    pub fn ssh_path(&self) -> Result<PathBuf, OpenSshError> {
        find_on_path("ssh").ok_or(OpenSshError::NotFound("ssh"))
    }
    pub fn scp_path(&self) -> Result<PathBuf, OpenSshError> {
        find_on_path("scp").ok_or(OpenSshError::NotFound("scp"))
    }
    pub fn sftp_path(&self) -> Result<PathBuf, OpenSshError> {
        find_on_path("sftp").ok_or(OpenSshError::NotFound("sftp"))
    }
    pub fn output(&self, invocation: &SshInvocation) -> Result<Output, OpenSshError> {
        Ok(Command::new(&invocation.executable)
            .args(&invocation.args)
            .output()?)
    }
    pub fn diagnostics(&self, target: Option<&str>) -> AgentDiagnostics {
        let ssh_path = self.ssh_path().ok();
        let scp_path = self.scp_path().ok();
        let ssh_version = ssh_path.as_ref().and_then(version);
        let scp_version = scp_path.as_ref().and_then(version);
        let agent_socket_configured = env::var_os("SSH_AUTH_SOCK").is_some();
        let agent_keys = hide_window(&mut Command::new("ssh-add"))
            .arg("-l")
            .output()
            .ok()
            .map(|output| AgentKeys::from_output(&output));
        let resolved_host = target
            .filter(|host| validate_host(host).is_ok() || validate_alias(host).is_ok())
            .and_then(|host| {
                ssh_path
                    .as_ref()
                    .and_then(|path| {
                        hide_window(&mut Command::new(path))
                            .args(["-G", host])
                            .output()
                            .ok()
                    })
                    .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
            });
        AgentDiagnostics {
            ssh_path,
            scp_path,
            ssh_version,
            scp_version,
            agent_socket_configured,
            agent_keys,
            resolved_host,
        }
    }
}

/// Launches a system terminal host, which then runs the system OpenSSH binary.
/// The app never renders or intermediates terminal input/output in this mode.
#[derive(Debug, Clone, Default)]
pub struct ExternalTerminal;

impl ExternalTerminal {
    // Android and iOS have no external terminal to launch: both arguments go unused there.
    #[allow(unused_variables)]
    pub fn launch(name: &str, invocation: &SshInvocation) -> Result<(), OpenSshError> {
        #[cfg(windows)]
        {
            if let Some(windows_terminal) = find_on_path("wt") {
                Command::new(windows_terminal)
                    .args(["new-tab", "--title", &format!("SSH: {name}"), "--"])
                    .arg(&invocation.executable)
                    .args(&invocation.args)
                    .spawn()?;
                return Ok(());
            }
            return launch_powershell(invocation);
        }
        #[cfg(target_os = "linux")]
        {
            // x-terminal-emulator has no portable title flag.
            let _ = name;
            let terminal = find_on_path("x-terminal-emulator")
                .ok_or(OpenSshError::NotFound("x-terminal-emulator"))?;
            Command::new(terminal)
                .arg("--")
                .arg(&invocation.executable)
                .args(&invocation.args)
                .spawn()?;
            return Ok(());
        }
        #[cfg(target_os = "macos")]
        {
            let _ = (name, invocation);
            return Err(OpenSshError::Failed(
                "macOS external terminal launching requires a configured terminal adapter"
                    .to_owned(),
            ));
        }
        #[allow(unreachable_code)]
        Err(OpenSshError::Failed("unsupported platform".to_owned()))
    }
}

#[cfg(windows)]
fn launch_powershell(invocation: &SshInvocation) -> Result<(), OpenSshError> {
    let script_path = std::env::temp_dir().join(format!("easyssh-{}.ps1", uuid::Uuid::new_v4()));
    // The script receives executable and arguments separately; no user value is embedded in it.
    fs::write(&script_path, "param([string]$SshExecutable, [Parameter(ValueFromRemainingArguments=$true)][string[]]$SshArguments)\n& $SshExecutable @SshArguments\n")?;
    Command::new("powershell.exe")
        .args(["-NoExit", "-File"])
        .arg(&script_path)
        .arg(&invocation.executable)
        .args(&invocation.args)
        .spawn()?;
    Ok(())
}

fn version(path: &PathBuf) -> Option<String> {
    let output = hide_window(&mut Command::new(path))
        .arg("-V")
        .output()
        .ok()?;
    Some(
        String::from_utf8_lossy(if output.stderr.is_empty() {
            &output.stdout
        } else {
            &output.stderr
        })
        .trim()
        .to_string(),
    )
}
fn find_on_path(name: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        // 1Password's Windows SSH agent listens on the Microsoft OpenSSH named
        // pipe. Prefer the system client over a Git/MSYS client earlier on PATH.
        if matches!(name, "ssh" | "scp") {
            if let Some(path) = env::var_os("WINDIR")
                .map(PathBuf::from)
                .map(|directory| {
                    directory
                        .join("System32")
                        .join("OpenSSH")
                        .join(format!("{name}.exe"))
                })
                .filter(|path| path.is_file())
            {
                return Some(path);
            }
        }
    }
    let paths = env::var_os("PATH")?;
    let candidates = if cfg!(windows) {
        vec![format!("{name}.exe"), name.into()]
    } else {
        vec![name.into()]
    };
    env::split_paths(&paths)
        .flat_map(|directory| {
            candidates
                .iter()
                .map(move |candidate| directory.join(candidate))
        })
        .find(|path| path.is_file())
}

#[derive(Debug, Clone)]
pub struct AgentDiagnostics {
    pub ssh_path: Option<PathBuf>,
    pub scp_path: Option<PathBuf>,
    pub ssh_version: Option<String>,
    pub scp_version: Option<String>,
    pub agent_socket_configured: bool,
    pub agent_keys: Option<AgentKeys>,
    pub resolved_host: Option<String>,
}
#[derive(Debug, Clone)]
pub struct AgentKeys {
    pub available: bool,
    pub fingerprints: Vec<String>,
    pub raw_error: Option<String>,
}
impl AgentKeys {
    fn from_output(output: &Output) -> Self {
        let text = String::from_utf8_lossy(if output.status.success() {
            &output.stdout
        } else {
            &output.stderr
        });
        Self {
            available: output.status.success(),
            fingerprints: if output.status.success() {
                text.lines().map(str::to_owned).collect()
            } else {
                Vec::new()
            },
            raw_error: (!output.status.success()).then(|| text.trim().to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn alias_invocation_contains_no_shell() {
        let c = Connection::alias("Prod", "production");
        let invocation = SshInvocation::for_connection(&OpenSsh, &c);
        assert!(invocation.is_err() || invocation.unwrap().args == vec!["production"]);
    }

    fn endpoint() -> Connection {
        let mut c = Connection::alias("Prod", "production");
        c.target = ConnectionTarget::Endpoint {
            hostname: "example.com".into(),
            username: Some("deploy".into()),
            port: 2222,
        };
        c
    }

    fn invocation(connection: &Connection, command: &str) -> Option<SshInvocation> {
        // `ssh` may be missing on the machine running the tests.
        SshInvocation::for_control_command(&OpenSsh, connection, Path::new("/run/cm-ab"), command)
            .ok()
    }

    #[test]
    fn control_command_reuses_the_master_and_never_prompts() {
        let Some(i) = invocation(&endpoint(), "sh -c 'echo hi'") else {
            return;
        };
        assert_eq!(
            i.args,
            [
                "-S",
                "/run/cm-ab",
                "-o",
                "ControlMaster=no",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=5",
                "-T",
                "-p",
                "2222",
                "-l",
                "deploy",
                "example.com",
                "sh -c 'echo hi'"
            ]
        );
    }

    #[test]
    fn control_command_for_an_alias_and_a_rejected_command() {
        let alias = Connection::alias("Prod", "production");
        if let Some(i) = invocation(&alias, "uptime") {
            assert_eq!(&i.args[i.args.len() - 2..], ["production", "uptime"]);
        }
        let rejected =
            SshInvocation::for_control_command(&OpenSsh, &alias, Path::new("/run/cm-ab"), "a\0b");
        assert!(rejected.is_err());
    }

    #[test]
    fn master_args_name_the_socket() {
        assert_eq!(
            SshInvocation::master_args(Path::new("/run/cm-ab")),
            ["-M", "-S", "/run/cm-ab"]
        );
    }

    #[test]
    fn a_host_with_a_proxy_is_never_started_without_it() {
        use crate::domain::{ProxyKind, ProxySettings};
        let mut c = Connection::alias("Prod", "production");
        c.proxy = Some(ProxySettings {
            kind: ProxyKind::Socks5,
            host: "127.0.0.1".into(),
            port: 7890,
            username: None,
        });
        // Whether or not `ssh` is installed, the answer is the proxy error,
        // never an invocation that would connect directly.
        let all = [
            SshInvocation::for_connection(&OpenSsh, &c),
            SshInvocation::for_connection_with_verbosity(&OpenSsh, &c, true),
            SshInvocation::for_fixed_remote_command(&OpenSsh, &c, "true"),
        ];
        for result in all {
            match result {
                Err(OpenSshError::ProxyUnsupported(proxy)) => {
                    assert_eq!(proxy, "socks5://127.0.0.1:7890");
                }
                other => panic!("expected the proxy error, got {other:?}"),
            }
        }
    }

    #[test]
    fn verbose_invocation_only_adds_debug_flag_on_request() {
        let c = Connection::alias("Prod", "production");
        let invocation = SshInvocation::for_connection_with_verbosity(&OpenSsh, &c, true);
        assert!(invocation.is_err() || invocation.unwrap().args == vec!["-vvv", "production"]);
    }
}
