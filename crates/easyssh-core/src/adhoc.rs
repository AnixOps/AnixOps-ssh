//! Ad-hoc targets: a connection that was typed (the quick-connect box) or
//! arrived in a link, and is not saved. Its session id is
//! `adhoc:[user@]host[:port]` (IPv6 in brackets); the id carries everything
//! the backend needs and nothing else, so no option can ride along.

use crate::domain::{Connection, ConnectionTarget};
use crate::security::{validate_connection, ValidationError};

/// What the id of an unsaved connection starts with.
pub const ADHOC_ID_PREFIX: &str = "adhoc:";

/// Whether `id` names an unsaved connection.
pub fn is_adhoc_id(id: &str) -> bool {
    id.starts_with(ADHOC_ID_PREFIX)
}

/// The connection an `adhoc:` id stands for, validated like a saved host.
/// `Ok(None)` when `id` is not an ad-hoc id.
pub fn adhoc_connection(id: &str) -> Result<Option<Connection>, String> {
    let Some(text) = id.strip_prefix(ADHOC_ID_PREFIX) else {
        return Ok(None);
    };
    let (username, rest) = match text.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, text),
    };
    let (hostname, port) = match rest.strip_prefix('[') {
        Some(bracketed) => {
            let (host, tail) = bracketed.split_once(']').ok_or("a closing ] is missing")?;
            let port = match tail.strip_prefix(':') {
                Some(port) => Some(port),
                None if tail.is_empty() => None,
                None => return Err("unexpected text after ]".to_owned()),
            };
            (host, port)
        }
        None => match rest.rsplit_once(':') {
            Some((host, port)) if !host.contains(':') => (host, Some(port)),
            _ => (rest, None),
        },
    };
    let port = match port {
        Some(port) => port
            .parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| ValidationError::Port.to_string())?,
        None => 22,
    };
    let mut connection = Connection::alias(text, "unused");
    connection.id = id.to_owned();
    connection.target = ConnectionTarget::Endpoint {
        hostname: hostname.to_owned(),
        username: username.map(str::to_owned),
        port,
    };
    validate_connection(&connection).map_err(|error| error.to_string())?;
    Ok(Some(connection))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(id: &str) -> (String, Option<String>, u16) {
        let connection = adhoc_connection(id).unwrap().unwrap();
        match connection.target {
            ConnectionTarget::Endpoint {
                hostname,
                username,
                port,
            } => (hostname, username, port),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_the_forms_the_ui_builds() {
        assert_eq!(
            endpoint("adhoc:example.com"),
            ("example.com".into(), None, 22)
        );
        assert_eq!(
            endpoint("adhoc:root@10.0.0.5:2222"),
            ("10.0.0.5".into(), Some("root".into()), 2222)
        );
        assert_eq!(
            endpoint("adhoc:me@[fe80::1]:22"),
            ("fe80::1".into(), Some("me".into()), 22)
        );
        assert_eq!(endpoint("adhoc:[::1]"), ("::1".into(), None, 22));
    }

    #[test]
    fn other_ids_are_not_adhoc() {
        assert!(adhoc_connection("ssh-config:pi").unwrap().is_none());
        assert!(adhoc_connection("3").unwrap().is_none());
    }

    #[test]
    fn refuses_unsafe_values() {
        for id in [
            "adhoc:-oProxyCommand=x",
            "adhoc:me@-host",
            "adhoc:-me@host",
            "adhoc:a/b@host",
            "adhoc:host:0",
            "adhoc:host:70000",
            "adhoc:host:x",
            "adhoc:a/b@host",
            "adhoc:host;ls",
            "adhoc:",
            "adhoc:[::1",
        ] {
            assert!(adhoc_connection(id).is_err(), "{id}");
        }
    }
}
