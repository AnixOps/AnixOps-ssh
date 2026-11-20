//! Per-format tests on synthetic fixtures. Nothing here touches the real
//! user's files.

use easyssh_import::*;
use std::io::Write;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn ssh_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openssh")
}

fn host<'a>(report: &'a ImportReport, label: &str) -> &'a ImportedHost {
    report
        .hosts
        .iter()
        .find(|h| h.label == label)
        .unwrap_or_else(|| panic!("no host {label:?} in {:?}", report.hosts))
}

fn has_warning(report: &ImportReport, needle: &str) -> bool {
    report.warnings.iter().any(|w| w.message.contains(needle))
}

fn path(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in files {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn utf16le(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend(unit.to_le_bytes());
    }
    out
}

// ---- OpenSSH -------------------------------------------------------------

#[test]
fn openssh_hosts_defaults_and_first_match_wins() {
    let bytes = fixture("openssh/config");
    assert!(openssh::detect(&bytes));
    let report = openssh::parse_with_base(&bytes, &ssh_dir());

    let labels: Vec<&str> = report.hosts.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "included",
            "web1",
            "web2",
            "db",
            "quoted alias",
            "v6",
            "legacy",
            "dev-1",
            "bastion"
        ],
        "wildcard patterns are never hosts"
    );

    let web1 = host(&report, "web1");
    assert_eq!(web1.hostname, "web.example.com");
    assert_eq!(web1.port, 2222);
    assert_eq!(web1.username.as_deref(), Some("fallback"), "from Host *");
    assert_eq!(web1.identity_file, Some(PathBuf::from("~/.ssh/id_web")));
    assert!(web1.notes.contains("~/.ssh/id_backup"));
    assert!(web1.notes.contains("~/.ssh/id_default"));
    assert_eq!(host(&report, "web2").hostname, "web.example.com");

    let db = host(&report, "db");
    assert_eq!(db.hostname, "192.0.2.20", "key=value form");
    assert_eq!(
        db.username.as_deref(),
        Some("dbadmin"),
        "keys are case-insensitive"
    );
    assert_eq!(db.port, 22);
    assert_eq!(db.jump, ["bastion", "admin@jump2.example.com:2200"]);

    let quoted = host(&report, "quoted alias");
    assert_eq!(quoted.hostname, "quoted.example.com");
    assert_eq!(quoted.username.as_deref(), Some("quoted user"));

    assert_eq!(host(&report, "v6").hostname, "2001:db8::10");

    // `dev-*` supplies user and port to dev-1, which wins over `Host *`.
    let dev = host(&report, "dev-1");
    assert_eq!(dev.hostname, "dev1.example.com");
    assert_eq!(dev.username.as_deref(), Some("devuser"));
    assert_eq!(dev.port, 2200);

    // Include with a glob, and an include nested inside a Host block.
    let included = host(&report, "included");
    assert_eq!(included.hostname, "included.example.com");
    assert_eq!(included.username.as_deref(), Some("incuser"));
    assert_eq!(included.port, 2022);

    assert!(report.groups.is_empty());
}

#[test]
fn openssh_marks_openssh_only_options() {
    let report = openssh::parse_with_base(&fixture("openssh/config"), &ssh_dir());
    let legacy = host(&report, "legacy");
    for reason in [
        "ProxyCommand",
        "LocalCommand",
        "PermitLocalCommand",
        "KexAlgorithms",
    ] {
        assert!(
            legacy.openssh_only.iter().any(|r| r == reason),
            "{reason} missing in {:?}",
            legacy.openssh_only
        );
    }
    assert!(legacy.notes.contains("LocalForward 8080 localhost:80"));
    assert!(host(&report, "web1").openssh_only.is_empty());
}

#[test]
fn openssh_match_blocks_mark_every_host() {
    let report = openssh::parse(&fixture("openssh/config_match"));
    let alpha = host(&report, "alpha");
    assert_eq!(alpha.openssh_only, ["Match"]);
    assert_eq!(alpha.username, None, "options inside Match are not applied");
    assert_eq!(alpha.port, 22);
    assert!(has_warning(&report, "Match blocks are not evaluated"));
}

#[test]
fn openssh_include_without_base_and_depth_cap() {
    let bytes = fixture("openssh/config");
    let report = openssh::parse(&bytes);
    assert!(has_warning(&report, "no base directory"));
    assert!(report.hosts.iter().all(|h| h.label != "included"));

    let report = openssh::parse_with_base(&fixture("openssh/config_loop"), &ssh_dir());
    assert!(has_warning(&report, "nesting deeper than 8"));
    assert_eq!(
        report.hosts.iter().filter(|h| h.label == "looped").count(),
        1
    );
}

#[test]
fn openssh_malformed_input_warns() {
    let report = openssh::parse_with_base(&fixture("openssh/config_bad"), &ssh_dir());
    assert!(has_warning(&report, "invalid Port"));
    assert!(has_warning(&report, "unterminated quote"));
    assert!(has_warning(&report, "has no value"));
    assert!(has_warning(&report, "Include"));
    assert_eq!(report.skipped, 1, "option-like hostname is rejected");
    assert_eq!(host(&report, "ok").port, 22);
    assert!(report.hosts.iter().all(|h| !h.hostname.starts_with('-')));
}

#[cfg(unix)]
#[test]
fn openssh_include_cannot_escape_base_dir() {
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(
        outside.path().join("evil.conf"),
        "Host evil\n HostName evil.example.com\n",
    )
    .unwrap();
    let base = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), base.path().join("link")).unwrap();
    let config =
        b"Include link/evil.conf\nInclude ../evil.conf\nHost fine\n HostName fine.example.com\n";
    let report = openssh::parse_with_base(config, base.path());
    assert_eq!(report.hosts.len(), 1);
    assert_eq!(report.hosts[0].label, "fine");
    assert!(has_warning(&report, "outside") || has_warning(&report, "not found"));
}

#[test]
fn openssh_glob_matching() {
    let report = openssh::parse(
        b"Host *.example.com\n User wild\nHost app.example.com\n HostName 192.0.2.7\nHost !app.example.com *\n Port 99\n",
    );
    let app = host(&report, "app.example.com");
    assert_eq!(app.username.as_deref(), Some("wild"));
    assert_eq!(app.port, 22, "negated pattern excludes the Host * block");
}

// ---- PuTTY ---------------------------------------------------------------

#[test]
fn putty_reg() {
    let bytes = fixture("putty.reg");
    assert!(putty::detect(&bytes));
    let report = putty::parse(&bytes);

    let web = host(&report, "Web Server");
    assert_eq!(web.hostname, "web.example.com");
    assert_eq!(
        web.username.as_deref(),
        Some("alice"),
        "user@ in HostName wins"
    );
    assert_eq!(web.port, 22);
    assert_eq!(
        web.identity_file,
        Some(PathBuf::from("C:\\Users\\example\\keys\\web.ppk"))
    );

    let db = host(&report, "Database #1");
    assert_eq!(db.group_path, path(&["Prod"]));
    assert_eq!(db.port, 2222);
    assert_eq!(db.openssh_only, ["ProxyCommand"]);
    assert!(has_warning(&report, "proxy password was not imported"));

    assert_eq!(host(&report, "Bad Port").port, 22);
    assert!(has_warning(&report, "invalid PortNumber"));
    assert!(report.hosts.iter().all(|h| h.label != "Default Settings"));
    assert_eq!(report.skipped, 2, "serial and no-host sessions");
    assert_eq!(report.groups, vec![path(&["Prod"])]);
}

#[test]
fn putty_reg_utf16() {
    let text = String::from_utf8(fixture("putty.reg")).unwrap();
    let report = putty::parse(&utf16le(&text));
    assert_eq!(report.hosts.len(), 3);
    assert!(putty::detect(&utf16le(&text)));
}

#[test]
fn putty_without_sessions_warns() {
    let report = putty::parse(b"Windows Registry Editor Version 5.00\r\n\r\n[HKEY_CURRENT_USER\\Software\\SimonTatham\\PuTTY]\r\n");
    assert!(report.hosts.is_empty());
    assert!(has_warning(&report, "no PuTTY sessions"));
}

// ---- SecureCRT -----------------------------------------------------------

#[test]
fn securecrt_xml() {
    let bytes = fixture("securecrt.xml");
    assert!(securecrt::detect(&bytes));
    let report = securecrt::parse(&bytes);

    let web = host(&report, "web1");
    assert_eq!(web.hostname, "web1.example.com");
    assert_eq!(web.port, 2222, "dword is hexadecimal");
    assert_eq!(web.username.as_deref(), Some("alice"));
    assert_eq!(web.group_path, path(&["Production", "Web"]));
    assert_eq!(web.identity_file, Some(PathBuf::from("C:\\keys\\web1.pem")));
    assert_eq!(web.jump, ["Production/bastion"]);
    assert!(has_warning(&report, "saved password was not imported"));

    let bastion = host(&report, "bastion");
    assert_eq!(bastion.group_path, path(&["Production"]));
    assert_eq!(bastion.port, 22);

    assert_eq!(host(&report, "Tom & Jerry").hostname, "2001:db8::7");
    assert!(report.hosts.iter().all(|h| h.label != "telnet-box"));
    assert!(report.hosts.iter().all(|h| h.label != "Default"));
    assert_eq!(report.skipped, 1);
    assert!(report.groups.contains(&path(&["Production", "Web"])));
}

#[test]
fn securecrt_malformed_xml_warns() {
    let report = securecrt::parse(
        b"<VanDyke><key name=\"Sessions\"><key name=\"a\"><string name=\"Hostname\">x</str",
    );
    assert!(!report.warnings.is_empty());
    let report = securecrt::parse(b"not xml at all");
    assert!(report.hosts.is_empty());
    assert!(!report.warnings.is_empty());
}

// ---- MobaXterm -----------------------------------------------------------

#[test]
fn mobaxterm_sessions() {
    let bytes = fixture("mobaxterm.mxtsessions");
    assert!(mobaxterm::detect(&bytes));
    let report = mobaxterm::parse(&bytes);

    let top = host(&report, "Top Level");
    assert_eq!(top.hostname, "top.example.com");
    assert_eq!(top.port, 22);
    assert_eq!(top.username.as_deref(), Some("alice"));
    assert!(top.group_path.is_empty());
    assert_eq!(top.identity_file, None);

    let web = host(&report, "web");
    assert_eq!(web.hostname, "web.example.com");
    assert_eq!(web.port, 2222);
    assert_eq!(web.username.as_deref(), Some("bob"));
    assert_eq!(web.group_path, path(&["Servers", "Prod"]));
    assert_eq!(
        web.identity_file,
        Some(PathBuf::from("_ProfileDir_\\.ssh\\web_key"))
    );

    assert_eq!(host(&report, "v6").hostname, "2001:db8::9");
    assert_eq!(host(&report, "v6").port, 22);
    assert!(report
        .hosts
        .iter()
        .all(|h| h.label != "rdp-box" && h.label != "vnc"));
    assert_eq!(report.skipped, 3, "rdp, vnc and the broken entry");
    assert!(has_warning(&report, "stored credentials was not imported"));
    assert!(report.groups.contains(&path(&["Servers"])));
}

// ---- Xshell --------------------------------------------------------------

#[test]
fn xshell_single_file_and_utf16() {
    let bytes = fixture("xshell.xsh");
    assert!(xshell::detect(&bytes));
    let report = xshell::parse_named("Sessions/xs.xsh", &bytes);
    let xs = host(&report, "xs");
    assert_eq!(xs.hostname, "xs.example.com");
    assert_eq!(xs.port, 2022);
    assert_eq!(xs.username.as_deref(), Some("xuser"));
    assert!(xs.notes.contains("my-xshell-key"));
    assert!(has_warning(&report, "Password was not imported"));
    assert!(has_warning(&report, "Passphrase was not imported"));

    let text = String::from_utf8(bytes).unwrap();
    let report = xshell::parse(&utf16le(&text));
    assert_eq!(report.hosts[0].hostname, "xs.example.com");
    assert_eq!(report.hosts[0].label, "xs.example.com", "no name known");
}

#[test]
fn xshell_zip() {
    let xsh = fixture("xshell.xsh");
    let other = b"[CONNECTION]\r\nProtocol=SSH\r\nHost=192.0.2.8\r\n";
    let telnet = b"[CONNECTION]\r\nProtocol=TELNET\r\nHost=192.0.2.9\r\n";
    let zip = zip_of(&[
        ("Prod/web/xs.xsh", &xsh),
        ("top.xsh", other),
        ("old.xsh", telnet),
        ("readme.txt", b"ignored"),
    ]);
    assert!(xshell::detect(&zip));
    let report = xshell::parse(&zip);
    assert_eq!(report.hosts.len(), 2);
    assert_eq!(host(&report, "xs").group_path, path(&["Prod", "web"]));
    assert!(host(&report, "top").group_path.is_empty());
    assert_eq!(report.skipped, 1);
    assert!(report.groups.contains(&path(&["Prod"])));
}

#[test]
fn xshell_bad_inputs_warn() {
    let report = xshell::parse(b"PK\x03\x04 truncated");
    assert!(report.hosts.is_empty());
    assert!(!report.warnings.is_empty());
    let report = xshell::parse(b"just text");
    assert!(report.hosts.is_empty());
    assert_eq!(report.skipped, 1);
}

// ---- FinalShell ----------------------------------------------------------

#[test]
fn finalshell_json() {
    let bytes = fixture("finalshell.json");
    assert!(finalshell::detect(&bytes));
    let report = finalshell::parse(&bytes);

    let bastion = host(&report, "bastion");
    assert_eq!(bastion.group_path, path(&["Production"]));
    assert_eq!(bastion.username.as_deref(), Some("ops"));
    assert!(has_warning(&report, "password was not imported"));

    let web = host(&report, "web1");
    assert_eq!(web.group_path, path(&["Production", "Web"]));
    assert_eq!(web.port, 2222, "string port");
    assert_eq!(web.jump, ["bastion"]);
    assert!(web.notes.contains("front end"));
    assert!(web.notes.contains("key-123"));

    let v6 = host(&report, "v6");
    assert_eq!(v6.port, 22);
    assert!(has_warning(&report, "invalid port"));
    assert!(has_warning(&report, "jump server \"missing\" not found"));

    // A folder cycle terminates and still yields the host.
    assert!(host(&report, "looped").group_path.len() <= 2);
}

#[test]
fn finalshell_single_object_and_zip() {
    let one = br#"{"name":"solo","host":"192.0.2.4","port":22,"user_name":"u"}"#;
    assert_eq!(finalshell::parse(one).hosts.len(), 1);
    let zip = zip_of(&[("conn/a.json", one), ("conn/b.json", b"{broken")]);
    assert!(finalshell::detect(&zip));
    let report = finalshell::parse(&zip);
    assert_eq!(report.hosts.len(), 1);
    assert_eq!(report.skipped, 1);
    assert!(has_warning(&report, "invalid JSON"));
}

// ---- Termius -------------------------------------------------------------

#[test]
fn termius_csv() {
    let bytes = fixture("termius.csv");
    assert!(termius::detect(&bytes));
    assert_eq!(detect_format(&bytes), Some(Format::Termius));
    let report = termius::parse(&bytes);

    let web = host(&report, "web1");
    assert_eq!(web.group_path, path(&["Prod", "Web"]));
    assert_eq!(web.port, 2222);
    assert_eq!(web.notes, "Tags: blue,eu");
    assert!(has_warning(&report, "`password` is not imported"));
    assert!(has_warning(&report, "`ssh key` is not imported"));
    assert_eq!(host(&report, "db").port, 22);
    assert_eq!(host(&report, "v6").port, 22);
    assert!(has_warning(&report, "invalid port"));
    assert_eq!(report.skipped, 1, "telnet row");
}

#[test]
fn termius_json() {
    let bytes = fixture("termius.json");
    assert!(termius::detect(&bytes));
    let report = termius::parse(&bytes);
    let web = host(&report, "web1");
    assert_eq!(web.group_path, path(&["Prod", "Web"]));
    assert_eq!(web.port, 2222);
    assert_eq!(web.username.as_deref(), Some("alice"));
    assert_eq!(web.jump, ["bastion"]);
    let bastion = host(&report, "bastion");
    assert_eq!(bastion.username.as_deref(), Some("ops"));
    assert!(has_warning(&report, "password was not imported"));
    assert_eq!(host(&report, "bare").port, 2200);
}

// ---- Ansible -------------------------------------------------------------

#[test]
fn ansible_yaml_nesting_vars_and_mappings() {
    let report = ansible::parse(&fixture("ansible/inventory.yml"));
    let web = host(&report, "web02.example.com");
    assert_eq!(web.group_path, path(&["prod", "web"]));
    assert_eq!(web.hostname, "web02.example.com");
    // all < prod < web for the port; all < prod for the user.
    assert_eq!(web.port, 2222);
    assert_eq!(web.username.as_deref(), Some("deploy"));
    assert_eq!(web.jump, vec!["jump@bastion.example.com:2222"]);
    for n in ["web01.example.com", "web03.example.com"] {
        host(&report, n);
    }

    // Host variables beat group variables; the key is a path, not material.
    let odd = host(&report, "web-odd");
    assert_eq!(odd.hostname, "192.0.2.50");
    assert_eq!(odd.username.as_deref(), Some("alice"));
    assert_eq!(
        odd.identity_file.as_deref(),
        Some(Path::new("~/.ssh/web_ed25519"))
    );
    assert!(has_warning(&report, "ansible_password was not imported"));

    // Two groups: deepest wins, the other becomes a tag.
    let db = host(&report, "db1");
    assert_eq!(db.group_path, path(&["prod", "db"]));
    assert_eq!(db.notes, "Tags: monitoring");
    assert_eq!(db.port, 2200);
    assert_eq!(
        db.jump,
        vec!["jump@bastion.example.com:2222", "hop1", "hop2"]
    );
    assert_eq!(report.hosts.iter().filter(|h| h.label == "db1").count(), 1);

    // Templated values are dropped with a warning.
    let mon = host(&report, "mon1");
    assert_eq!(mon.hostname, "mon1");
    assert_eq!(mon.group_path, path(&["monitoring"]));
    assert!(has_warning(&report, "is templated"));

    // ungrouped hosts have no group; ProxyCommand is not imported.
    let loner = host(&report, "loner");
    assert!(loner.group_path.is_empty());
    assert!(loner.jump.is_empty());
    assert_eq!(loner.username.as_deref(), Some("ops"));
    assert!(has_warning(&report, "ProxyCommand"));

    // local, winrm and docker connections are skipped.
    for name in ["localbox", "win1", "ctr"] {
        assert!(!report.hosts.iter().any(|h| h.label == name), "{name}");
    }
    assert_eq!(report.skipped, 3);
    assert!(has_warning(&report, "ansible_connection `local`"));
    assert!(!report.groups.contains(&path(&["all"])));
    assert!(report.groups.contains(&path(&["prod", "web"])));
}

#[test]
fn ansible_ini_sections_ranges_and_vars() {
    let report = ansible::parse(&fixture("ansible/inventory.ini"));
    let standalone = host(&report, "standalone");
    assert_eq!(standalone.hostname, "192.0.2.9");
    assert!(standalone.group_path.is_empty());
    assert_eq!(standalone.username.as_deref(), Some("ops"));
    assert_eq!(standalone.port, 2200);

    let web = host(&report, "web03.example.com");
    assert_eq!(web.group_path, path(&["prod", "web"]));
    assert_eq!(web.username.as_deref(), Some("deploy"));
    assert_eq!(web.jump, vec!["jump@bastion.example.com:2222"]);
    assert_eq!(host(&report, "web-odd").username.as_deref(), Some("alice"));
    assert!(has_warning(&report, "ansible_password was not imported"));

    // [db:vars] is deeper than [prod:vars]; the inline port beats [all:vars].
    let db = host(&report, "db1");
    assert_eq!(db.username.as_deref(), Some("dbadmin"));
    assert_eq!(db.port, 2022);
    assert_eq!(db.group_path, path(&["prod", "db"]));
    assert_eq!(db.notes, "Tags: monitoring");
    assert_eq!(
        db.jump,
        vec!["jump@bastion.example.com:2222", "hop1", "hop2"]
    );

    // Alphabetic ranges, templated values.
    for n in ["abc-a", "abc-b", "abc-c"] {
        let h = host(&report, n);
        assert_eq!(h.port, 2201);
        assert_eq!(h.group_path, path(&["monitoring"]));
    }
    assert_eq!(host(&report, "mon1").hostname, "mon1");
    assert!(has_warning(&report, "is templated"));

    // The cap: 3000 requested, 1024 kept, with a warning.
    let big = report
        .hosts
        .iter()
        .filter(|h| h.label.starts_with("big"))
        .count();
    assert_eq!(big, ansible::MAX_RANGE_HOSTS);
    assert!(has_warning(&report, "range expands to more than"));

    assert!(!report.hosts.iter().any(|h| h.label == "localbox"));
    assert_eq!(report.skipped, 1);
}

#[test]
fn ansible_range_forms() {
    let ini = b"[g]\nn[01:03]\nm[8:10]\nx[a:c]\ny[1:7:3]\n[2001:db8::1]\nh[3:1]\n";
    let report = ansible::parse(ini);
    let labels: Vec<&str> = report.hosts.iter().map(|h| h.label.as_str()).collect();
    assert_eq!(
        labels,
        ["n01", "n02", "n03", "m8", "m9", "m10", "xa", "xb", "xc", "y1", "y4", "y7"]
    );
    assert!(has_warning(&report, "runs backwards"));
}

#[test]
fn ansible_secrets_and_odd_args() {
    let ini = b"[g]\nh1 ansible_host=192.0.2.1 ansible_ssh_pass=hunter2 ansible_become_pass=x ansible_ssh_common_args='-oProxyJump=a -J b,c -J -oProxyCommand=evil'\n";
    let report = ansible::parse(ini);
    let h = host(&report, "h1");
    assert_eq!(h.jump, vec!["a", "b", "c"]);
    assert!(has_warning(&report, "ansible_ssh_pass was not imported"));
    assert!(has_warning(&report, "ansible_become_pass was not imported"));
    assert!(has_warning(&report, "jump host"));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains("hunter2"));
}

#[test]
fn ansible_yaml_aliases_and_vault() {
    let yaml = b"all:\n  hosts:\n    a:\n      ansible_host: 192.0.2.1\n      ansible_user: !vault |\n        $ANSIBLE_VAULT;1.1;AES256\n        6162\n    b: &b\n      ansible_host: 192.0.2.2\n    c: *b\n";
    let report = ansible::parse(yaml);
    assert_eq!(host(&report, "a").username, None);
    assert!(has_warning(&report, "encrypted"));
    assert_eq!(host(&report, "c").hostname, "192.0.2.2");
}

#[test]
fn ansible_malformed_input_warns_without_panicking() {
    let bomb = {
        let mut s = String::from("all:\n  vars:\n    l0: &l0 [x, x, x, x, x, x, x, x, x]\n");
        for i in 1..12 {
            let prev = i - 1;
            s.push_str(&format!(
                "    l{i}: &l{i} [*l{prev}, *l{prev}, *l{prev}, *l{prev}, *l{prev}, *l{prev}, *l{prev}, *l{prev}, *l{prev}]\n"
            ));
        }
        s.into_bytes()
    };
    let deep = format!(
        "all:\n  children:\n{}",
        "    a:\n      children:\n".repeat(300)
    );
    let inputs: Vec<&[u8]> = vec![
        b"",
        b"all: [1, 2",
        b"all: 5",
        b"- a\n- b\n",
        b"all:\n  hosts: 7\n  children: [x]\n  vars: nope\n  other: 1\n",
        b"all:\n  children:\n    a:\n      children:\n        a:\n",
        b"[a:children]\na\n[a]\nh\n",
        b"[:children]\n[a:wat]\nh\n[]\n=\nk=v\n",
        b"[a]\nh key='unterminated\nh2 =x\nh3 ansible_port=99999 ansible_host=\n",
        b"[a]\n[[[:]]]\nh[1:]\nh[:2]\nh[0:99999999999]\nh[1:2:0]\n[a:vars]\nnoequals\n",
        b"[a]\nh1 ansible_host={{ x }} ansible_user={{ u }} ansible_port={{ p }}\n{{ name }}\n",
        b"\xff\xfe\x00\x00[a]\n",
        &bomb,
        deep.as_bytes(),
    ];
    for input in inputs {
        let report = ansible::parse(input);
        for h in &report.hosts {
            assert!(h.port != 0 && !h.hostname.is_empty() && !h.hostname.starts_with('-'));
        }
        let _ = detect_format(input);
        let _ = import_auto(input);
    }
    let report = ansible::parse(b"all: [1, 2");
    assert!(has_warning(&report, "invalid YAML"));
    assert!(report.hosts.is_empty());
    let report = ansible::parse(b"[a]\nh1 ansible_host={{ x }} ansible_port={{ p }}\n");
    assert_eq!(host(&report, "h1").hostname, "h1");
    assert_eq!(host(&report, "h1").port, 22);
}

#[test]
fn ansible_detection_does_not_steal() {
    // Other formats' fixtures keep their formats (see auto_detection_...).
    assert!(!ansible::detect(&fixture("openssh/config")));
    assert!(!ansible::detect(&fixture("termius.csv")));
    assert!(!ansible::detect(&fixture("hosts.csv")));
    assert!(!ansible::detect(&fixture("putty.reg")));
    // A plain YAML file, an INI file without Ansible markers.
    assert!(!ansible::detect(b"name: x\nitems:\n  - a\n"));
    assert!(!ansible::detect(b"[section]\nkey=value\n"));
    assert!(!ansible::detect(b"Host web\n  HostName 192.0.2.1\n"));
    // Ansible shapes.
    assert!(ansible::detect(b"all:\n  hosts:\n    h:\n"));
    assert!(ansible::detect(b"web:\n  hosts:\n    h1:\n"));
    assert!(ansible::detect(b"[web:children]\nx\n"));
    assert!(ansible::detect(b"h1 ansible_host=192.0.2.1\n"));
}

// ---- CSV -----------------------------------------------------------------

#[test]
fn csv_import() {
    let bytes = fixture("hosts.csv");
    assert!(csv_format::detect(&bytes));
    assert_eq!(detect_format(&bytes), Some(Format::Csv));
    let report = csv_format::parse(&bytes);

    let web = host(&report, "web1");
    assert_eq!(web.group_path, path(&["Prod", "Web"]));
    assert_eq!(web.identity_file, Some(PathBuf::from("~/.ssh/web1")));
    assert_eq!(web.jump, ["bastion"]);
    assert_eq!(host(&report, "v6").jump, ["bastion", "web1"]);
    assert_eq!(
        host(&report, "192.0.2.99").hostname,
        "192.0.2.99",
        "label defaults"
    );
    let quoted = host(&report, "quoted, label");
    assert_eq!(quoted.group_path, path(&["Group, with comma"]));
    assert_eq!(host(&report, "badport").port, 22);
    assert!(has_warning(&report, "invalid port"));
    assert_eq!(
        report.skipped, 2,
        "missing hostname and option-like hostname"
    );
    assert_eq!(
        report.groups,
        vec![
            path(&["Group, with comma"]),
            path(&["Prod"]),
            path(&["Prod", "Web"])
        ]
    );
}

#[test]
fn csv_requires_header_and_ignores_secret_columns() {
    let report = csv_format::parse(b"foo,bar\n1,2\n");
    assert!(report.hosts.is_empty());
    assert!(has_warning(&report, "`hostname` column is required"));

    let report = csv_format::parse(b"label,hostname,password\nx,192.0.2.1,hunter2\n");
    assert_eq!(report.hosts.len(), 1);
    assert!(has_warning(&report, "`password` is not imported"));
    assert!(!format!("{report:?}").contains("hunter2"));
}

// ---- auto detection, secrets, adapter ------------------------------------

#[test]
fn auto_detection_picks_each_format() {
    let cases: Vec<(&str, Format)> = vec![
        ("putty.reg", Format::Putty),
        ("securecrt.xml", Format::SecureCrt),
        ("mobaxterm.mxtsessions", Format::MobaXterm),
        ("xshell.xsh", Format::Xshell),
        ("finalshell.json", Format::FinalShell),
        ("termius.csv", Format::Termius),
        ("termius.json", Format::Termius),
        ("ansible/inventory.yml", Format::Ansible),
        ("ansible/inventory.ini", Format::Ansible),
        ("hosts.csv", Format::Csv),
        ("openssh/config", Format::OpenSsh),
    ];
    for (name, format) in cases {
        let bytes = fixture(name);
        assert_eq!(detect_format(&bytes), Some(format), "{name}");
        assert!(!import_auto(&bytes).hosts.is_empty(), "{name}");
    }
    assert_eq!(detect_format(b"\x00\x01\x02"), None);
    let report = import_auto(b"???");
    assert!(report.hosts.is_empty());
    assert!(has_warning(&report, "unrecognised"));

    let report = import_auto_in(&fixture("openssh/config"), Some(&ssh_dir()));
    assert!(report.hosts.iter().any(|h| h.label == "included"));
}

#[test]
fn secrets_never_reach_the_report() {
    for name in [
        "putty.reg",
        "securecrt.xml",
        "mobaxterm.mxtsessions",
        "xshell.xsh",
        "finalshell.json",
        "termius.csv",
        "termius.json",
    ] {
        let bytes = fixture(name);
        assert!(
            String::from_utf8_lossy(&bytes).contains("not-a-real-secret"),
            "{name} fixture should carry a decoy secret"
        );
        let report = import_auto(&bytes);
        let dump = serde_json::to_string(&report).unwrap();
        assert!(!dump.contains("not-a-real-secret"), "{name} leaked");
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.message.contains("not imported")),
            "{name} gave no secrets warning"
        );
    }
}

#[test]
fn adapter_maps_to_core_entities() {
    let report = csv_format::parse(&fixture("hosts.csv"));
    let batch = report.to_entities();
    let prod = batch.groups.iter().find(|g| g.name == "Prod").unwrap();
    let web_group = batch.groups.iter().find(|g| g.name == "Web").unwrap();
    assert_eq!(web_group.parent_id.as_deref(), Some(prod.id.as_str()));
    assert_eq!(prod.parent_id, None);
    let index = |g: &str| batch.groups.iter().position(|x| x.name == g).unwrap();
    assert!(index("Prod") < index("Web"), "parents first");

    let web = batch.connections.iter().find(|c| c.name == "web1").unwrap();
    assert_eq!(web.group_id.as_deref(), Some(web_group.id.as_str()));
    assert_eq!(web.proxy_jump.as_deref(), Some("bastion"));
    assert!(web.notes.contains("~/.ssh/web1"));
    assert_eq!(
        web.target,
        easyssh_core::ConnectionTarget::Endpoint {
            hostname: "web1.example.com".into(),
            username: Some("alice".into()),
            port: 2222
        }
    );

    let ssh = openssh::parse_with_base(&fixture("openssh/config"), &ssh_dir()).to_entities();
    let legacy = ssh.connections.iter().find(|c| c.name == "legacy").unwrap();
    assert!(legacy.tags.iter().any(|t| t == OPENSSH_ONLY_TAG));
    assert!(legacy.notes.contains("ProxyCommand"));
}
