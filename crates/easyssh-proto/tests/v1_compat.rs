//! Proves the schema matches what easyssh-core v1 serializes today.

use easyssh_core::domain::{
    AppConfig, CommandSnippet, Connection, ConnectionTarget, DisplayDensity, Group, Locale,
    SessionRecord, SidebarNavigation, Theme, Workspace,
};
use easyssh_core::transfer::{Transfer, TransferDirection, TransferStatus};
use easyssh_core::tunnel::{Tunnel, TunnelKind};
use easyssh_proto::validate;
use serde_json::{json, Value};
use std::path::PathBuf;

fn ok(schema: &str, value: &Value) {
    if let Err(errors) = validate(schema, value) {
        panic!("{schema} should validate but: {errors:#?}\n{value:#}");
    }
}

fn rejected(schema: &str, value: &Value) -> Vec<String> {
    validate(schema, value).expect_err(&format!("{schema} should be rejected: {value}"))
}

fn full_connection() -> Connection {
    let mut c = Connection::alias("prod", "prod-alias");
    c.target = ConnectionTarget::Endpoint {
        hostname: "10.0.0.5".into(),
        username: Some("deploy".into()),
        port: 2222,
    };
    c.group_id = Some("g1".into());
    c.tags = vec!["prod".into(), "db".into()];
    c.favorite = true;
    c.color = Some("chart-3".into());
    c.notes = "primary".into();
    c.proxy_jump = Some("bastion".into());
    c.local_forwards = vec!["8080:localhost:80".into()];
    c.remote_forwards = vec!["9090:localhost:90".into()];
    c.dynamic_forwards = vec!["1080".into()];
    c.remote_command = Some("tmux attach".into());
    c.local_directories = vec!["/home/me/src".into()];
    c.remote_directories = vec!["/srv/app".into()];
    c.terminal.font_size = 16.5;
    c.terminal.scrollback_lines = 50_000;
    c.terminal.startup_command = Some("cd /srv".into());
    c.engine = Some(easyssh_core::EnginePreference::Russh);
    c
}

fn full_config() -> AppConfig {
    let mut config = AppConfig::new();
    config.connections = vec![full_connection(), Connection::alias("home", "home")];
    config.groups = vec![
        Group {
            id: "g1".into(),
            name: "Prod".into(),
            color: Some("chart-1".into()),
            parent_id: None,
        },
        Group {
            id: "g2".into(),
            name: "Db".into(),
            color: None,
            parent_id: Some("g1".into()),
        },
    ];
    config.ssh_engine = Some(easyssh_core::SshEngine::Russh);
    config.recent_connection_ids = vec![config.connections[0].id.clone()];
    config.snippets = vec![CommandSnippet {
        id: "s1".into(),
        name: "disk".into(),
        content: "df -h".into(),
    }];
    config.tunnels = vec![
        Tunnel {
            id: "t1".into(),
            name: "db".into(),
            host_id: "h".into(),
            kind: TunnelKind::Local,
            bind_address: Some("127.0.0.1".into()),
            bind_port: 15432,
            target_host: Some("db".into()),
            target_port: Some(5432),
        },
        Tunnel {
            id: "t2".into(),
            name: "socks".into(),
            host_id: "h".into(),
            kind: TunnelKind::Dynamic,
            bind_address: None,
            bind_port: 1080,
            target_host: None,
            target_port: None,
        },
    ];
    let mut transfer = Transfer::new(
        TransferDirection::Upload,
        PathBuf::from("/tmp/a.txt"),
        "/srv/a.txt".into(),
        false,
    );
    transfer.status = TransferStatus::Completed;
    transfer.started_at = Some(chrono::Utc::now());
    transfer.finished_at = Some(chrono::Utc::now());
    config.transfer_history = vec![transfer];
    config.sessions = vec![SessionRecord {
        id: "x".into(),
        connection_id: Some(config.connections[0].id.clone()),
        name: "prod".into(),
        target: "deploy@10.0.0.5".into(),
        launched_at: chrono::Utc::now(),
        verbose: true,
        launched: true,
        hidden: true,
    }];
    config.sidebar.collapsed_group_ids = vec!["g1".into()];
    config.sidebar.selected_navigation = SidebarNavigation::OpenSshConfig;
    config.sidebar.selected_tag = Some("prod".into());
    config.sync.display_name = "me".into();
    config.sync.repository_path = Some("/r".into());
    config.sync.remote_url = Some("git@example.com:me/h.git".into());
    config.sync.branch = Some("main".into());
    config.sync.last_success_at = Some(chrono::Utc::now());
    config.sync.last_error = Some("boom".into());
    config.sync.last_snapshot_hash = Some("abc".into());
    config.experimental.remote_file_browser = true;
    config.display_density = DisplayDensity::Large;
    config.theme = Theme::Dark;
    config.locale = Locale::ZhCn;
    config.workspace = Workspace::Keys;
    config
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap()
}

#[test]
fn default_app_config_validates() {
    ok("AppConfig", &to_value(&AppConfig::new()));
    ok("AppConfig", &to_value(&AppConfig::default()));
}

#[test]
fn populated_app_config_validates() {
    ok("AppConfig", &to_value(&full_config()));
}

#[test]
fn entities_validate_individually() {
    let config = full_config();
    for c in &config.connections {
        ok("Connection", &to_value(c));
    }
    ok("ConnectionTarget", &to_value(&config.connections[0].target));
    ok("ConnectionTarget", &to_value(&config.connections[1].target));
    for g in &config.groups {
        ok("Group", &to_value(g));
    }
    ok("CommandSnippet", &to_value(&config.snippets[0]));
    for t in &config.tunnels {
        ok("Tunnel", &to_value(t));
    }
    ok("TerminalPreferences", &to_value(&config.terminal));
    ok("SidebarPreferences", &to_value(&config.sidebar));
    ok("SyncPreferences", &to_value(&config.sync));
    ok("ExperimentalFeatures", &to_value(&config.experimental));
    ok("TelemetryPreferences", &to_value(&config.telemetry));
    ok("SessionRecord", &to_value(&config.sessions[0]));
    ok("Transfer", &to_value(&config.transfer_history[0]));
}

#[test]
fn every_enum_variant_validates() {
    for v in [TunnelKind::Local, TunnelKind::Remote, TunnelKind::Dynamic] {
        ok("TunnelKind", &to_value(&v));
    }
    for v in [Theme::Light, Theme::Dark, Theme::System] {
        ok("Theme", &to_value(&v));
    }
    for v in [Locale::En, Locale::ZhCn, Locale::System] {
        ok("Locale", &to_value(&v));
    }
    for v in [
        DisplayDensity::Compact,
        DisplayDensity::Comfortable,
        DisplayDensity::Large,
    ] {
        ok("DisplayDensity", &to_value(&v));
    }
    for v in [
        SidebarNavigation::All,
        SidebarNavigation::Favorites,
        SidebarNavigation::Recent,
        SidebarNavigation::OpenSshConfig,
        SidebarNavigation::Ungrouped,
    ] {
        ok("SidebarNavigation", &to_value(&v));
    }
    for v in [
        Workspace::Home,
        Workspace::Hosts,
        Workspace::Transfers,
        Workspace::Keys,
        Workspace::Settings,
        Workspace::Files,
        Workspace::Snippets,
        Workspace::Forwarding,
    ] {
        ok("Workspace", &to_value(&v));
    }
    for v in [TransferDirection::Upload, TransferDirection::Download] {
        ok("TransferDirection", &to_value(&v));
    }
    for v in [
        TransferStatus::Queued,
        TransferStatus::Pending,
        TransferStatus::Authorizing,
        TransferStatus::Transferring,
        TransferStatus::Completed,
        TransferStatus::Failed,
        TransferStatus::Cancelled,
        TransferStatus::Interrupted,
    ] {
        ok("TransferStatus", &to_value(&v));
    }
}

#[test]
fn v1_config_with_defaulted_fields_missing_still_validates() {
    // An older file lacks every `#[serde(default)]` field.
    let mut value = to_value(&AppConfig::new());
    let object = value.as_object_mut().unwrap();
    for key in [
        "tunnels",
        "terminal",
        "sessions",
        "transfer_history",
        "workspace",
        "display_density",
        "sidebar",
        "sync",
        "experimental",
        "telemetry",
    ] {
        object.remove(key);
    }
    ok("AppConfig", &value);
    // ...and v1 itself accepts it, which is what the schema mirrors.
    serde_json::from_value::<AppConfig>(value).unwrap();
}

#[test]
fn generated_types_round_trip_v1_json() {
    let value = to_value(&full_config());
    let generated: easyssh_proto::AppConfig = serde_json::from_value(value.clone()).unwrap();
    let back = serde_json::to_value(&generated).unwrap();
    ok("AppConfig", &back);
    // And v1 reads what the generated types write.
    let v1: AppConfig = serde_json::from_value(back).unwrap();
    assert_eq!(to_value(&v1), value);
}

#[test]
fn unknown_field_is_rejected() {
    let mut value = to_value(&full_connection());
    value["surprise"] = json!(1);
    rejected("Connection", &value);

    let mut target = to_value(&full_connection().target);
    target["extra"] = json!(true);
    rejected("ConnectionTarget", &target);
}

#[test]
fn wrong_enum_value_is_rejected() {
    let mut tunnel = to_value(&full_config().tunnels[0]);
    tunnel["kind"] = json!("sideways");
    rejected("Tunnel", &tunnel);

    let mut config = to_value(&AppConfig::new());
    config["theme"] = json!("Dark");
    rejected("AppConfig", &config);

    let mut config = to_value(&AppConfig::new());
    config["locale"] = json!("zh-CN");
    rejected("AppConfig", &config);
}

#[test]
fn missing_required_field_is_rejected() {
    let mut value = to_value(&full_connection());
    value.as_object_mut().unwrap().remove("name");
    let errors = rejected("Connection", &value);
    assert!(errors.iter().any(|e| e.contains("name")), "{errors:?}");

    // Nullable-but-required: v1 always writes `group_id`, even as null.
    let mut value = to_value(&full_connection());
    value.as_object_mut().unwrap().remove("group_id");
    rejected("Connection", &value);

    let mut config = to_value(&AppConfig::new());
    config.as_object_mut().unwrap().remove("connections");
    rejected("AppConfig", &config);
}

#[test]
fn wrong_types_and_bad_tagged_target_are_rejected() {
    let mut value = to_value(&full_connection());
    value["target"] =
        json!({ "kind": "endpoint", "hostname": "h", "username": null, "port": 70000 });
    rejected("Connection", &value);

    value["target"] = json!({ "kind": "bastion", "alias": "x" });
    rejected("Connection", &value);

    // Endpoint without its required members.
    value["target"] = json!({ "kind": "endpoint", "hostname": "h" });
    rejected("Connection", &value);

    let mut value = to_value(&full_connection());
    value["created_at"] = json!("yesterday");
    rejected("Connection", &value);
}

#[test]
fn unknown_schema_name_is_an_error() {
    assert!(validate("Nope", &json!({})).is_err());
}

#[test]
fn schema_version_constant_matches_schema() {
    let schema: Value = serde_json::from_str(easyssh_proto::SCHEMA_JSON).unwrap();
    assert_eq!(
        schema["$defs"]["SchemaVersion"]["const"],
        json!(easyssh_proto::SCHEMA_VERSION)
    );
}

#[test]
fn every_declared_kind_has_a_schema() {
    let schema: Value = serde_json::from_str(easyssh_proto::SCHEMA_JSON).unwrap();
    let defs = schema["$defs"].as_object().unwrap();
    for (_, params, result) in easyssh_proto::COMMANDS {
        assert!(defs.contains_key(*params), "{params}");
        assert!(result.is_empty() || defs.contains_key(*result), "{result}");
    }
    for (_, payload) in easyssh_proto::EVENTS {
        assert!(defs.contains_key(*payload), "{payload}");
    }
}

#[test]
fn envelopes() {
    let id = "7b0e5c1e-6d0a-4f7e-9d34-0a6b6a3f5b11";
    ok(
        "Command",
        &json!({ "id": id, "kind": "hosts.list", "payload": {} }),
    );
    rejected(
        "Command",
        &json!({ "id": "not-a-uuid", "kind": "hosts.list", "payload": {} }),
    );
    rejected("Command", &json!({ "id": id, "kind": "hosts.list" }));
    ok(
        "Event",
        &json!({ "kind": "session.exit", "payload": {}, "seq": 0 }),
    );
    rejected(
        "Event",
        &json!({ "kind": "session.exit", "payload": {}, "seq": -1 }),
    );

    ok("Reply", &json!({ "id": id, "ok": null }));
    ok("Reply", &json!({ "id": id, "ok": { "id": "x" } }));
    ok(
        "Reply",
        &json!({ "id": id, "error": { "code": "not_found", "message": "no such host" } }),
    );
    // Exactly one of ok / error.
    rejected(
        "Reply",
        &json!({ "id": id, "ok": 1, "error": { "code": "c", "message": "m" } }),
    );
    rejected("Reply", &json!({ "id": id }));
    rejected("ProtoError", &json!({ "code": "x" }));
}

#[test]
fn command_and_event_payloads_follow_their_kind() {
    use easyssh_proto::{validate_command, validate_event};
    let id = "7b0e5c1e-6d0a-4f7e-9d34-0a6b6a3f5b11";
    let command =
        |kind: &str, payload: Value| json!({ "id": id, "kind": kind, "payload": payload });

    validate_command(&command("hosts.list", json!({}))).unwrap();
    validate_command(&command(
        "hosts.save",
        json!({ "connection": to_value(&full_connection()) }),
    ))
    .unwrap();
    validate_command(&command("hosts.delete", json!({ "id": "h" }))).unwrap();
    validate_command(&command(
        "session.open",
        json!({ "host_id": null, "cols": 80, "rows": 24 }),
    ))
    .unwrap();
    validate_command(&command(
        "session.write",
        json!({ "session_id": "s", "data": "ls\r" }),
    ))
    .unwrap();
    validate_command(&command(
        "session.resize",
        json!({ "session_id": "s", "cols": 100, "rows": 30 }),
    ))
    .unwrap();
    validate_command(&command("session.close", json!({ "session_id": "s" }))).unwrap();
    validate_command(&command("proto.hello", json!({ "schema_version": 1 }))).unwrap();

    assert!(validate_command(&command("session.write", json!({ "session_id": "s" }))).is_err());
    // Any version number is a valid hello: the daemon answers a version it
    // does not speak with the `schema_mismatch` error instead of a schema one.
    validate_command(&command(
        "proto.hello",
        json!({ "schema_version": 2, "app_version": "1.0.2" }),
    ))
    .unwrap();
    assert!(validate_command(&command("proto.hello", json!({ "schema_version": "1" }))).is_err());
    assert!(validate_command(&command(
        "proto.hello",
        json!({ "schema_version": 1, "unknown": true })
    ))
    .is_err());
    validate_command(&command("daemon.shutdown_when_idle", json!({}))).unwrap();
    assert!(validate_command(&command("nope.nothing", json!({}))).is_err());

    let event = |kind: &str, payload: Value, seq: u64| json!({ "kind": kind, "payload": payload, "seq": seq });
    validate_event(&event(
        "session.output",
        json!({ "session_id": "s", "data": "hi" }),
        1,
    ))
    .unwrap();
    validate_event(&event(
        "session.exit",
        json!({ "session_id": "s", "code": null }),
        2,
    ))
    .unwrap();
    validate_event(&event(
        "askpass.prompt",
        json!({ "id": 1, "host": "h", "prompt": "password: ", "confirm": false }),
        3,
    ))
    .unwrap();
    assert!(validate_event(&event("session.exit", json!({ "session_id": "s" }), 4)).is_err());
}
