//! The host list as the mobile UI sees it: SSH endpoints in the store, no
//! `~/.ssh/config` aliases (there is no OpenSSH on a phone) and no serial
//! ports. The JSON has the shape of the desktop's `HostView`, so the React
//! host list and `saveHost` work unchanged.
//!
//! Editing covers what a phone needs: name, address, user and port, the group,
//! tags, favourite and a jump host. A jump host is stored the way OpenSSH
//! writes it (`user@host:port`, several separated by commas), so a host
//! edited here still works after it syncs to a desktop.

use crate::Target;
use easyssh_core::security::{validate_connection, validate_host, validate_username};
use easyssh_core::{AppConfig, Connection, ConnectionTarget, Group, Tunnel, TunnelKind};
use easyssh_store::Store;
use serde_json::{json, Value};

/// What the mobile host form submits.
#[derive(Debug, Clone, Default)]
pub struct NewHost {
    /// Set to edit an existing host; everything the form does not show (notes,
    /// forwards, terminal preferences, ...) is kept.
    pub id: Option<String>,
    pub name: String,
    pub hostname: String,
    pub username: Option<String>,
    pub port: u16,
    /// The group's name; found ignoring case, or created.
    pub group: Option<String>,
    pub tags: Vec<String>,
    pub favorite: bool,
    /// The jump host chain, `[user@]host[:port]` separated by commas.
    pub jump: Option<String>,
}

fn display_target(hostname: &str, username: Option<&str>, port: u16) -> String {
    let user = username.map(|user| format!("{user}@")).unwrap_or_default();
    if port == 22 {
        format!("{user}{hostname}")
    } else {
        format!("{user}{hostname}:{port}")
    }
}

fn display_command(
    hostname: &str,
    username: Option<&str>,
    port: u16,
    jump: Option<&str>,
) -> String {
    let mut command = String::from("ssh ");
    if port != 22 {
        command.push_str(&format!("-p {port} "));
    }
    if let Some(jump) = jump.filter(|jump| !jump.is_empty()) {
        command.push_str(&format!("-J {jump} "));
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
        "command": display_command(
            hostname,
            username.as_deref(),
            *port,
            connection.proxy_jump.as_deref(),
        ),
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

/// Tags as the form submits them: trimmed, without empties and repeats
/// (comparing case-insensitively), in the order given.
pub fn clean_tags(tags: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for tag in tags {
        let tag = tag.trim().trim_start_matches('#').trim();
        if tag.is_empty() || tag.chars().any(char::is_control) || tag.chars().count() > 40 {
            continue;
        }
        let folded = tag.to_lowercase();
        if !seen.contains(&folded) {
            seen.push(folded);
            out.push(tag.to_owned());
        }
    }
    out
}

/// One hop of a jump chain as written by OpenSSH: `[user@]host[:port]`, with
/// IPv6 addresses in brackets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    pub user: Option<String>,
    pub host: String,
    pub port: u16,
}

/// Parses `user@host:port,other`. An unusable hop is an error naming it.
pub fn parse_jump(text: &str) -> Result<Vec<Hop>, String> {
    let mut hops = Vec::new();
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (user, rest) = match part.rsplit_once('@') {
            Some((user, rest)) => (Some(user.to_owned()), rest),
            None => (None, part),
        };
        let (host, port) = if let Some(bracketed) = rest.strip_prefix('[') {
            let (host, tail) = bracketed
                .split_once(']')
                .ok_or_else(|| format!("{part}: a closing ] is missing"))?;
            let port = match tail.strip_prefix(':') {
                Some(port) => Some(port),
                None if tail.is_empty() => None,
                None => return Err(format!("{part}: unexpected text after ]")),
            };
            (host.to_owned(), port)
        } else {
            match rest.rsplit_once(':') {
                Some((host, port)) if !host.contains(':') => (host.to_owned(), Some(port)),
                _ => (rest.to_owned(), None),
            }
        };
        let port = match port {
            Some(port) => port
                .parse::<u16>()
                .ok()
                .filter(|port| *port > 0)
                .ok_or_else(|| format!("{part}: the port must be 1 to 65535"))?,
            None => 22,
        };
        validate_host(&host).map_err(|error| format!("{part}: {error}"))?;
        if let Some(user) = &user {
            validate_username(user).map_err(|error| format!("{part}: {error}"))?;
        }
        hops.push(Hop { user, host, port });
    }
    Ok(hops)
}

/// The text to store for a jump host that is itself a saved host.
pub fn jump_text(hostname: &str, username: Option<&str>, port: u16) -> String {
    let host = if hostname.contains(':') {
        format!("[{hostname}]")
    } else {
        hostname.to_owned()
    };
    let user = username.map(|user| format!("{user}@")).unwrap_or_default();
    if port == 22 {
        format!("{user}{host}")
    } else {
        format!("{user}{host}:{port}")
    }
}

/// Saves a host: a new one, or the one named by `input.id` with only the
/// fields of the form replaced. Returns its id.
pub fn save_host(store: &Store, input: &NewHost) -> Result<String, String> {
    let hostname = input.hostname.trim();
    let name = input.name.trim();
    let mut connection = match &input.id {
        Some(id) => store
            .load_app_config()
            .map_err(|error| error.to_string())?
            .connections
            .into_iter()
            .find(|connection| &connection.id == id)
            .ok_or_else(|| format!("unknown host: {id}"))?,
        None => Connection::alias("", "unused"),
    };
    connection.name = if name.is_empty() { hostname } else { name }.to_owned();
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
    connection.group_id = group_for(store, input.group.as_deref())?;
    connection.tags = clean_tags(&input.tags);
    connection.favorite = input.favorite;
    connection.proxy_jump = match input.jump.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(jump) => {
            parse_jump(jump)?;
            Some(jump.to_owned())
        }
    };
    connection.updated_at = chrono::Utc::now();
    validate_connection(&connection).map_err(|error| error.to_string())?;
    store
        .save_host(&connection, None)
        .map_err(|error| error.to_string())?;
    Ok(connection.id)
}

/// Saves a new host and returns its id.
pub fn add_host(store: &Store, input: &NewHost) -> Result<String, String> {
    save_host(
        store,
        &NewHost {
            id: None,
            ..input.clone()
        },
    )
}

/// The id of the group called `name` (ignoring case), created when there is
/// none. An empty name is no group.
fn group_for(store: &Store, name: Option<&str>) -> Result<Option<String>, String> {
    let name = name.map(str::trim).filter(|name| !name.is_empty());
    let Some(name) = name else {
        return Ok(None);
    };
    if name.chars().any(char::is_control) || name.chars().count() > 80 {
        return Err("the group name is not valid".into());
    }
    let groups = store.list_groups().map_err(|error| error.to_string())?;
    if let Some(found) = groups
        .iter()
        .find(|record| record.item.name.eq_ignore_ascii_case(name))
    {
        return Ok(Some(found.item.id.clone()));
    }
    let group = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name: name.to_owned(),
        color: None,
        parent_id: None,
    };
    store
        .upsert_group(&group)
        .map_err(|error| error.to_string())?;
    Ok(Some(group.id))
}

fn endpoint(connection: &Connection) -> Option<(&str, Option<&str>, u16)> {
    match &connection.target {
        ConnectionTarget::Endpoint {
            hostname,
            username,
            port,
        } => Some((hostname, username.as_deref(), *port)),
        _ => None,
    }
}

/// Resolves one hop to a target: a saved host named (or identified) by the
/// hop's text, else the address the text spells out. A hop without a user
/// uses the user of the host being opened.
fn hop_target(config: &AppConfig, hop_text: &str, fallback_user: &str) -> Result<Target, String> {
    let saved = config.connections.iter().find(|connection| {
        endpoint(connection).is_some() && (connection.id == hop_text || connection.name == hop_text)
    });
    if let Some(connection) = saved {
        let (host, user, port) = endpoint(connection).expect("checked above");
        return Ok(Target {
            host_id: connection.id.clone(),
            label: connection.name.clone(),
            host: host.to_owned(),
            port,
            user: user.unwrap_or(fallback_user).to_owned(),
            identity: None,
            jump: Vec::new(),
            saved_password: None,
        });
    }
    let hop = parse_jump(hop_text)?
        .into_iter()
        .next()
        .ok_or_else(|| "the jump host is empty".to_owned())?;
    let user = hop.user.unwrap_or_else(|| fallback_user.to_owned());
    // A hop that is a saved host with the same address shares its identity
    // (and so its saved key, once the vault provides one).
    let host_id = config
        .connections
        .iter()
        .find(|connection| {
            endpoint(connection).is_some_and(|(host, _, port)| host == hop.host && port == hop.port)
        })
        .map(|connection| connection.id.clone())
        .unwrap_or_else(|| format!("jump:{user}@{}:{}", hop.host, hop.port));
    Ok(Target {
        host_id,
        label: hop.host.clone(),
        host: hop.host,
        port: hop.port,
        user,
        identity: None,
        jump: Vec::new(),
        saved_password: None,
    })
}

/// Where to connect for `host_id`, with its jump chain. A host without a user
/// name cannot be opened: a phone has no local user to default to.
///
/// `identity` and `saved_password` are `None` here: the vault's saved login is
/// added by [`crate::with_saved_login`], which is the only place secrets are
/// read.
pub fn target_for(config: &AppConfig, host_id: &str) -> Result<Target, String> {
    let connection = config
        .connections
        .iter()
        .find(|connection| connection.id == host_id)
        .ok_or_else(|| format!("unknown host: {host_id}"))?;
    let Some((hostname, username, port)) = endpoint(connection) else {
        return Err("only hosts with an address can be opened on a phone".into());
    };
    let user = username.map(str::to_owned).ok_or_else(|| {
        format!(
            "{} has no user name; edit the host and add one",
            connection.name
        )
    })?;
    let mut jump = Vec::new();
    if let Some(chain) = connection.proxy_jump.as_deref() {
        for part in chain.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let hop = hop_target(config, part, &user)?;
            if hop.host_id == connection.id {
                return Err(format!("{} cannot be its own jump host", connection.name));
            }
            jump.push(hop);
        }
    }
    Ok(Target {
        host_id: connection.id.clone(),
        label: connection.name.clone(),
        host: hostname.to_owned(),
        port,
        user,
        identity: None,
        jump,
        saved_password: None,
    })
}

/// What the tunnel form submits: a local forward through a saved host.
#[derive(Debug, Clone)]
pub struct NewTunnel {
    pub id: Option<String>,
    pub name: String,
    pub host_id: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

/// Saves a local port forward (`-L bind_port:target_host:target_port`) and
/// returns its id. It listens on the loopback interface of the phone.
pub fn save_tunnel(store: &Store, input: &NewTunnel) -> Result<String, String> {
    let tunnel = Tunnel {
        id: input
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        name: input.name.trim().to_owned(),
        host_id: input.host_id.clone(),
        kind: TunnelKind::Local,
        bind_address: None,
        bind_port: input.bind_port,
        target_host: Some(input.target_host.trim().to_owned()),
        target_port: Some(input.target_port),
    };
    if tunnel.name.is_empty() || tunnel.name.chars().any(char::is_control) {
        return Err("the forward needs a name".into());
    }
    tunnel.forward_spec().map_err(|error| error.to_string())?;
    let config = store.load_app_config().map_err(|error| error.to_string())?;
    if !config.connections.iter().any(|c| c.id == tunnel.host_id) {
        return Err(format!("unknown host: {}", tunnel.host_id));
    }
    store
        .save_tunnel(&tunnel)
        .map_err(|error| error.to_string())?;
    Ok(tunnel.id)
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
            ..NewHost::default()
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
        assert_eq!(target.host_id, with);
        assert!(target.jump.is_empty());
        assert!(target_for(&config, &without)
            .unwrap_err()
            .contains("no user name"));
        assert!(target_for(&config, "missing").is_err());
    }

    #[test]
    fn editing_a_host_keeps_what_the_form_does_not_show() {
        let (_dir, store) = store();
        let id = add_host(&store, &new_host("web", "10.0.0.5", Some("deploy"), 22)).unwrap();
        let mut config = store.load_app_config().unwrap();
        let mut original = config.connections.remove(0);
        original.notes = "restart nginx after deploy".into();
        original.local_forwards = vec!["8080:localhost:80".into()];
        store.save_host(&original, None).unwrap();

        let edited = NewHost {
            id: Some(id.clone()),
            tags: vec!["web".into(), " #Prod ".into(), "WEB".into(), "".into()],
            group: Some("Production".into()),
            favorite: true,
            ..new_host("web 1", "10.0.0.6", Some("root"), 2200)
        };
        assert_eq!(save_host(&store, &edited).unwrap(), id);
        let config = store.load_app_config().unwrap();
        assert_eq!(config.connections.len(), 1, "edited in place, not copied");
        let saved = &config.connections[0];
        assert_eq!(saved.name, "web 1");
        let mut tags = saved.tags.clone();
        tags.sort();
        assert_eq!(tags, vec!["Prod", "web"]);
        let group = config.groups[0].id.clone();
        assert_eq!(saved.group_id.as_deref(), Some(group.as_str()));
        assert!(saved.favorite);
        assert_eq!(saved.notes, "restart nginx after deploy");
        assert_eq!(saved.local_forwards, vec!["8080:localhost:80"]);
        let shown = snapshot(&config);
        assert_eq!(shown["hosts"][0]["target"], "root@10.0.0.6:2200");
        assert_eq!(shown["hosts"][0]["groupId"], group.as_str());
        assert_eq!(shown["groups"][0]["name"], "Production");
        assert!(save_host(
            &store,
            &NewHost {
                id: Some("missing".into()),
                ..new_host("x", "x.example", Some("me"), 22)
            }
        )
        .is_err());
    }

    #[test]
    fn a_group_is_found_by_name_ignoring_case_or_created() {
        let (_dir, store) = store();
        let first = save_host(
            &store,
            &NewHost {
                group: Some(" Lab ".into()),
                ..new_host("pi", "pi.local", Some("pi"), 22)
            },
        )
        .unwrap();
        let second = save_host(
            &store,
            &NewHost {
                group: Some("lab".into()),
                ..new_host("nas", "nas.local", Some("admin"), 22)
            },
        )
        .unwrap();
        let none = save_host(
            &store,
            &NewHost {
                group: Some("  ".into()),
                ..new_host("x", "x.local", Some("me"), 22)
            },
        )
        .unwrap();
        let config = store.load_app_config().unwrap();
        assert_eq!(config.groups.len(), 1, "one group, not two");
        assert_eq!(config.groups[0].name, "Lab");
        let group_of = |id: &str| {
            config
                .connections
                .iter()
                .find(|c| c.id == id)
                .unwrap()
                .group_id
                .clone()
        };
        assert_eq!(group_of(&first), Some(config.groups[0].id.clone()));
        assert_eq!(group_of(&second), group_of(&first));
        assert_eq!(group_of(&none), None);
        assert!(save_host(
            &store,
            &NewHost {
                group: Some("bad\nname".into()),
                ..new_host("y", "y.local", Some("me"), 22)
            }
        )
        .is_err());
    }

    #[test]
    fn tags_are_trimmed_and_unique_ignoring_case() {
        let tags =
            |list: &[&str]| clean_tags(&list.iter().map(|t| t.to_string()).collect::<Vec<_>>());
        assert_eq!(tags(&["a", "A", " b ", "#c", ""]), vec!["a", "b", "c"]);
        assert_eq!(tags(&["x\ny"]), Vec::<String>::new());
        assert_eq!(tags(&[&"z".repeat(41)]), Vec::<String>::new());
    }

    #[test]
    fn a_jump_chain_is_parsed_like_openssh_writes_it() {
        assert_eq!(
            parse_jump("me@bastion:2222, other").unwrap(),
            vec![
                Hop {
                    user: Some("me".into()),
                    host: "bastion".into(),
                    port: 2222
                },
                Hop {
                    user: None,
                    host: "other".into(),
                    port: 22
                },
            ]
        );
        assert_eq!(parse_jump("u@[::1]:2200").unwrap()[0].host, "::1");
        assert_eq!(parse_jump("u@[::1]:2200").unwrap()[0].port, 2200);
        assert_eq!(parse_jump("::1").unwrap()[0].host, "::1");
        assert!(parse_jump("h:0").is_err());
        assert!(parse_jump("h:99999").is_err());
        assert!(parse_jump("-oProxyCommand=x").is_err());
        assert!(parse_jump("[::1").is_err());
        assert_eq!(parse_jump("").unwrap(), vec![]);
        assert_eq!(jump_text("::1", Some("u"), 2200), "u@[::1]:2200");
        assert_eq!(jump_text("h", None, 22), "h");
    }

    #[test]
    fn a_jump_host_becomes_a_hop_of_the_target() {
        let (_dir, store) = store();
        let bastion = add_host(
            &store,
            &new_host("bastion", "b.example", Some("jump"), 2222),
        )
        .unwrap();
        let inner = save_host(
            &store,
            &NewHost {
                jump: Some(jump_text("b.example", Some("jump"), 2222)),
                ..new_host("db", "10.0.0.9", Some("dba"), 22)
            },
        )
        .unwrap();
        let config = store.load_app_config().unwrap();
        let target = target_for(&config, &inner).unwrap();
        assert_eq!(target.jump.len(), 1);
        let hop = &target.jump[0];
        assert_eq!(
            (hop.host.as_str(), hop.port, hop.user.as_str()),
            ("b.example", 2222, "jump")
        );
        // The hop is the saved bastion, so it shares its identity.
        assert_eq!(hop.host_id, bastion);
        assert_eq!(target.key().jump, vec!["jump@b.example:2222"]);
        assert!(snapshot(&config)["hosts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|h| h["command"] == "ssh -J jump@b.example:2222 dba@10.0.0.9"));
    }

    #[test]
    fn a_jump_host_can_be_named_by_a_saved_host_and_defaults_to_the_users_name() {
        let (_dir, store) = store();
        add_host(&store, &new_host("bastion", "b.example", Some("jump"), 22)).unwrap();
        let by_name = save_host(
            &store,
            &NewHost {
                jump: Some("bastion".into()),
                ..new_host("one", "1.example", Some("me"), 22)
            },
        )
        .unwrap();
        let unknown = save_host(
            &store,
            &NewHost {
                jump: Some("elsewhere.example".into()),
                ..new_host("two", "2.example", Some("me"), 22)
            },
        )
        .unwrap();
        let config = store.load_app_config().unwrap();
        assert_eq!(target_for(&config, &by_name).unwrap().jump[0].user, "jump");
        let hop = &target_for(&config, &unknown).unwrap().jump[0];
        assert_eq!(
            (hop.host.as_str(), hop.user.as_str()),
            ("elsewhere.example", "me")
        );
        assert!(hop.host_id.starts_with("jump:"));
        // A bad chain is refused when saving, not when connecting.
        assert!(save_host(
            &store,
            &NewHost {
                jump: Some("h:0".into()),
                ..new_host("bad", "3.example", Some("me"), 22)
            }
        )
        .is_err());
    }

    #[test]
    fn a_forward_is_a_loopback_local_forward_through_a_saved_host() {
        let (_dir, store) = store();
        let host = add_host(&store, &new_host("db", "10.0.0.9", Some("me"), 22)).unwrap();
        let id = save_tunnel(
            &store,
            &NewTunnel {
                id: None,
                name: "Postgres".into(),
                host_id: host.clone(),
                bind_port: 5433,
                target_host: "localhost".into(),
                target_port: 5432,
            },
        )
        .unwrap();
        let config = store.load_app_config().unwrap();
        let tunnel = &config.tunnels[0];
        assert_eq!(tunnel.id, id);
        assert_eq!(tunnel.kind, TunnelKind::Local);
        assert_eq!(tunnel.bind_address, None);
        assert_eq!(tunnel.forward_spec().unwrap(), "5433:localhost:5432");
        let bad = |name: &str, bind: u16, target: u16, host_id: &str| {
            save_tunnel(
                &store,
                &NewTunnel {
                    id: None,
                    name: name.into(),
                    host_id: host_id.into(),
                    bind_port: bind,
                    target_host: "localhost".into(),
                    target_port: target,
                },
            )
        };
        assert!(bad("", 1, 1, &host).is_err());
        assert!(bad("x", 0, 80, &host).is_err());
        assert!(bad("x", 80, 0, &host).is_err());
        assert!(bad("x", 80, 80, "missing").is_err());
    }
}
