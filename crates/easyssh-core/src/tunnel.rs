//! Port-forwarding tunnels, each run as its own `ssh -N` process.

use crate::domain::{Connection, ConnectionTarget};
use crate::openssh::{OpenSsh, OpenSshError};
use crate::process::hide_window;
use crate::security::{validate_alias, validate_connection, validate_host, ValidationError};
use serde::{Deserialize, Serialize};
use std::process::{Child, Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelKind {
    /// `-L`: a local port reaches a host as seen from the server.
    Local,
    /// `-R`: a server port reaches a host as seen from this computer.
    Remote,
    /// `-D`: a local SOCKS proxy through the server.
    Dynamic,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Tunnel {
    pub id: String,
    pub name: String,
    /// The host this tunnel connects through, as the host list identifies it.
    pub host_id: String,
    pub kind: TunnelKind,
    pub bind_address: Option<String>,
    pub bind_port: u16,
    /// Required for local and remote forwards; unused for dynamic ones.
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
}

/// A host or address without the brackets an IPv6 address is often written
/// in (`[::1]` is `::1`); saved tunnels keep it bare.
pub fn bare_host(value: &str) -> &str {
    let value = value.trim();
    value
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(value)
}

/// Whether an address field names a Unix domain socket rather than a host:
/// an absolute path. Saved tunnels keep the path in the same fields as a
/// host (`bind_address`, `target_host`), so no schema change is needed; a
/// socket end has no port (the bind port is saved as 0, the target port
/// as none).
pub fn is_socket_path(value: &str) -> bool {
    value.trim().starts_with('/')
}

/// A socket path as `-L`/`-R` take it: no control characters, and no colon
/// (the spec's separator; OpenSSH cannot express one either).
pub fn socket_path(value: &str) -> Result<String, OpenSshError> {
    let value = value.trim();
    if value.len() < 2 || value.contains(':') || value.chars().any(char::is_control) {
        return Err(ValidationError::Unsafe {
            field: "socket path",
        }
        .into());
    }
    Ok(value.to_owned())
}

fn address(value: &str) -> Result<String, OpenSshError> {
    let value = bare_host(value);
    validate_host(value)?;
    Ok(if value.contains(':') {
        format!("[{value}]")
    } else {
        value.to_owned()
    })
}

fn port(value: Option<u16>, field: &'static str) -> Result<u16, OpenSshError> {
    match value {
        Some(port) if port > 0 => Ok(port),
        Some(_) => Err(ValidationError::Port.into()),
        None => Err(ValidationError::Required { field }.into()),
    }
}

impl Tunnel {
    /// The argument for `-L`, `-R` or `-D`, after validating every part.
    ///
    /// Either end of a local or remote forward may be a Unix socket path
    /// (`-L 8080:/run/docker.sock`, `-L /tmp/db.sock:db:5432`,
    /// `-R /run/app.sock:localhost:3000`).
    pub fn forward_spec(&self) -> Result<String, OpenSshError> {
        let mut spec = String::new();
        let bind = self.bind_address.as_deref().filter(|bind| !bind.is_empty());
        match bind {
            Some(path) if is_socket_path(path) => {
                if self.kind == TunnelKind::Dynamic {
                    return Err(ValidationError::Unsafe {
                        field: "socket path",
                    }
                    .into());
                }
                spec.push_str(&socket_path(path)?);
            }
            _ => {
                if let Some(bind) = bind {
                    spec.push_str(&address(bind)?);
                    spec.push(':');
                }
                // Port 0: any free port (the server picks one for a remote forward).
                spec.push_str(&self.bind_port.to_string());
            }
        }
        if self.kind != TunnelKind::Dynamic {
            let host = self
                .target_host
                .as_deref()
                .filter(|host| !host.is_empty())
                .ok_or(ValidationError::Required {
                    field: "target host",
                })?;
            if is_socket_path(host) {
                spec.push_str(&format!(":{}", socket_path(host)?));
            } else {
                let target_port = port(self.target_port, "target port")?;
                spec.push_str(&format!(":{}:{target_port}", address(host)?));
            }
        }
        Ok(spec)
    }
}

/// The parts of a `-L`/`-R`/`-D` argument or an ssh config `LocalForward`,
/// `RemoteForward` or `DynamicForward` value, in the fields a [`Tunnel`]
/// saves. A Unix socket end is a path in `bind_address` (bind port 0) or
/// `target_host` (no target port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForwardParts {
    pub bind_address: Option<String>,
    pub bind_port: u16,
    pub target_host: Option<String>,
    pub target_port: Option<u16>,
}

/// Splits on `:` outside `[...]`.
fn colon_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => {
                parts.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn parse_port(text: &str) -> Option<u16> {
    text.parse().ok()
}

/// `[bind:]port` or a socket path.
fn parse_listen(parts: &[&str]) -> Option<(Option<String>, u16)> {
    match parts {
        [path] if is_socket_path(path) => Some((Some((*path).to_owned()), 0)),
        [port] => Some((None, parse_port(port)?)),
        [bind, port] => Some((
            Some(bare_host(bind).to_owned()).filter(|bind| !bind.is_empty()),
            parse_port(port)?,
        )),
        _ => None,
    }
}

/// `host:port` or a socket path.
fn parse_target(parts: &[&str]) -> Option<(String, Option<u16>)> {
    match parts {
        [path] if is_socket_path(path) => Some(((*path).to_owned(), None)),
        [host, port] if !host.is_empty() => {
            Some((bare_host(host).to_owned(), Some(parse_port(port)?)))
        }
        _ => None,
    }
}

impl ForwardParts {
    /// Parses a forward written as OpenSSH takes it on the command line
    /// (`8080:db:5432`, `8080:/run/docker.sock`, `/tmp/l.sock:db:5432`,
    /// `[::1]:8080:db:5432`) or as the ssh config prints it (`ssh -G`:
    /// `127.0.0.1:8080 db:5432`, `8080 /run/docker.sock`). For a dynamic
    /// forward, only the listen side.
    pub fn parse(kind: TunnelKind, text: &str) -> Option<ForwardParts> {
        let text = text.trim();
        let words: Vec<&str> = text.split_whitespace().collect();
        let (listen, target): (Vec<&str>, Option<Vec<&str>>) = if words.len() == 2 {
            (colon_parts(words[0]), Some(colon_parts(words[1])))
        } else if words.len() == 1 {
            let all = colon_parts(words[0]);
            if kind == TunnelKind::Dynamic {
                (all, None)
            } else if all.first().is_some_and(|p| is_socket_path(p)) {
                // `/path` then the target: a path, or host and port.
                let rest = &all[1..];
                let target_ok = match rest {
                    [path] => is_socket_path(path),
                    [_, _] => true,
                    _ => false,
                };
                if !target_ok {
                    return None;
                }
                (vec![all[0]], Some(rest.to_vec()))
            } else if all.last().is_some_and(|p| is_socket_path(p)) {
                (
                    all[..all.len() - 1].to_vec(),
                    Some(vec![all[all.len() - 1]]),
                )
            } else if all.len() >= 3 {
                let at = all.len() - 2;
                (all[..at].to_vec(), Some(all[at..].to_vec()))
            } else {
                return None;
            }
        } else {
            return None;
        };
        let (bind_address, bind_port) = parse_listen(&listen)?;
        if kind == TunnelKind::Dynamic {
            return (target.is_none() && !bind_address.as_deref().is_some_and(is_socket_path))
                .then_some(ForwardParts {
                    bind_address,
                    bind_port,
                    target_host: None,
                    target_port: None,
                });
        }
        let (target_host, target_port) = parse_target(&target?)?;
        Some(ForwardParts {
            bind_address,
            bind_port,
            target_host: Some(target_host),
            target_port,
        })
    }
}

/// Keepalives of a tunnel's `ssh -N`: the built-in engine's defaults
/// (`easyssh_ssh::DEFAULT_KEEPALIVE` and `DEFAULT_KEEPALIVE_MAX`), so a dead
/// connection is noticed after the same three minutes on either engine.
pub const KEEPALIVE_INTERVAL_SECS: u64 = 60;
pub const KEEPALIVE_COUNT_MAX: u32 = 3;

/// The `ssh` arguments that run `tunnel` through `connection`.
pub fn tunnel_args(connection: &Connection, tunnel: &Tunnel) -> Result<Vec<String>, OpenSshError> {
    validate_connection(connection)?;
    let flag = match tunnel.kind {
        TunnelKind::Local => "-L",
        TunnelKind::Remote => "-R",
        TunnelKind::Dynamic => "-D",
    };
    let mut args: Vec<String> = vec![
        "-N".to_owned(),
        "-o".to_owned(),
        "ExitOnForwardFailure=yes".to_owned(),
        "-o".to_owned(),
        format!("ServerAliveInterval={KEEPALIVE_INTERVAL_SECS}"),
        "-o".to_owned(),
        format!("ServerAliveCountMax={KEEPALIVE_COUNT_MAX}"),
    ];
    args.extend([flag.to_owned(), tunnel.forward_spec()?]);
    if let Some(jump) = &connection.proxy_jump {
        validate_alias(jump)?;
        args.extend(["-J".to_owned(), jump.clone()]);
    }
    match &connection.target {
        ConnectionTarget::Alias { alias } => args.push(alias.clone()),
        ConnectionTarget::Endpoint {
            hostname,
            username,
            port,
        } => {
            args.extend(["-p".to_owned(), port.to_string()]);
            if let Some(username) = username {
                args.extend(["-l".to_owned(), username.clone()]);
            }
            args.push(hostname.clone());
        }
        ConnectionTarget::Serial(_) => return Err(ValidationError::NotSsh.into()),
    }
    Ok(args)
}

/// Starts the tunnel in the background. Standard error is piped so the
/// caller can show why a tunnel failed.
pub fn spawn_tunnel(
    openssh: &OpenSsh,
    connection: &Connection,
    tunnel: &Tunnel,
    env: &[(String, String)],
) -> Result<Child, OpenSshError> {
    let args = tunnel_args(connection, tunnel)?;
    Ok(hide_window(&mut Command::new(openssh.ssh_path()?))
        .args(args)
        .envs(env.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tunnel(kind: TunnelKind) -> Tunnel {
        Tunnel {
            id: "t".into(),
            name: "db".into(),
            host_id: "h".into(),
            kind,
            bind_address: None,
            bind_port: 15432,
            target_host: Some("db.internal".into()),
            target_port: Some(5432),
        }
    }

    #[test]
    fn builds_forward_specs() {
        assert_eq!(
            tunnel(TunnelKind::Local).forward_spec().unwrap(),
            "15432:db.internal:5432"
        );
        let mut socks = tunnel(TunnelKind::Dynamic);
        socks.bind_address = Some("::1".into());
        assert_eq!(socks.forward_spec().unwrap(), "[::1]:15432");
    }

    #[test]
    fn unix_sockets_on_either_end() {
        let mut to_socket = tunnel(TunnelKind::Local);
        to_socket.bind_port = 8080;
        to_socket.target_host = Some("/run/docker.sock".into());
        to_socket.target_port = None;
        assert_eq!(to_socket.forward_spec().unwrap(), "8080:/run/docker.sock");
        to_socket.bind_address = Some("127.0.0.1".into());
        assert_eq!(
            to_socket.forward_spec().unwrap(),
            "127.0.0.1:8080:/run/docker.sock"
        );

        let mut from_socket = tunnel(TunnelKind::Local);
        from_socket.bind_address = Some("/tmp/db.sock".into());
        from_socket.bind_port = 0;
        assert_eq!(
            from_socket.forward_spec().unwrap(),
            "/tmp/db.sock:db.internal:5432"
        );
        from_socket.target_host = Some("/run/db.sock".into());
        from_socket.target_port = None;
        assert_eq!(
            from_socket.forward_spec().unwrap(),
            "/tmp/db.sock:/run/db.sock"
        );

        let mut remote = tunnel(TunnelKind::Remote);
        remote.bind_address = Some("/run/app.sock".into());
        remote.bind_port = 0;
        remote.target_host = Some("localhost".into());
        remote.target_port = Some(3000);
        assert_eq!(
            remote.forward_spec().unwrap(),
            "/run/app.sock:localhost:3000"
        );
    }

    type Parts = (Option<String>, u16, Option<String>, Option<u16>);

    fn parts(kind: TunnelKind, text: &str) -> Option<Parts> {
        ForwardParts::parse(kind, text)
            .map(|p| (p.bind_address, p.bind_port, p.target_host, p.target_port))
    }

    #[test]
    fn forward_arguments_and_config_values_parse() {
        use TunnelKind::{Dynamic, Local, Remote};
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(
            parts(Local, "8080:db:5432"),
            Some((None, 8080, some("db"), Some(5432)))
        );
        assert_eq!(
            parts(Local, "[::1]:8080:[fd00::5]:5432"),
            Some((some("::1"), 8080, some("fd00::5"), Some(5432)))
        );
        assert_eq!(
            parts(Local, "8080:/run/docker.sock"),
            Some((None, 8080, some("/run/docker.sock"), None))
        );
        assert_eq!(
            parts(Local, "127.0.0.1:8080:/run/docker.sock"),
            Some((some("127.0.0.1"), 8080, some("/run/docker.sock"), None))
        );
        assert_eq!(
            parts(Local, "/tmp/l.sock:db:5432"),
            Some((some("/tmp/l.sock"), 0, some("db"), Some(5432)))
        );
        assert_eq!(
            parts(Local, "/tmp/l.sock:/run/d.sock"),
            Some((some("/tmp/l.sock"), 0, some("/run/d.sock"), None))
        );
        assert_eq!(
            parts(Remote, "/run/app.sock:localhost:3000"),
            Some((some("/run/app.sock"), 0, some("localhost"), Some(3000)))
        );
        // `ssh -G` prints listen and target separated by a space.
        assert_eq!(
            parts(Local, "127.0.0.1:8080 db:5432"),
            Some((some("127.0.0.1"), 8080, some("db"), Some(5432)))
        );
        assert_eq!(
            parts(Local, "8080 /run/docker.sock"),
            Some((None, 8080, some("/run/docker.sock"), None))
        );
        assert_eq!(
            parts(Remote, "/run/app.sock /tmp/local.sock"),
            Some((some("/run/app.sock"), 0, some("/tmp/local.sock"), None))
        );
        assert_eq!(parts(Dynamic, "1080"), Some((None, 1080, None, None)));
        assert_eq!(
            parts(Dynamic, "[::1]:1080"),
            Some((some("::1"), 1080, None, None))
        );
    }

    #[test]
    fn malformed_forwards_do_not_parse() {
        use TunnelKind::{Dynamic, Local};
        for bad in [
            "",
            "8080",
            "8080:db",
            "x:db:5432",
            "8080:db:99999",
            "/a:/b:/c",
        ] {
            assert_eq!(parts(Local, bad), None, "{bad}");
        }
        assert_eq!(parts(Dynamic, "/tmp/socks"), None);
        assert_eq!(parts(Dynamic, "1080 db:1"), None);
    }

    #[test]
    fn socket_paths_are_validated() {
        assert!(is_socket_path("/run/x.sock"));
        assert!(!is_socket_path("localhost"));
        let mut colon = tunnel(TunnelKind::Local);
        colon.target_host = Some("/run/a:b".into());
        assert!(colon.forward_spec().is_err());
        let mut newline = tunnel(TunnelKind::Local);
        newline.bind_address = Some("/tmp/a\nb".into());
        assert!(newline.forward_spec().is_err());
        let mut socks = tunnel(TunnelKind::Dynamic);
        socks.bind_address = Some("/tmp/socks".into());
        assert!(socks.forward_spec().is_err());
    }

    #[test]
    fn rejects_unsafe_or_missing_parts() {
        let mut bad = tunnel(TunnelKind::Local);
        bad.target_host = Some("-oProxyCommand=evil".into());
        assert!(bad.forward_spec().is_err());
        let mut missing = tunnel(TunnelKind::Remote);
        missing.target_port = None;
        assert!(missing.forward_spec().is_err());
        let mut zero = tunnel(TunnelKind::Local);
        zero.target_port = Some(0);
        assert!(zero.forward_spec().is_err());
    }

    #[test]
    fn port_zero_listens_on_any_free_port() {
        let mut any = tunnel(TunnelKind::Dynamic);
        any.bind_port = 0;
        assert_eq!(any.forward_spec().unwrap(), "0");
        let mut remote = tunnel(TunnelKind::Remote);
        remote.bind_port = 0;
        assert_eq!(remote.forward_spec().unwrap(), "0:db.internal:5432");
    }

    #[test]
    fn ipv6_addresses_are_bracketed_once() {
        assert_eq!(bare_host("[::1]"), "::1");
        assert_eq!(bare_host(" ::1 "), "::1");
        assert_eq!(bare_host("db.internal"), "db.internal");
        assert_eq!(bare_host("[db"), "[db");
        let mut both = tunnel(TunnelKind::Local);
        both.bind_address = Some("[::1]".into());
        both.target_host = Some("[fd00::5]".into());
        assert_eq!(both.forward_spec().unwrap(), "[::1]:15432:[fd00::5]:5432");
        both.bind_address = Some("::1".into());
        both.target_host = Some("fd00::5".into());
        assert_eq!(both.forward_spec().unwrap(), "[::1]:15432:[fd00::5]:5432");
    }

    #[test]
    fn arguments_end_with_the_destination() {
        let connection = Connection::alias("prod", "prod");
        let args = tunnel_args(&connection, &tunnel(TunnelKind::Local)).unwrap();
        assert_eq!(args[0], "-N");
        assert!(args.contains(&"ExitOnForwardFailure=yes".to_owned()));
        // The same keepalives as the built-in engine.
        assert!(args.contains(&"ServerAliveInterval=60".to_owned()));
        assert!(args.contains(&"ServerAliveCountMax=3".to_owned()));
        assert_eq!(
            args[args.len() - 3..],
            ["-L", "15432:db.internal:5432", "prod"]
        );
    }
}
