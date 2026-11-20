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

/// Whether a bind address or target host names a Unix domain socket (an
/// absolute path) instead of a host: `-L /tmp/db.sock:/run/db.sock`.
pub fn is_socket_path(value: &str) -> bool {
    value.trim().starts_with('/')
}

fn socket_path(value: &str) -> Result<String, OpenSshError> {
    let value = value.trim();
    // `:` separates the parts of the spec, and OpenSSH has no escape for it.
    if value.contains(':') || value.chars().any(char::is_control) {
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
    pub fn forward_spec(&self) -> Result<String, OpenSshError> {
        let mut spec = String::new();
        let bind = self.bind_address.as_deref().filter(|bind| !bind.is_empty());
        match bind {
            // A socket path replaces the address and the port.
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
                spec.push(':');
                spec.push_str(&socket_path(host)?);
                return Ok(spec);
            }
            let target_port = port(self.target_port, "target port")?;
            spec.push_str(&format!(":{}:{target_port}", address(host)?));
        }
        Ok(spec)
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
    fn unix_sockets_replace_the_port() {
        let mut local = tunnel(TunnelKind::Local);
        local.bind_address = Some("/tmp/db.sock".into());
        local.bind_port = 0;
        local.target_host = Some("/run/postgres/.s.PGSQL.5432".into());
        local.target_port = None;
        assert_eq!(
            local.forward_spec().unwrap(),
            "/tmp/db.sock:/run/postgres/.s.PGSQL.5432"
        );
        // A socket on one side only.
        local.target_host = Some("db.internal".into());
        local.target_port = Some(5432);
        assert_eq!(
            local.forward_spec().unwrap(),
            "/tmp/db.sock:db.internal:5432"
        );
        let mut to_socket = tunnel(TunnelKind::Remote);
        to_socket.target_host = Some("/tmp/app.sock".into());
        to_socket.target_port = None;
        assert_eq!(to_socket.forward_spec().unwrap(), "15432:/tmp/app.sock");
        // Not for SOCKS, and no ':' in a path.
        let mut socks = tunnel(TunnelKind::Dynamic);
        socks.bind_address = Some("/tmp/socks".into());
        assert!(socks.forward_spec().is_err());
        to_socket.target_host = Some("/tmp/a:b".into());
        assert!(to_socket.forward_spec().is_err());
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
