mod common;

use common::{fixture, Fake, Request, Response};
use easyssh_services::jumpserver::{self, authorization, signature, Asset, AssetList, DEFAULT_ORG};
use easyssh_services::{ServiceError, ServiceForm};
use easyssh_store::import::{import_apply, ApplyRequest, DuplicatePolicy, GroupMode};
use serde_json::{json, Value};

const KEY_ID: &str = "7a1b2c3d-1111-4222-8333-444455556666";
const SECRET: &str = "0f0e0d0c-aaaa-4bbb-8ccc-123456789abc";

fn form(url: &str) -> ServiceForm {
    ServiceForm {
        url: url.to_owned(),
        access_key_id: KEY_ID.to_owned(),
        gateway: false,
        default_user: "root".to_owned(),
        ..ServiceForm::default()
    }
}

fn gateway_form(url: &str) -> ServiceForm {
    ServiceForm {
        gateway: true,
        gateway_user: "alice".to_owned(),
        ..form(url)
    }
}

#[test]
fn the_signature_matches_a_fixed_vector() {
    // Computed with python3 reproducing SignatureAuth in anixops-ansible
    // scripts/jms.py:
    //   hmac.new(secret, "\n".join([
    //       "(request-target): get /api/v1/assets/hosts/?limit=100&offset=0",
    //       "accept: application/json",
    //       "date: Thu, 01 Jan 2026 00:00:00 GMT"]).encode(), sha256) -> base64
    let sig = signature(
        SECRET,
        "GET",
        "/api/v1/assets/hosts/?limit=100&offset=0",
        "application/json",
        "Thu, 01 Jan 2026 00:00:00 GMT",
    );
    assert_eq!(sig, "5uEeiPgQsBImVHBvzr6XHe4S7nT/dahE3wHM1dZIKPE=");
    let other = signature(
        SECRET,
        "get",
        "/api/v1/assets/hosts/?limit=1&offset=0",
        "application/json",
        "Thu, 01 Jan 2026 00:00:00 GMT",
    );
    assert_eq!(other, "Xx0UbBlWgHXhWTg4zDbu8NpgqyPtTJU0cPEHMKJmqVU=");
    assert_eq!(
        authorization(KEY_ID, &sig),
        format!(
            "Signature keyId=\"{KEY_ID}\",algorithm=\"hmac-sha256\",headers=\"(request-target) accept date\",signature=\"{sig}\""
        )
    );
}

/// What JumpServer does: recompute the signature from the request.
fn verified(request: &Request) -> bool {
    let (Some(auth), Some(date), Some(accept)) = (
        request.header("authorization"),
        request.header("date"),
        request.header("accept"),
    ) else {
        return false;
    };
    let expected = authorization(
        KEY_ID,
        &signature(SECRET, &request.method, &request.target, accept, date),
    );
    auth == expected
}

fn asset(i: usize) -> Value {
    json!({
        "id": format!("id-{i}"), "name": format!("host-{i}"), "address": format!("10.0.{}.{}", i / 250, i % 250),
        "is_active": true, "platform": {"id": 1, "name": "Linux"},
        "protocols": [{"name": "ssh", "port": 22}], "nodes_display": ["/Default/Prod/Web"]
    })
}

fn page(request: &Request, all: &[Value]) -> Response {
    if !verified(request) {
        return Response::json(401, r#"{"detail":"bad signature"}"#);
    }
    let limit: usize = request.query("limit").unwrap().parse().unwrap();
    let offset: usize = request.query("offset").unwrap().parse().unwrap();
    let results: Vec<&Value> = all.iter().skip(offset).take(limit).collect();
    let next = if offset + limit < all.len() {
        json!("http://example.invalid/next")
    } else {
        Value::Null
    };
    Response::json(
        200,
        &json!({"count": all.len(), "next": next, "previous": null, "results": results})
            .to_string(),
    )
}

#[test]
fn reads_every_page_signed_with_the_org_header() {
    let all: Vec<Value> = (0..230).map(asset).collect();
    let fake = Fake::start(move |request| page(request, &all));
    let fetched = jumpserver::fetch(&form(&fake.url), SECRET).unwrap();
    assert_eq!(fetched.hosts.len(), 230);

    let seen = fake.seen();
    assert_eq!(seen.len(), 3);
    for (index, request) in seen.iter().enumerate() {
        assert_eq!(request.method, "GET");
        assert_eq!(request.path(), "/api/v1/assets/hosts/");
        assert_eq!(request.query("limit").as_deref(), Some("100"));
        assert_eq!(request.query("offset"), Some((index * 100).to_string()));
        assert!(verified(request), "the signature must verify");
        assert_eq!(request.header("x-jms-org"), Some(DEFAULT_ORG));
        assert_eq!(request.header("accept"), Some("application/json"));
        assert!(!request.target.contains(SECRET) && !request.target.contains(KEY_ID));
    }
}

#[test]
fn a_named_organization_is_sent_and_part_of_the_source_key() {
    let fake = Fake::start(|request| {
        assert_eq!(request.header("x-jms-org"), Some("org-42"));
        Response::json(200, r#"{"count":0,"next":null,"results":[]}"#)
    });
    let mut f = form(&fake.url);
    f.org_id = "org-42".into();
    assert_eq!(jumpserver::test(&f, SECRET).unwrap(), 0);
    assert!(jumpserver::scope(&f).unwrap().ends_with("#org-42#"));
}

#[test]
fn an_unpaginated_answer_is_read_too() {
    let fake = Fake::start(|_| Response::json(200, &json!([asset(1), asset(2)]).to_string()));
    let fetched = jumpserver::fetch(&form(&fake.url), SECRET).unwrap();
    assert_eq!(fetched.hosts.len(), 2);
    assert_eq!(fake.seen().len(), 1);
}

fn assets(value: Value) -> AssetList {
    let assets: Vec<Asset> = serde_json::from_value(value).unwrap();
    AssetList {
        total: assets.len(),
        assets,
        warnings: Vec::new(),
    }
}

fn fixture_assets() -> Value {
    json!([
        {"id": "a1", "name": "web-1", "address": "web1.example.com", "is_active": true,
         "platform": {"id": 1, "name": "Linux"}, "protocols": [{"name": "ssh", "port": 2200}],
         "nodes_display": ["/Default/Prod/Web", "/Default/Other"]},
        {"id": "a2", "name": "win-1", "address": "192.0.2.50", "is_active": true,
         "platform": {"id": 5, "name": "Windows"}, "protocols": [{"name": "rdp", "port": 3389}],
         "nodes_display": ["/Default/Prod"]},
        {"id": "a3", "name": "off-1", "address": "192.0.2.51", "is_active": false,
         "platform": {"name": "Linux"}, "protocols": [{"name": "ssh", "port": 22}]},
        {"id": "a4", "name": "", "address": "198.51.100.4", "platform": "Unix",
         "nodes_display": ["/Default"]},
        {"id": "a5", "name": "win-ssh", "address": "192.0.2.52", "platform": {"name": "Windows"}},
        {"id": "a6", "name": "noaddr", "address": "", "protocols": [{"name": "ssh", "port": 22}]},
        {"name": "noid", "address": "192.0.2.60"},
        {"id": "a8", "name": "db", "address": "192.0.2.70", "protocols": [{"name": "mysql", "port": 3306}]}
    ])
}

#[test]
fn maps_assets_directly_when_the_gateway_is_off() {
    let fetched =
        jumpserver::map_assets(&form("https://JMS.example.com/"), assets(fixture_assets()))
            .unwrap();
    assert_eq!(
        fetched.scope,
        format!("jumpserver:https://jms.example.com#{DEFAULT_ORG}#")
    );
    let labels: Vec<&str> = fetched.hosts.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(labels, ["web-1", "198.51.100.4"]);
    let web = &fetched.hosts[0];
    assert_eq!(
        (web.hostname.as_str(), web.port),
        ("web1.example.com", 2200)
    );
    assert_eq!(web.username.as_deref(), Some("root"));
    assert_eq!(web.group_path, ["jms.example.com", "Prod", "Web"]);
    assert_eq!(
        web.source.as_deref(),
        Some(format!("jumpserver:https://jms.example.com#{DEFAULT_ORG}#a1").as_str())
    );
    let unnamed = &fetched.hosts[1];
    assert_eq!(unnamed.port, 22, "no protocols and not Windows: SSH on 22");
    assert_eq!(
        unnamed.group_path,
        ["jms.example.com"],
        "only the root node: no folder"
    );
    // win-1 (RDP), off-1 (disabled), win-ssh (Windows without protocols),
    // noaddr, noid and db (MySQL only) are skipped.
    assert_eq!(fetched.skipped, 6);
    let warnings = fetched.warnings.join("\n");
    assert!(warnings.contains("1 asset(s) are disabled"), "{warnings}");
    assert!(warnings.contains("win-1 (Windows)"), "{warnings}");
    assert!(warnings.contains("db"), "{warnings}");
    assert!(
        warnings.contains("noaddr: the asset has no address"),
        "{warnings}"
    );
    assert!(warnings.contains("noid: the asset has no id"), "{warnings}");
}

#[test]
fn gateway_mode_points_every_host_at_koko() {
    let fetched = jumpserver::map_assets(
        &gateway_form("https://jms.example.com"),
        assets(fixture_assets()),
    )
    .unwrap();
    for host in &fetched.hosts {
        assert_eq!(
            host.hostname, "jms.example.com",
            "defaults to the JumpServer host"
        );
        assert_eq!(host.port, 2222);
        assert_eq!(
            host.username.as_deref(),
            Some("alice"),
            "the JumpServer user"
        );
    }
    assert!(fetched.hosts[0].notes.contains("/web-1"));
    assert!(fetched.hosts[0].notes.contains("web1.example.com:2200"));

    let mut f = gateway_form("https://jms.example.com");
    f.gateway_host = "koko.example.com".into();
    f.gateway_port = 22222;
    let fetched = jumpserver::map_assets(&f, assets(fixture_assets())).unwrap();
    assert_eq!(fetched.hosts[0].hostname, "koko.example.com");
    assert_eq!(fetched.hosts[0].port, 22222);

    f.gateway_user = " ".into();
    let e = jumpserver::map_assets(&f, assets(fixture_assets())).unwrap_err();
    assert!(matches!(e, ServiceError::Invalid(_)));
    assert!(e.to_string().contains("JumpServer user name"));
}

#[test]
fn errors_are_plain_and_never_echo_the_secret() {
    let status = std::sync::Arc::new(std::sync::Mutex::new(401u16));
    let shared = status.clone();
    let fake = Fake::start(move |_| match *shared.lock().unwrap() {
        401 => Response::json(401, &format!(r#"{{"detail":"bad {SECRET}"}}"#)),
        403 => Response::json(403, r#"{"detail":"no"}"#),
        404 => Response::json(404, "nope"),
        500 => Response::json(500, &format!(r#"{{"detail":"boom {SECRET}"}}"#)),
        _ => Response::json(200, r#"{"hello":1}"#),
    });
    let f = form(&fake.url);
    let run = |code: u16| {
        *status.lock().unwrap() = code;
        jumpserver::fetch(&f, SECRET).unwrap_err()
    };
    assert!(matches!(run(401), ServiceError::Unauthorized(_)));
    assert!(run(401).to_string().contains("clock"));
    assert!(matches!(run(403), ServiceError::Forbidden(_)));
    assert!(run(404).to_string().contains("JumpServer"));
    assert!(matches!(run(500), ServiceError::Status(500, _)));
    assert!(matches!(run(200), ServiceError::Protocol(_)));
    for code in [401, 403, 404, 500, 200] {
        assert!(!run(code).to_string().contains("123456789abc"), "{code}");
    }
}

#[test]
fn refuses_bad_settings_without_sending() {
    let fake = Fake::start(|_| Response::json(200, r#"{"count":0,"results":[]}"#));
    let mut f = form(&fake.url);
    f.access_key_id.clear();
    assert!(jumpserver::test(&f, SECRET).is_err());
    let f = form(&fake.url);
    for secret in ["", "a b", "x\r\ny"] {
        assert!(jumpserver::test(&f, secret).is_err(), "{secret:?}");
    }
    assert!(jumpserver::test(&form("http://jms.example.com"), SECRET).is_err());
    assert!(fake.seen().is_empty());
}

fn apply(
    store: &easyssh_store::Store,
    hosts: Vec<easyssh_store::import::ImportHost>,
) -> easyssh_store::import::ApplyOutcome {
    import_apply(
        store,
        &ApplyRequest {
            hosts,
            group_mode: GroupMode::Source,
            group_id: None,
            duplicates: DuplicatePolicy::Update,
        },
    )
    .unwrap()
}

#[test]
fn assets_behind_one_gateway_stay_separate_hosts_and_sync_again() {
    let (_vault, store) = fixture();
    let all: Vec<Value> = (1..=3).map(asset).collect();
    let fake = Fake::start(move |request| page(request, &all));
    let f = gateway_form(&fake.url);

    // The three hosts share host, port and user: only the source key tells
    // them apart.
    let preview = jumpserver::fetch(&f, SECRET)
        .unwrap()
        .preview(&store)
        .unwrap();
    assert!(preview.hosts.iter().all(|h| h.exists == Some(false)));
    let outcome = apply(&store, jumpserver::fetch(&f, SECRET).unwrap().hosts);
    assert_eq!((outcome.created, outcome.updated), (3, 0), "{outcome:?}");
    assert_eq!(store.list_hosts().unwrap().len(), 3);

    let again = jumpserver::fetch(&f, SECRET)
        .unwrap()
        .preview(&store)
        .unwrap();
    assert!(again.hosts.iter().all(|h| h.exists == Some(true)));
    let outcome = apply(&store, jumpserver::fetch(&f, SECRET).unwrap().hosts);
    assert_eq!(
        (outcome.created, outcome.updated, outcome.unchanged),
        (0, 0, 3)
    );

    // Importing only one asset again does not mistake it for another's host.
    let one = jumpserver::fetch(&f, SECRET).unwrap().hosts.split_off(2);
    let outcome = apply(&store, one);
    assert_eq!(
        (outcome.created, outcome.updated, outcome.unchanged),
        (0, 0, 1)
    );
    assert_eq!(store.list_hosts().unwrap().len(), 3);
}

#[test]
fn a_second_jumpserver_does_not_collide_with_the_first() {
    let (_vault, store) = fixture();
    let all = vec![asset(1)];
    let first = Fake::start({
        let all = all.clone();
        move |request| page(request, &all)
    });
    let second = Fake::start(move |request| page(request, &all));
    // Same asset id, same gateway user, but different JumpServers: different
    // origins, so different source keys (the gateway host differs too).
    apply(
        &store,
        jumpserver::fetch(&gateway_form(&first.url), SECRET)
            .unwrap()
            .hosts,
    );
    let mut f = gateway_form(&second.url);
    f.gateway_host = "koko-2.example.com".into();
    let outcome = apply(&store, jumpserver::fetch(&f, SECRET).unwrap().hosts);
    assert_eq!(outcome.created, 1);
    assert_eq!(store.list_hosts().unwrap().len(), 2);
}
