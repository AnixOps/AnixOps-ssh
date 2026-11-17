//! The host list as the mobile UI sees it: SSH endpoints in the store, no
//! `~/.ssh/config` aliases (there is no OpenSSH on a phone) and no serial
//! ports. The JSON has the shape of the desktop's `HostView`, so the React
//! host list and `saveHost` work unchanged.

use crate::Target;
use easyssh_core::security::validate_connection;
use easyssh_core::{AppConfig, Connection, ConnectionTarget};
use easyssh_store::Store;
use serde_json::{json, Value};

/// What the mobile host form submits.
#[derive(Debug, Clone)]
pub struct NewHost {
    pub name: String,
    pub hostname: String,
    pub username: Option<String>,
    pub port: u16,
}

fn display_target(hostname: &str, username: Option<&str>, port: u16) -> String {
    let user = username.map(|user| format!("{user}@")).unwrap_or_default();
    if port == 22 {
        format!("{user}{hostname}")
    } else {
        format!("{user}{hostname}:{port}")
    }
}

fn display_command(hostname: &str, username: Option<&str>, port: u16) -> String {
    let mut command = String::from("ssh ");
    if port != 22 {
        command.push_str(&format!("-p {port} "));
    }
    if let Some(user) = username {
        command.push_str(&format!("{user}@"));
    }
    command.push_str(hostname);
    command
}

fn host_json(connection: &Connection) -> Option<Value> {
    let ConnectionTarget::Endpoint {
        hostname,
        username,
        port,
    } = &connection.target
    else {
        return None;
    };
    Some(json!({
        "id": connection.id,
        "name": connection.name,
        "target": display_target(hostname, username.as_deref(), *port),
        "command": display_command(hostname, username.as_deref(), *port),
        "groupId": connection.group_id,
        "tags": connection.tags,
        "favorite": connection.favorite,
        "color": connection.color,
        "source": "saved",
        "fields": {
            "alias": null,
            "hostname": hostname,
            "username": username,
            "port": port,
            "proxyJump": connection.proxy_jump,
            "notes": connection.notes,
            "engine": null,
            "shellIntegration": false,
            "serial": null,
        },
    }))
}

/// The `HostsSnapshot` the UI loads.
pub fn snapshot(config: &AppConfig) -> Value {
    json!({
        "hosts": config.connections.iter().filter_map(host_json).collect::<Vec<_>>(),
        "groups": config.groups.iter().map(|group| json!({
            "id": group.id,
            "name": group.name,
            "color": group.color,
        })).collect::<Vec<_>>(),
        "warnings": [],
    })
}

/// Saves a new host and returns its id.
pub fn add_host(store: &Store, input: &NewHost) -> Result<String, String> {
    let hostname = input.hostname.trim();
    let name = input.name.trim();
    let mut connection = Connection::alias(if name.is_empty() { hostname } else { name }, "unused");
    connection.target = ConnectionTarget::Endpoint {
        hostname: hostname.to_owned(),
        username: input
            .username
            .as_deref()
            .map(str::trim)
            .filter(|user| !user.is_empty())
            .map(str::to_owned),
        port: input.port,
    };
    validate_connection(&connection).map_err(|error| error.to_string())?;
    store
        .save_host(&connection, None)
        .map_err(|error| error.to_string())?;
    Ok(connection.id)
}

/// Where to connect for `host_id`. A host without a user name cannot be
/// opened: a phone has no local user to default to.
pub fn target_for(config: &AppConfig, host_id: &str) -> Result<Target, String> {
    let connection = config
        .connections
        .iter()
        .find(|connection| connection.id == host_id)
        .ok_or_else(|| format!("unknown host: {host_id}"))?;
    let ConnectionTarget::Endpoint {
        hostname,
        username,
        port,
    } = &connection.target
    else {
        return Err("only hosts with an address can be opened on a phone".into());
    };
    let user = username.clone().ok_or_else(|| {
        format!(
            "{} has no user name; edit the host and add one",
            connection.name
        )
    })?;
    Ok(Target {
        label: connection.name.clone(),
        host: hostname.clone(),
        port: *port,
        user,
        identity: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use easyssh_store::open_app_store;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let opened = open_app_store(&dir.path().join("config.json")).unwrap();
        (dir, opened.store)
    }

    fn new_host(name: &str, hostname: &str, user: Option<&str>, port: u16) -> NewHost {
        NewHost {
            name: name.into(),
            hostname: hostname.into(),
            username: user.map(str::to_owned),
            port,
        }
    }

    #[test]
    fn a_saved_host_shows_up_in_the_snapshot_with_the_desktop_shape() {
        let (_dir, store) = store();
        let id = add_host(&store, &new_host("web", "10.0.0.5", Some("deploy"), 2222)).unwrap();
        let snapshot = snapshot(&store.load_app_config().unwrap());
        let host = &snapshot["hosts"][0];
        assert_eq!(host["id"], id.as_str());
        assert_eq!(host["name"], "web");
        assert_eq!(host["target"], "deploy@10.0.0.5:2222");
        assert_eq!(host["command"], "ssh -p 2222 deploy@10.0.0.5");
        assert_eq!(host["source"], "saved");
        assert_eq!(host["fields"]["port"], 2222);
        assert_eq!(host["fields"]["hostname"], "10.0.0.5");
        assert!(host["fields"]["serial"].is_null());
        assert_eq!(snapshot["warnings"], json!([]));
    }

    #[test]
    fn the_name_defaults_to_the_address_and_unsafe_addresses_are_refused() {
        let (_dir, store) = store();
        add_host(&store, &new_host(" ", "example.org", Some("me"), 22)).unwrap();
        let config = store.load_app_config().unwrap();
        assert_eq!(config.connections[0].name, "example.org");
        assert!(add_host(
            &store,
            &new_host("x", "-oProxyCommand=evil", Some("me"), 22)
        )
        .is_err());
        assert!(add_host(&store, &new_host("x", "", Some("me"), 22)).is_err());
    }

    #[test]
    fn opening_a_host_needs_a_user_name() {
        let (_dir, store) = store();
        let with = add_host(&store, &new_host("a", "a.example", Some("me"), 2200)).unwrap();
        let without = add_host(&store, &new_host("b", "b.example", None, 22)).unwrap();
        let config = store.load_app_config().unwrap();
        let target = target_for(&config, &with).unwrap();
        assert_eq!(
            (target.host.as_str(), target.port, target.user.as_str()),
            ("a.example", 2200, "me")
        );
        assert!(target_for(&config, &without)
            .unwrap_err()
            .contains("no user name"));
        assert!(target_for(&config, "missing").is_err());
    }
}
