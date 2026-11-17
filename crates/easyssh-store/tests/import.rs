//! Host import: preview marks what is stored, apply merges without removing.
//! Uses the synthetic fixtures of `easyssh-import`; nothing here reads the
//! real `~/.ssh`.

use easyssh_core::{Connection, ConnectionTarget, Group};
use easyssh_store::import::{
    import_apply, import_preview, read_import_file, ApplyRequest, DuplicatePolicy, GroupMode,
    ImportHost, ImportPreview, PreviewRequest, MAX_IMPORT_BYTES,
};
use easyssh_store::{NotConfigured, Store};
use std::path::Path;
use std::sync::Arc;

const CSV: &[u8] = include_bytes!("../../easyssh-import/tests/fixtures/hosts.csv");
const OPENSSH_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../easyssh-import/tests/fixtures/openssh"
);

fn store() -> Store {
    Store::open_in_memory("dev-a", Arc::new(NotConfigured)).unwrap()
}

fn endpoint(name: &str, hostname: &str, port: u16, user: Option<&str>) -> Connection {
    let mut connection = Connection::alias(name, name);
    connection.target = ConnectionTarget::Endpoint {
        hostname: hostname.to_owned(),
        username: user.map(str::to_owned),
        port,
    };
    connection
}

fn preview_csv(store: &Store) -> ImportPreview {
    import_preview(
        store,
        &PreviewRequest {
            bytes: CSV,
            file_name: "hosts.csv",
            ..PreviewRequest::default()
        },
    )
    .unwrap()
}

fn apply(
    store: &Store,
    hosts: Vec<ImportHost>,
    group_mode: GroupMode,
    group_id: Option<String>,
    duplicates: DuplicatePolicy,
) -> easyssh_store::import::ApplyOutcome {
    import_apply(
        store,
        &ApplyRequest {
            hosts,
            group_mode,
            group_id,
            duplicates,
        },
    )
    .unwrap()
}

fn find<'a>(hosts: &'a [ImportHost], label: &str) -> &'a ImportHost {
    hosts.iter().find(|host| host.label == label).unwrap()
}

fn stored(store: &Store) -> Vec<Connection> {
    store
        .list_hosts()
        .unwrap()
        .into_iter()
        .map(|record| record.item.connection)
        .collect()
}

fn group_named(store: &Store, name: &str) -> Group {
    store
        .list_groups()
        .unwrap()
        .into_iter()
        .map(|record| record.item)
        .find(|group| group.name == name)
        .unwrap_or_else(|| panic!("no group {name}"))
}

#[test]
fn preview_detects_the_format_and_flags_stored_hosts() {
    let store = store();
    // Same hostname (any case), port and user as the CSV's web1.
    store
        .save_host(
            &endpoint("mine", "WEB1.example.com", 2222, Some("alice")),
            None,
        )
        .unwrap();
    // Same host but another user or port is not a duplicate.
    store
        .save_host(&endpoint("other", "bastion", 22, Some("root")), None)
        .unwrap();
    store
        .save_host(&endpoint("ports", "192.0.2.1", 2200, Some("ops")), None)
        .unwrap();
    let preview = preview_csv(&store);
    assert_eq!(preview.format.as_deref(), Some("csv"));
    assert_eq!(preview.detected.as_deref(), Some("csv"));
    assert_eq!(find(&preview.hosts, "web1").exists, Some(true));
    assert_eq!(find(&preview.hosts, "bastion").exists, Some(false));
    assert_eq!(find(&preview.hosts, "v6").exists, Some(false));
    // Rows without a usable host are counted, not listed.
    assert!(preview.skipped >= 2, "skipped {}", preview.skipped);
    assert!(!preview.warnings.is_empty());
    assert!(preview
        .groups
        .contains(&vec!["Prod".to_owned(), "Web".to_owned()]));
    // A preview writes nothing.
    assert_eq!(stored(&store).len(), 3);
}

#[test]
fn a_format_override_wins_and_unknown_ids_are_refused() {
    let store = store();
    let request = |format| PreviewRequest {
        bytes: CSV,
        file_name: "hosts.csv",
        format: Some(format),
        base_dir: None,
    };
    let preview = import_preview(&store, &request("termius")).unwrap();
    assert_eq!(preview.format.as_deref(), Some("termius"));
    assert_eq!(preview.detected.as_deref(), Some("csv"));
    let error = import_preview(&store, &request("nonsense")).unwrap_err();
    assert!(error.to_string().contains("unknown format"), "{error}");
}

#[test]
fn an_unrecognised_file_is_an_empty_preview_with_a_warning() {
    let store = store();
    let preview = import_preview(
        &store,
        &PreviewRequest {
            bytes: b"\x00\x01 not a host list",
            file_name: "blob.bin",
            ..PreviewRequest::default()
        },
    )
    .unwrap();
    assert!(preview.format.is_none() && preview.hosts.is_empty());
    assert!(preview.warnings[0].contains("unrecognised"));
}

#[test]
fn an_oversized_file_is_refused_with_a_clear_error() {
    let store = store();
    let big = vec![b'a'; MAX_IMPORT_BYTES + 1];
    let error = import_preview(
        &store,
        &PreviewRequest {
            bytes: &big,
            file_name: "big.csv",
            ..PreviewRequest::default()
        },
    )
    .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("over the limit of 8 MiB"), "{text}");

    // On disk the size is checked before the file is read.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.csv");
    std::fs::write(&path, &big).unwrap();
    let error = read_import_file(&path).unwrap_err();
    assert!(error.to_string().contains("over the limit"), "{error}");
    // The limit itself is fine.
    std::fs::write(&path, &big[1..]).unwrap();
    assert_eq!(read_import_file(&path).unwrap().len(), MAX_IMPORT_BYTES);
    assert!(read_import_file(&dir.path().join("missing")).is_err());
}

#[test]
fn duplicate_policies_skip_update_and_keep_both() {
    let store = store();
    let mut mine = endpoint("mine", "web1.example.com", 2222, Some("alice"));
    mine.favorite = true;
    mine.color = Some("chart-3".to_owned());
    mine.notes = "kept unless the import has notes".to_owned();
    let id = mine.id.clone();
    store.save_host(&mine, None).unwrap();
    let hosts: Vec<ImportHost> = preview_csv(&store)
        .hosts
        .into_iter()
        .filter(|host| host.label == "web1" || host.label == "bastion")
        .collect();
    assert_eq!(hosts.len(), 2);

    // skip: the stored host is untouched, the other one is created.
    let outcome = apply(
        &store,
        hosts.clone(),
        GroupMode::None,
        None,
        DuplicatePolicy::Skip,
    );
    assert_eq!(
        (outcome.created, outcome.updated, outcome.unchanged),
        (1, 0, 1)
    );
    let all = stored(&store);
    assert_eq!(all.len(), 2);
    assert_eq!(all.iter().find(|c| c.id == id).unwrap().name, "mine");

    // update: same id, new name; favorite and colour stay.
    let outcome = apply(
        &store,
        hosts.clone(),
        GroupMode::None,
        None,
        DuplicatePolicy::Update,
    );
    assert_eq!(
        (outcome.created, outcome.updated, outcome.unchanged),
        (0, 1, 1)
    );
    let all = stored(&store);
    assert_eq!(all.len(), 2);
    let updated = all.iter().find(|c| c.id == id).unwrap();
    assert_eq!(updated.name, "web1");
    assert!(updated.favorite);
    assert_eq!(updated.color.as_deref(), Some("chart-3"));
    assert!(updated.notes.contains("Identity file"));

    // The same update again changes nothing.
    let outcome = apply(
        &store,
        hosts.clone(),
        GroupMode::None,
        None,
        DuplicatePolicy::Update,
    );
    assert_eq!(
        (outcome.created, outcome.updated, outcome.unchanged),
        (0, 0, 2)
    );
    assert!(!outcome.changed());

    // keep-both: a second copy of each.
    let outcome = apply(
        &store,
        hosts,
        GroupMode::None,
        None,
        DuplicatePolicy::KeepBoth,
    );
    assert_eq!((outcome.created, outcome.updated), (2, 0));
    assert_eq!(stored(&store).len(), 4);
}

#[test]
fn repeated_hosts_in_one_import_do_not_duplicate_unless_asked() {
    let store = store();
    let host = ImportHost {
        label: "twice".into(),
        hostname: "192.0.2.50".into(),
        port: 22,
        ..ImportHost::default()
    };
    let outcome = apply(
        &store,
        vec![host.clone(), host.clone()],
        GroupMode::None,
        None,
        DuplicatePolicy::Skip,
    );
    assert_eq!((outcome.created, outcome.unchanged), (1, 1));
    let outcome = apply(
        &store,
        vec![host.clone(), host],
        GroupMode::None,
        None,
        DuplicatePolicy::KeepBoth,
    );
    assert_eq!(outcome.created, 2);
    assert_eq!(stored(&store).len(), 3);
}

#[test]
fn apply_never_removes_anything() {
    let store = store();
    let keep = endpoint("keep", "keep.example.com", 22, None);
    let group = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name: "Mine".into(),
        color: None,
        parent_id: None,
    };
    let mut grouped = endpoint("grouped", "grouped.example.com", 22, Some("me"));
    grouped.group_id = Some(group.id.clone());
    store.save_host(&keep, None).unwrap();
    store.save_host(&grouped, Some(&group)).unwrap();
    let snippet = easyssh_core::CommandSnippet {
        id: uuid::Uuid::new_v4().to_string(),
        name: "ls".into(),
        content: "ls -l".into(),
    };
    store.save_snippet(&snippet).unwrap();

    for policy in [
        DuplicatePolicy::Skip,
        DuplicatePolicy::Update,
        DuplicatePolicy::KeepBoth,
    ] {
        for mode in [GroupMode::Source, GroupMode::None] {
            let hosts = preview_csv(&store).hosts;
            apply(&store, hosts, mode, None, policy);
        }
    }
    let ids: Vec<String> = stored(&store).into_iter().map(|c| c.id).collect();
    assert!(ids.contains(&keep.id) && ids.contains(&grouped.id));
    let kept = stored(&store)
        .into_iter()
        .find(|c| c.id == grouped.id)
        .unwrap();
    assert_eq!(kept.group_id.as_deref(), Some(group.id.as_str()));
    assert!(store
        .list_groups()
        .unwrap()
        .iter()
        .any(|g| g.item.id == group.id));
    assert_eq!(store.list_snippets().unwrap().len(), 1);
}

#[test]
fn groups_follow_the_chosen_mode() {
    let store = store();
    // A stored "prod" (other case) is reused for the file's "Prod".
    let existing = Group {
        id: uuid::Uuid::new_v4().to_string(),
        name: "prod".into(),
        color: None,
        parent_id: None,
    };
    store
        .save_host(&endpoint("x", "x.example.com", 22, None), Some(&existing))
        .unwrap();
    let hosts = preview_csv(&store).hosts;

    let outcome = apply(
        &store,
        hosts.clone(),
        GroupMode::Source,
        None,
        DuplicatePolicy::Skip,
    );
    // "Web" under "Prod" and the "Group, with comma" folder are new.
    assert_eq!(outcome.groups_created, 2);
    let web = group_named(&store, "Web");
    assert_eq!(web.parent_id.as_deref(), Some(existing.id.as_str()));
    let hosts_now = stored(&store);
    let web1 = hosts_now.iter().find(|c| c.name == "web1").unwrap();
    assert_eq!(web1.group_id.as_deref(), Some(web.id.as_str()));
    let v6 = hosts_now.iter().find(|c| c.name == "v6").unwrap();
    assert_eq!(v6.group_id, None);

    // Existing: everything goes into one stored group.
    let target = group_named(&store, "Web");
    let again: Vec<ImportHost> = hosts
        .iter()
        .filter(|host| host.label == "v6")
        .cloned()
        .collect();
    let outcome = apply(
        &store,
        again,
        GroupMode::Existing,
        Some(target.id.clone()),
        DuplicatePolicy::Update,
    );
    assert_eq!(outcome.updated, 1);
    let v6 = stored(&store).into_iter().find(|c| c.name == "v6").unwrap();
    assert_eq!(v6.group_id.as_deref(), Some(target.id.as_str()));

    // None leaves an updated host where it is.
    let again: Vec<ImportHost> = hosts.into_iter().filter(|h| h.label == "v6").collect();
    apply(
        &store,
        again,
        GroupMode::None,
        None,
        DuplicatePolicy::Update,
    );
    let v6 = stored(&store).into_iter().find(|c| c.name == "v6").unwrap();
    assert_eq!(v6.group_id.as_deref(), Some(target.id.as_str()));

    // An unknown target is refused and writes nothing.
    let before = stored(&store).len();
    let error = import_apply(
        &store,
        &ApplyRequest {
            hosts: vec![ImportHost {
                label: "n".into(),
                hostname: "n.example.com".into(),
                port: 22,
                ..ImportHost::default()
            }],
            group_mode: GroupMode::Existing,
            group_id: Some("nope".into()),
            duplicates: DuplicatePolicy::Skip,
        },
    )
    .unwrap_err();
    assert!(error.to_string().contains("target group"), "{error}");
    assert_eq!(stored(&store).len(), before);
}

#[test]
fn openssh_include_needs_the_base_dir_and_openssh_only_hosts_are_tagged() {
    let store = store();
    let bytes = std::fs::read(Path::new(OPENSSH_DIR).join("config")).unwrap();
    let preview_with = |base_dir: Option<&Path>| {
        import_preview(
            &store,
            &PreviewRequest {
                bytes: &bytes,
                file_name: "config",
                format: None,
                base_dir,
            },
        )
        .unwrap()
    };
    let without = preview_with(None);
    assert_eq!(without.format.as_deref(), Some("openssh"));
    assert!(without.hosts.iter().all(|h| h.label != "included"));
    assert!(without.warnings.iter().any(|w| w.contains("Include")));

    let with = preview_with(Some(Path::new(OPENSSH_DIR)));
    let included = find(&with.hosts, "included");
    assert_eq!(included.hostname, "included.example.com");
    let legacy = find(&with.hosts, "legacy");
    assert!(!legacy.openssh_only.is_empty());
    // "quoted user" is not a username the app can launch.
    let quoted = find(&with.hosts, "quoted alias");
    assert!(quoted.problem.is_some());
    assert!(find(&with.hosts, "web1").problem.is_none());

    let outcome = apply(
        &store,
        with.hosts.clone(),
        GroupMode::None,
        None,
        DuplicatePolicy::Skip,
    );
    assert_eq!(outcome.rejected.len(), 1);
    assert_eq!(outcome.rejected[0].label, "quoted alias");
    let all = stored(&store);
    let legacy = all.iter().find(|c| c.name == "legacy").unwrap();
    assert!(legacy.tags.iter().any(|tag| tag == "openssh-only"));
    // `db` jumps through two hops, which `-J <alias>` cannot express: kept
    // in the notes and reported.
    let db = all.iter().find(|c| c.name == "db").unwrap();
    assert_eq!(db.proxy_jump, None);
    assert!(db.notes.contains("admin@jump2.example.com:2200"));
    assert!(outcome.warnings.iter().any(|w| w.starts_with("db:")));
    // A plain alias jump stays.
    assert!(all
        .iter()
        .any(|c| c.name == "included" && c.proxy_jump.is_none()));
}

#[test]
fn a_hostile_host_in_an_apply_request_is_rejected() {
    let store = store();
    let hosts = ["-oProxyCommand=x", "a b", "a;b"]
        .into_iter()
        .map(|hostname| ImportHost {
            label: hostname.into(),
            hostname: hostname.into(),
            port: 22,
            ..ImportHost::default()
        })
        .collect();
    let outcome = apply(&store, hosts, GroupMode::None, None, DuplicatePolicy::Skip);
    assert_eq!((outcome.created, outcome.rejected.len()), (0, 3));
    assert!(stored(&store).is_empty());
}
