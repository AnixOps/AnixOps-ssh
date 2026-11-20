mod common;

use common::{fixture, Fake, Request, Response};
use easyssh_services::control::{self, parse_tags, Node, NodeList};
use easyssh_services::{ServiceError, ServiceForm};
use easyssh_store::import::{import_apply, ApplyRequest, DuplicatePolicy, GroupMode};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

const TOKEN: &str = "anixadm_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789-_abcde";

fn form(url: &str) -> ServiceForm {
    ServiceForm {
        url: url.to_owned(),
        default_user: "ops".to_owned(),
        ..ServiceForm::default()
    }
}

fn node(id: u64, name: &str, host: &str) -> Value {
    json!({
        "id": id, "name": name, "host": host, "server_ip": null, "tags": null,
        "status": 1, "node_group_name": "", "approval": "approved"
    })
}

fn nodes_reply(request: &Request, all: &[Value]) -> Response {
    let page: usize = request.query("page").unwrap().parse().unwrap();
    let size: usize = request.query("page_size").unwrap().parse().unwrap();
    let list: Vec<&Value> = all.iter().skip((page - 1) * size).take(size).collect();
    Response::json(
        200,
        &json!({"data": {"total": all.len(), "list": list}}).to_string(),
    )
}

#[test]
fn maps_nodes_with_nulls_tags_groups_and_skips_unapproved() {
    let list: Vec<Node> = serde_json::from_value(json!([
        {"id": 1, "name": "web-1", "host": "web1.example.com", "server_ip": "192.0.2.10",
         "tags": "[\"prod\",\"web\",\"prod\"]", "node_group_name": "Edge", "approval": "approved"},
        {"id": 2, "name": "db-1", "host": "", "server_ip": "192.0.2.20", "tags": null,
         "node_group_name": null, "approval": ""},
        {"id": 3, "name": "pending-1", "host": "p.example.com", "approval": "pending"},
        {"id": 4, "name": "gone", "host": "g.example.com", "approval": "rejected"},
        {"id": 5, "name": "empty", "host": null, "server_ip": null},
        {"id": 6, "name": "", "host": "198.51.100.6", "tags": "a, b ,,", "node_group_name": "  "}
    ]))
    .unwrap();
    let mut f = form("https://Panel.Example.com/");
    f.identity_file = Some("~/.ssh/ops_ed25519".to_owned());
    let fetched = control::map_nodes(
        &f,
        NodeList {
            nodes: list,
            total: 6,
            warnings: Vec::new(),
        },
    )
    .unwrap();

    assert_eq!(fetched.scope, "control:https://panel.example.com#");
    assert_eq!(fetched.hosts.len(), 3);
    assert_eq!(fetched.skipped, 3, "pending, rejected and no address");
    let web = &fetched.hosts[0];
    assert_eq!(web.label, "web-1");
    assert_eq!(web.hostname, "web1.example.com", "host wins over server_ip");
    assert_eq!(web.port, 22);
    assert_eq!(web.username.as_deref(), Some("ops"));
    assert_eq!(web.group_path, ["panel.example.com", "Edge"]);
    assert_eq!(web.tags, ["prod", "web"]);
    assert_eq!(
        web.source.as_deref(),
        Some("control:https://panel.example.com#1")
    );
    assert_eq!(web.identity_file.as_deref(), Some("~/.ssh/ops_ed25519"));
    let db = &fetched.hosts[1];
    assert_eq!(db.hostname, "192.0.2.20", "falls back to server_ip");
    assert_eq!(db.group_path, ["panel.example.com", "AnixOps Control"]);
    assert!(db.tags.is_empty());
    let unnamed = &fetched.hosts[2];
    assert_eq!(unnamed.label, "node-6");
    assert_eq!(unnamed.tags, ["a", "b"]);
    assert_eq!(unnamed.group_path[1], "AnixOps Control");
    let warnings = fetched.warnings.join("\n");
    assert!(
        warnings.contains("empty: the node has no address"),
        "{warnings}"
    );
    assert!(
        warnings.contains("2 node(s) are not approved"),
        "{warnings}"
    );
}

#[test]
fn tags_come_in_several_shapes() {
    let some = |v: Value| parse_tags(Some(&v));
    assert_eq!(some(json!("[\"x\",\"y\"]")), ["x", "y"]);
    assert_eq!(some(json!(["x", 7, null, "x"])), ["x", "7"]);
    assert_eq!(some(json!("x,y")), ["x", "y"]);
    assert_eq!(some(json!("[]")), Vec::<String>::new());
    assert_eq!(some(json!("")), Vec::<String>::new());
    assert!(parse_tags(None).is_empty());
    assert!(parse_tags(Some(&json!(5))).is_empty());
    let long = "t".repeat(100);
    assert_eq!(some(json!([long])).remove(0).chars().count(), 48);
}

#[test]
fn reads_every_page_with_the_token_in_the_header_only() {
    let all: Vec<Value> = (1..=230)
        .map(|i| node(i, &format!("n{i}"), &format!("h{i}.example.com")))
        .collect();
    let fake = Fake::start(move |request| nodes_reply(request, &all));
    let fetched = control::fetch(&form(&fake.url), TOKEN).unwrap();
    assert_eq!(fetched.hosts.len(), 230);
    assert_eq!(fetched.hosts[229].label, "n230");

    let seen = fake.seen();
    assert_eq!(seen.len(), 3, "three pages of at most 100");
    for request in &seen {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path(), "/api/v4/admin/nodes");
        assert_eq!(request.query("page_size").as_deref(), Some("100"));
        assert_eq!(
            request.header("authorization"),
            Some(format!("Bearer {TOKEN}").as_str())
        );
        assert!(
            !request.target.contains("anixadm_"),
            "the token must never be in a URL: {}",
            request.target
        );
    }
}

#[test]
fn stops_when_the_panel_lists_fewer_than_it_counts() {
    // total says 500 but the list ends: no endless loop.
    let fake = Fake::start(|request| {
        let page = request.query("page").unwrap();
        let list = if page == "1" {
            vec![node(1, "a", "a.example.com")]
        } else {
            vec![]
        };
        Response::json(
            200,
            &json!({"data": {"total": 500, "list": list}}).to_string(),
        )
    });
    let fetched = control::fetch(&form(&fake.url), TOKEN).unwrap();
    assert_eq!(fetched.hosts.len(), 1);
    assert_eq!(fake.seen().len(), 2);
}

#[test]
fn a_panel_path_prefix_is_kept() {
    let fake = Fake::start(|request| {
        assert_eq!(request.path(), "/panel/api/v4/admin/nodes");
        Response::json(200, r#"{"data":{"total":0,"list":[]}}"#)
    });
    let total = control::test(&form(&format!("{}/panel/", fake.url)), TOKEN).unwrap();
    assert_eq!(total, 0);
    assert_eq!(fake.seen().len(), 1);
}

#[test]
fn test_connection_asks_for_one_node_and_reports_the_total() {
    let all: Vec<Value> = (1..=7).map(|i| node(i, "n", "h.example.com")).collect();
    let fake = Fake::start(move |request| nodes_reply(request, &all));
    assert_eq!(control::test(&form(&fake.url), TOKEN).unwrap(), 7);
    let seen = fake.seen();
    assert_eq!(seen[0].query("page_size").as_deref(), Some("1"));
}

#[test]
fn failures_say_what_happened_and_never_echo_the_token() {
    let status = Arc::new(Mutex::new(401u16));
    let shared = status.clone();
    let fake = Fake::start(move |_| match *shared.lock().unwrap() {
        401 => Response::json(
            401,
            &format!(r#"{{"error":{{"code":"api_token_invalid","message":"bad {TOKEN}"}}}}"#),
        ),
        403 => Response::json(
            403,
            r#"{"error":{"code":"api_token_forbidden","message":"scope"}}"#,
        ),
        429 => Response::json(
            429,
            r#"{"error":{"code":"api_token_rate_limited","message":"slow down"}}"#,
        )
        .with_header("Retry-After", "42"),
        500 => Response::json(500, &format!(r#"{{"error":{{"message":"boom {TOKEN}"}}}}"#)),
        404 => Response::json(404, "not found"),
        _ => Response::json(200, r#"{"hello":"world"}"#),
    });
    let f = form(&fake.url);
    let run = |code: u16| {
        *status.lock().unwrap() = code;
        control::fetch(&f, TOKEN).unwrap_err()
    };

    let e = run(401);
    assert!(matches!(e, ServiceError::Unauthorized(_)), "{e:?}");
    assert!(e.to_string().contains("wrong, revoked or expired"));
    let e = run(403);
    assert!(matches!(e, ServiceError::Forbidden(_)), "{e:?}");
    assert!(e.to_string().contains("scope read or admin"));
    let e = run(429);
    assert_eq!(
        e,
        ServiceError::RateLimited {
            retry_after: Some(42)
        }
    );
    assert!(e.to_string().contains("42 seconds"));
    let e = run(500);
    assert!(matches!(e, ServiceError::Status(500, _)), "{e:?}");
    assert!(e.to_string().starts_with("The service answered 500"));
    let e = run(404);
    assert!(e.to_string().contains("AnixOps Control panel"), "{e}");
    let e = run(200);
    assert!(matches!(e, ServiceError::Protocol(_)), "{e:?}");
    for code in [401, 403, 429, 500, 404, 200] {
        assert!(
            !run(code).to_string().contains("AbCdEf"),
            "the token leaked in the message for {code}"
        );
    }
}

#[test]
fn refuses_unsafe_addresses_and_odd_tokens_without_sending() {
    let fake = Fake::start(|_| Response::json(200, r#"{"data":{"total":0,"list":[]}}"#));
    for url in [
        "http://panel.example.com",
        "ftp://panel.example.com",
        "https://user:pw@panel.example.com",
        "https://panel.example.com/?token=1",
        "",
        "https://",
    ] {
        let e = control::test(&form(url), TOKEN).unwrap_err();
        assert!(matches!(e, ServiceError::Invalid(_)), "{url}: {e:?}");
    }
    for token in ["", "not-a-token", "anixadm_a b", "anixadm_\r\nX: 1"] {
        let e = control::test(&form(&fake.url), token).unwrap_err();
        assert!(matches!(e, ServiceError::Invalid(_)), "{token:?}: {e:?}");
    }
    assert!(fake.seen().is_empty(), "nothing was sent");
    // A bare host means https.
    let origin = control::scope(&form("panel.example.com")).unwrap();
    assert_eq!(origin, "control:https://panel.example.com#");
    // localhost over http is for testing.
    assert!(control::scope(&form("http://localhost:8080")).is_ok());
}

fn apply(
    store: &easyssh_store::Store,
    fetched: easyssh_services::Fetched,
) -> easyssh_store::import::ApplyOutcome {
    import_apply(
        store,
        &ApplyRequest {
            hosts: fetched.hosts,
            group_mode: GroupMode::Source,
            group_id: None,
            duplicates: DuplicatePolicy::Update,
        },
    )
    .unwrap()
}

#[test]
fn importing_again_updates_instead_of_duplicating_and_reports_what_left() {
    let (_vault, store) = fixture();
    let nodes = Arc::new(Mutex::new(vec![
        node(1, "web-1", "web1.example.com"),
        node(2, "web-2", "web2.example.com"),
        node(3, "db-1", "db1.example.com"),
    ]));
    let shared = nodes.clone();
    let fake = Fake::start(move |request| nodes_reply(request, &shared.lock().unwrap()));
    let f = form(&fake.url);

    // First import: everything is new, in "<panel host>/AnixOps Control".
    let preview = control::fetch(&f, TOKEN).unwrap().preview(&store).unwrap();
    assert_eq!(preview.hosts.len(), 3);
    assert!(preview
        .hosts
        .iter()
        .all(|h| h.exists == Some(false) && h.problem.is_none()));
    assert!(preview.removed.is_empty());
    assert_eq!(
        preview.groups,
        [
            vec!["127.0.0.1".to_owned()],
            vec!["127.0.0.1".to_owned(), "AnixOps Control".to_owned()]
        ]
    );
    let outcome = apply(&store, control::fetch(&f, TOKEN).unwrap());
    assert_eq!((outcome.created, outcome.updated), (3, 0));
    assert_eq!(store.list_hosts().unwrap().len(), 3);
    assert_eq!(store.list_groups().unwrap().len(), 2);

    // Nothing changed on the panel: all exist, nothing is written.
    let again = control::fetch(&f, TOKEN).unwrap().preview(&store).unwrap();
    assert!(again.hosts.iter().all(|h| h.exists == Some(true)));
    let outcome = apply(&store, control::fetch(&f, TOKEN).unwrap());
    assert_eq!(
        (outcome.created, outcome.updated, outcome.unchanged),
        (0, 0, 3)
    );

    // The panel renames web-1, moves web-2 to a new address, drops db-1 and
    // adds app-1.
    {
        let mut list = nodes.lock().unwrap();
        list[0]["name"] = json!("web-one");
        list[1]["host"] = json!("web2-new.example.com");
        list.remove(2);
        list.push(node(4, "app-1", "app1.example.com"));
    }
    let preview = control::fetch(&f, TOKEN).unwrap().preview(&store).unwrap();
    assert_eq!(preview.removed, ["db-1"]);
    let moved = preview.hosts.iter().find(|h| h.label == "web-2").unwrap();
    assert_eq!(
        moved.exists,
        Some(true),
        "found by source key, not by address"
    );
    let outcome = apply(&store, control::fetch(&f, TOKEN).unwrap());
    assert_eq!((outcome.created, outcome.updated), (1, 2), "{outcome:?}");

    let hosts = store.list_hosts().unwrap();
    assert_eq!(
        hosts.len(),
        4,
        "db-1 stays: nothing is deleted automatically"
    );
    let names: Vec<String> = hosts
        .iter()
        .map(|r| r.item.connection.name.clone())
        .collect();
    for name in ["web-one", "web-2", "db-1", "app-1"] {
        assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
    }
    let web2 = hosts
        .iter()
        .find(|r| r.item.connection.name == "web-2")
        .unwrap();
    let easyssh_core::ConnectionTarget::Endpoint {
        hostname, username, ..
    } = &web2.item.connection.target
    else {
        panic!("endpoint");
    };
    assert_eq!(hostname, "web2-new.example.com");
    assert_eq!(username.as_deref(), Some("ops"));

    // Hosts stay bound to their node even after the user edits the name.
    let outcome = apply(&store, control::fetch(&f, TOKEN).unwrap());
    assert_eq!((outcome.created, outcome.updated), (0, 0));
    assert_eq!(store.list_hosts().unwrap().len(), 4);
}

#[test]
fn tags_reach_the_stored_host_and_a_host_deleted_by_the_user_comes_back_as_new() {
    let (_vault, store) = fixture();
    let mut one = node(1, "web-1", "web1.example.com");
    one["tags"] = json!("[\"prod\"]");
    let all = vec![one];
    let fake = Fake::start(move |request| nodes_reply(request, &all));
    let f = form(&fake.url);
    apply(&store, control::fetch(&f, TOKEN).unwrap());
    let hosts = store.list_hosts().unwrap();
    assert_eq!(hosts[0].item.connection.tags, ["prod"]);
    store.remove_host(&hosts[0].item.connection.id).unwrap();
    let preview = control::fetch(&f, TOKEN).unwrap().preview(&store).unwrap();
    assert_eq!(preview.hosts[0].exists, Some(false));
    assert!(
        preview.removed.is_empty(),
        "a host the user deleted is not reported"
    );
    assert_eq!(apply(&store, control::fetch(&f, TOKEN).unwrap()).created, 1);
}
