//! Ansible static inventories, YAML and INI.
//!
//! **YAML**: top-level keys are groups (normally `all`); a group is a mapping
//! with `hosts` (name -> vars), `children` (name -> group, nested arbitrarily)
//! and `vars`. **INI**: `[group]` with `host key=value ...` lines (shell-style
//! quoting), `[group:children]` and `[group:vars]`; hosts before any section
//! are in `ungrouped`. Host patterns with ranges (`web[01:03].example.com`,
//! `db-[a:c]`, `[1:9:2]`) are expanded, up to [`MAX_RANGE_HOSTS`] per pattern
//! and [`MAX_HOSTS`] per inventory, with a warning beyond.
//!
//! Variable precedence (as in Ansible): `all` < parent group < child group
//! (deeper wins, equal depth by group name) < host variables.
//!
//! | Ansible variable                                            | Imported as          |
//! |-------------------------------------------------------------|----------------------|
//! | inventory name                                              | `label`              |
//! | `ansible_host` / `ansible_ssh_host` (else the name)         | `hostname`           |
//! | `ansible_user` / `ansible_ssh_user`                         | `username`           |
//! | `ansible_port` / `ansible_ssh_port`                         | `port`               |
//! | `ansible_ssh_private_key_file` / `ansible_private_key_file` | `identity_file`      |
//! | `-J x` / `-o ProxyJump=x` in `ansible_ssh_common_args` or `ansible_ssh_extra_args` | `jump` |
//!
//! Group path: the chain of child groups above the host, outermost first,
//! without `all` and `ungrouped`. A host in several groups goes under the
//! deepest one (ties: the group listed first for the host); its other groups
//! become `Tags:` in the notes. Where a group has several parents the one
//! defined first is followed.
//!
//! Never imported, with a warning: passwords (`ansible_password`,
//! `ansible_ssh_pass`, `ansible_become_pass`, ...), `ProxyCommand`, templated
//! (`{{ ... }}`) and vault-encrypted values. Hosts whose `ansible_connection`
//! is not an SSH transport (`local`, `winrm`, `docker`, ...) are skipped.

use crate::model::{ImportReport, ImportWarning, ImportedHost};
use crate::util::{decode_text, parse_port};
use serde_norway::Value as Yaml;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

/// Hosts one pattern may expand to.
pub const MAX_RANGE_HOSTS: usize = 1024;
/// Hosts one inventory may hold.
pub const MAX_HOSTS: usize = 20_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

const PASSWORD_VARS: &[&str] = &[
    "ansible_password",
    "ansible_ssh_pass",
    "ansible_ssh_password",
    "ansible_become_pass",
    "ansible_become_password",
    "ansible_sudo_pass",
    "ansible_sudo_password",
    "ansible_su_pass",
    "ansible_su_password",
    "ansible_ssh_private_key_passphrase",
    "ansible_ssh_pipelining_password",
];

/// Connection plugins that run over SSH.
const SSH_CONNECTIONS: &[&str] = &[
    "ssh",
    "smart",
    "paramiko",
    "paramiko_ssh",
    "libssh",
    "network_cli",
    "netconf",
];

pub fn detect(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(256 * 1024)];
    let text = decode_text(head);
    looks_like_yaml(&text) || looks_like_ini(&text)
}

fn content_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{FEFF}'))
        .enumerate()
        .filter(|(_, l)| !l.is_empty() && !l.starts_with('#') && !l.starts_with(';'))
}

fn looks_like_yaml(text: &str) -> bool {
    let mut structural = false;
    for (_, line) in content_lines(text).take(4000) {
        if line == "---" || line.starts_with("--- ") {
            continue;
        }
        let key = line.trim_start_matches("- ");
        let Some((name, rest)) = key.split_once(':') else {
            // A YAML inventory is all `key:` lines; section headers and
            // `key=value` lines mean INI or something else.
            if line.starts_with('[') || line.contains('=') {
                return false;
            }
            continue;
        };
        let name = name.trim().trim_matches(['"', '\'']);
        let rest = rest.trim();
        if matches!(name, "children" | "hosts") && (rest.is_empty() || rest.starts_with('{')) {
            structural = true;
        }
    }
    structural
}

/// True for text whose first content line is a `key:` (or `{`), which is
/// how a YAML inventory starts even when it has no hosts yet.
fn starts_like_yaml(text: &str) -> bool {
    let Some((_, first)) = content_lines(text).find(|(_, l)| *l != "---") else {
        return false;
    };
    first.starts_with('{')
        || first.split_once(':').is_some_and(|(k, rest)| {
            !k.contains(char::is_whitespace)
                && (rest.trim().is_empty() || rest.trim().starts_with(['{', '[', '&', '!', '*']))
        })
}

fn looks_like_ini(text: &str) -> bool {
    content_lines(text).take(4000).any(|(_, line)| {
        if let Some(rest) = line.strip_prefix('[') {
            let name = rest.split_once(']').map_or(rest, |(n, _)| n);
            return name.ends_with(":children") || name.ends_with(":vars");
        }
        let lower = line.to_ascii_lowercase();
        lower
            .split_whitespace()
            .any(|t| t.starts_with("ansible_") && t.contains('='))
    })
}

// ---- inventory model -----------------------------------------------------

#[derive(Debug, Clone)]
enum Var {
    Text(String),
    /// A vault-encrypted, list or mapping value.
    Opaque,
}

type Vars = Vec<(String, Var)>;

#[derive(Default)]
struct Group {
    name: String,
    children: Vec<usize>,
    hosts: Vec<usize>,
    vars: Vars,
}

#[derive(Default)]
struct Host {
    name: String,
    vars: Vars,
    groups: Vec<usize>,
}

#[derive(Default)]
struct Inventory {
    groups: Vec<Group>,
    group_index: HashMap<String, usize>,
    hosts: Vec<Host>,
    host_index: HashMap<String, usize>,
    truncated_hosts: bool,
}

impl Inventory {
    fn group(&mut self, name: &str) -> usize {
        if let Some(&i) = self.group_index.get(name) {
            return i;
        }
        self.groups.push(Group {
            name: name.to_owned(),
            ..Group::default()
        });
        let i = self.groups.len() - 1;
        self.group_index.insert(name.to_owned(), i);
        i
    }

    fn host(&mut self, name: &str) -> Option<usize> {
        if let Some(&i) = self.host_index.get(name) {
            return Some(i);
        }
        if self.hosts.len() >= MAX_HOSTS {
            self.truncated_hosts = true;
            return None;
        }
        self.hosts.push(Host {
            name: name.to_owned(),
            ..Host::default()
        });
        let i = self.hosts.len() - 1;
        self.host_index.insert(name.to_owned(), i);
        Some(i)
    }

    fn add_child(&mut self, parent: usize, child: usize) {
        if parent != child && !self.groups[parent].children.contains(&child) {
            self.groups[parent].children.push(child);
        }
    }

    fn add_member(&mut self, group: usize, host: usize) {
        if !self.groups[group].hosts.contains(&host) {
            self.groups[group].hosts.push(host);
        }
        if !self.hosts[host].groups.contains(&group) {
            self.hosts[host].groups.push(group);
        }
    }
}

fn set_var(vars: &mut Vars, key: &str, value: Var) {
    match vars.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = value,
        None => vars.push((key.to_owned(), value)),
    }
}

/// Adds a variable, noting secrets in the report instead of storing them.
fn add_var(report: &mut ImportReport, record: &str, vars: &mut Vars, key: &str, value: Var) {
    if PASSWORD_VARS.contains(&key) {
        report.secret_skipped(record, key);
    } else {
        set_var(vars, key, value);
    }
}

// ---- host patterns -------------------------------------------------------

enum Seg {
    Lit(String),
    Num {
        start: u64,
        step: u64,
        count: u64,
        width: usize,
    },
    Alpha {
        start: u8,
        step: u8,
        count: u64,
    },
}

impl Seg {
    fn len(&self) -> u64 {
        match self {
            Seg::Lit(_) => 1,
            Seg::Num { count, .. } | Seg::Alpha { count, .. } => *count,
        }
    }

    fn at(&self, i: u64) -> String {
        match self {
            Seg::Lit(s) => s.clone(),
            Seg::Num {
                start, step, width, ..
            } => format!("{:0width$}", start + i * step, width = *width),
            Seg::Alpha { start, step, .. } => ((*start + (i as u8) * *step) as char).to_string(),
        }
    }
}

/// Parses the inside of `[a:b]` / `[a:b:s]`. `None` when it is not a range
/// (IPv6 literals, anything else); `Some(Err)` for a malformed range.
fn parse_range(inner: &str) -> Option<Result<Seg, String>> {
    let parts: Vec<&str> = inner.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let step = match parts.get(2) {
        Some(s) => match s.parse::<u64>() {
            Ok(n) if n > 0 => n,
            _ => return Some(Err(format!("invalid range step in [{inner}]"))),
        },
        None => 1,
    };
    let digits = |s: &str| s.len() <= 9 && s.chars().all(|c| c.is_ascii_digit());
    if digits(parts[0]) && digits(parts[1]) {
        let (start, end): (u64, u64) = (parts[0].parse().ok()?, parts[1].parse().ok()?);
        if end < start {
            return Some(Err(format!("range [{inner}] runs backwards")));
        }
        let width = if parts[0].len() > 1 && parts[0].starts_with('0') {
            parts[0].len()
        } else {
            0
        };
        return Some(Ok(Seg::Num {
            start,
            step,
            count: (end - start) / step + 1,
            width,
        }));
    }
    let letter = |s: &str| s.len() == 1 && s.as_bytes()[0].is_ascii_alphabetic();
    if letter(parts[0]) && letter(parts[1]) {
        let (start, end) = (parts[0].as_bytes()[0], parts[1].as_bytes()[0]);
        if end < start {
            return Some(Err(format!("range [{inner}] runs backwards")));
        }
        return Some(Ok(Seg::Alpha {
            start,
            step: step.min(255) as u8,
            count: u64::from(end - start) / step + 1,
        }));
    }
    None
}

/// Expands a host pattern. Returns the names and whether the cap cut them.
fn expand_pattern(pattern: &str) -> Result<(Vec<String>, bool), String> {
    let mut segs: Vec<Seg> = Vec::new();
    let mut lit = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']') else {
            break;
        };
        let inner = &rest[open + 1..open + close];
        match parse_range(inner) {
            Some(Ok(seg)) => {
                lit.push_str(&rest[..open]);
                segs.push(Seg::Lit(std::mem::take(&mut lit)));
                segs.push(seg);
            }
            Some(Err(message)) => return Err(message),
            None => lit.push_str(&rest[..open + close + 1]),
        }
        rest = &rest[open + close + 1..];
    }
    lit.push_str(rest);
    if segs.is_empty() {
        return Ok((vec![pattern.to_owned()], false));
    }
    segs.push(Seg::Lit(lit));
    let total = segs
        .iter()
        .fold(1u64, |acc, s| acc.saturating_mul(s.len().max(1)));
    let take = total.min(MAX_RANGE_HOSTS as u64);
    let mut out = Vec::with_capacity(take as usize);
    for mut n in 0..take {
        let mut parts: Vec<String> = Vec::with_capacity(segs.len());
        for seg in segs.iter().rev() {
            let len = seg.len().max(1);
            parts.push(seg.at(n % len));
            n /= len;
        }
        parts.reverse();
        out.push(parts.concat());
    }
    Ok((out, total > take))
}

/// Splits `host:port` (not for IPv6 literals or ranges).
fn split_port(name: &str) -> (&str, Option<&str>) {
    let mut depth = 0usize;
    let mut colons = Vec::new();
    for (i, c) in name.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            ':' if depth == 0 => colons.push(i),
            _ => {}
        }
    }
    match colons.as_slice() {
        [i] if name[i + 1..].chars().all(|c| c.is_ascii_digit()) && *i + 1 < name.len() => {
            (&name[..*i], Some(&name[i + 1..]))
        }
        _ => (name, None),
    }
}

// ---- shell-style splitting -----------------------------------------------

/// Splits like a POSIX shell without expansion. An unquoted `#` at the start
/// of a token (when `comments`) ends the line. Unterminated quotes take the
/// rest of the input.
fn shell_split(text: &str, comments: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut have = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                have = true;
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    cur.push(q);
                }
            }
            '"' => {
                have = true;
                while let Some(q) = chars.next() {
                    match q {
                        '"' => break,
                        '\\' => match chars.peek() {
                            Some(&n) if matches!(n, '"' | '\\' | '$' | '`') => {
                                cur.push(n);
                                chars.next();
                            }
                            _ => cur.push('\\'),
                        },
                        _ => cur.push(q),
                    }
                }
            }
            '\\' => {
                have = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            '#' if comments && !have => break,
            c if c.is_whitespace() => {
                if have {
                    out.push(std::mem::take(&mut cur));
                    have = false;
                }
            }
            c => {
                have = true;
                cur.push(c);
            }
        }
    }
    if have {
        out.push(cur);
    }
    out
}

/// Removes one pair of matching quotes around an INI variable value.
fn unquote(value: &str) -> String {
    let v = value.trim();
    for q in ['"', '\''] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            return v[1..v.len() - 1].to_owned();
        }
    }
    v.to_owned()
}

// ---- INI -----------------------------------------------------------------

enum Section {
    Hosts(usize),
    Children(usize),
    Vars(usize),
    Skip,
}

fn parse_ini(text: &str, inv: &mut Inventory, report: &mut ImportReport) {
    let ungrouped = inv.group("ungrouped");
    let mut section = Section::Hosts(ungrouped);
    for (index, line) in content_lines(text) {
        let number = index + 1;
        if let Some(rest) = line.strip_prefix('[') {
            let name = rest.split_once(']').map_or(rest, |(n, _)| n).trim();
            section = match name.rsplit_once(':') {
                _ if name.is_empty() => {
                    report.warn(ImportWarning::at_line(number, "empty section name"));
                    Section::Skip
                }
                Some((group, "children")) => Section::Children(inv.group(group.trim())),
                Some((group, "vars")) => Section::Vars(inv.group(group.trim())),
                Some((_, other)) if other.chars().all(|c| c.is_ascii_alphabetic()) => {
                    report.warn(ImportWarning::at_line(
                        number,
                        format!("unknown section type `{other}`, section ignored"),
                    ));
                    Section::Skip
                }
                _ => Section::Hosts(inv.group(name)),
            };
            continue;
        }
        match section {
            Section::Skip => {}
            Section::Children(parent) => {
                let child = line.split_whitespace().next().unwrap_or("");
                let child = inv.group(child);
                inv.add_child(parent, child);
            }
            Section::Vars(group) => {
                let Some((key, value)) = line.split_once('=') else {
                    report.warn(ImportWarning::at_line(number, "expected `key=value`"));
                    continue;
                };
                let record = inv.groups[group].name.clone();
                let mut vars = std::mem::take(&mut inv.groups[group].vars);
                add_var(
                    report,
                    &record,
                    &mut vars,
                    key.trim(),
                    Var::Text(unquote(value)),
                );
                inv.groups[group].vars = vars;
            }
            Section::Hosts(group) => {
                let tokens = shell_split(line, true);
                let Some((pattern, args)) = tokens.split_first() else {
                    continue;
                };
                let mut vars: Vars = Vec::new();
                let (pattern, port) = split_port(pattern);
                if let Some(port) = port {
                    vars.push(("ansible_port".into(), Var::Text(port.to_owned())));
                }
                for arg in args {
                    match arg.split_once('=') {
                        Some((key, value)) if !key.is_empty() => {
                            add_var(report, pattern, &mut vars, key, Var::Text(value.to_owned()))
                        }
                        _ => report.warn(ImportWarning::at_line(
                            number,
                            format!("{pattern}: ignored `{arg}` (expected `key=value`)"),
                        )),
                    }
                }
                add_host_pattern(inv, report, group, pattern, &vars, Some(number));
            }
        }
    }
}

/// Expands `pattern`, creates the hosts, joins them to `group` and merges
/// `vars` into each.
fn add_host_pattern(
    inv: &mut Inventory,
    report: &mut ImportReport,
    group: usize,
    pattern: &str,
    vars: &Vars,
    line: Option<usize>,
) {
    let warn = |report: &mut ImportReport, message: String| {
        let mut w = ImportWarning::for_record(pattern, message);
        w.line = line;
        report.warn(w);
    };
    let (names, truncated) = match expand_pattern(pattern) {
        Ok(result) => result,
        Err(message) => {
            report.skipped += 1;
            warn(report, message);
            return;
        }
    };
    if truncated {
        warn(
            report,
            format!("range expands to more than {MAX_RANGE_HOSTS} hosts, only the first {MAX_RANGE_HOSTS} were imported"),
        );
    }
    for name in names {
        let Some(host) = inv.host(&name) else {
            return;
        };
        inv.add_member(group, host);
        for (key, value) in vars {
            set_var(&mut inv.hosts[host].vars, key, value.clone());
        }
    }
}

// ---- YAML ----------------------------------------------------------------

fn yaml_text(value: &Yaml) -> Option<String> {
    match value {
        Yaml::String(s) => Some(s.clone()),
        Yaml::Number(n) => Some(n.to_string()),
        Yaml::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn yaml_var(value: &Yaml) -> Option<Var> {
    match value {
        Yaml::Null => None,
        // `!vault` and other tagged values carry data we must not read.
        Yaml::Tagged(_) => Some(Var::Opaque),
        other => Some(yaml_text(other).map_or(Var::Opaque, Var::Text)),
    }
}

fn yaml_vars(report: &mut ImportReport, record: &str, value: &Yaml, out: &mut Vars, what: &str) {
    match value {
        Yaml::Null => {}
        Yaml::Mapping(map) => {
            for (key, value) in map {
                let (Some(key), Some(var)) = (yaml_text(key), yaml_var(value)) else {
                    continue;
                };
                add_var(report, record, out, &key, var);
            }
        }
        _ => report.warn(ImportWarning::for_record(
            record,
            format!("`{what}` must be a mapping"),
        )),
    }
}

fn yaml_group(
    inv: &mut Inventory,
    report: &mut ImportReport,
    name: &str,
    body: &Yaml,
    parent: Option<usize>,
    depth: usize,
) {
    let group = inv.group(name);
    if let Some(parent) = parent {
        inv.add_child(parent, group);
    }
    let map = match body {
        Yaml::Null => return,
        Yaml::Mapping(map) => map,
        _ => {
            report.warn(ImportWarning::for_record(name, "group must be a mapping"));
            return;
        }
    };
    if depth > MAX_DEPTH {
        report.warn(ImportWarning::for_record(
            name,
            "groups nested too deeply, rest ignored",
        ));
        return;
    }
    for (key, value) in map {
        match yaml_text(key).as_deref() {
            Some("vars") => {
                let mut vars = std::mem::take(&mut inv.groups[group].vars);
                yaml_vars(report, name, value, &mut vars, "vars");
                inv.groups[group].vars = vars;
            }
            Some("hosts") => yaml_hosts(inv, report, group, name, value),
            Some("children") => match value {
                Yaml::Null => {}
                Yaml::Mapping(children) => {
                    for (child, body) in children {
                        match yaml_text(child) {
                            Some(child) => {
                                yaml_group(inv, report, &child, body, Some(group), depth + 1)
                            }
                            None => report.warn(ImportWarning::for_record(
                                name,
                                "ignored non-text group name",
                            )),
                        }
                    }
                }
                _ => report.warn(ImportWarning::for_record(
                    name,
                    "`children` must be a mapping",
                )),
            },
            other => report.warn(ImportWarning::for_record(
                name,
                format!("ignored unknown key `{}`", other.unwrap_or("?")),
            )),
        }
    }
}

fn yaml_hosts(
    inv: &mut Inventory,
    report: &mut ImportReport,
    group: usize,
    group_name: &str,
    value: &Yaml,
) {
    let mut entries: Vec<(String, &Yaml)> = Vec::new();
    match value {
        Yaml::Null => {}
        Yaml::Mapping(map) => {
            for (key, body) in map {
                match yaml_text(key) {
                    Some(name) => entries.push((name, body)),
                    None => report.warn(ImportWarning::for_record(
                        group_name,
                        "ignored non-text host name",
                    )),
                }
            }
        }
        Yaml::Sequence(items) => {
            for item in items {
                if let Some(name) = yaml_text(item) {
                    entries.push((name, &Yaml::Null));
                }
            }
        }
        _ => report.warn(ImportWarning::for_record(
            group_name,
            "`hosts` must be a mapping",
        )),
    }
    for (name, body) in entries {
        let mut vars: Vars = Vec::new();
        yaml_vars(report, &name, body, &mut vars, "host");
        add_host_pattern(inv, report, group, &name, &vars, None);
    }
}

fn parse_yaml(text: &str, inv: &mut Inventory, report: &mut ImportReport) {
    let root: Yaml = match serde_norway::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            report.warn(ImportWarning::new(format!("invalid YAML: {error}")));
            return;
        }
    };
    match root {
        Yaml::Null => {}
        Yaml::Mapping(map) => {
            for (key, body) in &map {
                match yaml_text(key) {
                    Some(name) => yaml_group(inv, report, &name, body, None, 0),
                    None => report.warn(ImportWarning::new("ignored non-text top-level key")),
                }
            }
        }
        _ => report.warn(ImportWarning::new(
            "expected a YAML mapping of groups at the top level",
        )),
    }
}

// ---- resolution ----------------------------------------------------------

fn is_implicit(name: &str) -> bool {
    name == "all" || name == "ungrouped"
}

struct Resolver<'a> {
    inv: &'a Inventory,
    parents: Vec<Vec<usize>>,
    depth: Vec<usize>,
}

impl<'a> Resolver<'a> {
    fn new(inv: &'a Inventory) -> Self {
        let mut parents = vec![Vec::new(); inv.groups.len()];
        for (p, group) in inv.groups.iter().enumerate() {
            for &c in &group.children {
                parents[c].push(p);
            }
        }
        let mut resolver = Self {
            inv,
            parents,
            depth: vec![usize::MAX; inv.groups.len()],
        };
        for g in 0..inv.groups.len() {
            resolver.depth_of(g, &mut HashSet::new());
        }
        resolver
    }

    /// `all` is 0, a group without parents 1, otherwise parents + 1.
    fn depth_of(&mut self, g: usize, seen: &mut HashSet<usize>) -> usize {
        if self.depth[g] != usize::MAX {
            return self.depth[g];
        }
        if self.inv.groups[g].name == "all" {
            self.depth[g] = 0;
            return 0;
        }
        if !seen.insert(g) {
            return 1; // cycle
        }
        let parents = self.parents[g].clone();
        let d = parents
            .iter()
            .map(|&p| self.depth_of(p, seen))
            .max()
            .map_or(1, |d| d + 1);
        seen.remove(&g);
        self.depth[g] = d;
        d
    }

    /// Groups whose variables apply to `host`, lowest precedence first.
    fn var_groups(&self, host: &Host) -> Vec<usize> {
        let mut seen: HashSet<usize> = HashSet::new();
        let mut stack: Vec<usize> = host.groups.clone();
        if let Some(&all) = self.inv.group_index.get("all") {
            stack.push(all);
        }
        while let Some(g) = stack.pop() {
            if seen.insert(g) {
                stack.extend(&self.parents[g]);
            }
        }
        let mut list: Vec<usize> = seen.into_iter().collect();
        list.sort_by(|a, b| {
            (self.depth[*a], &self.inv.groups[*a].name)
                .cmp(&(self.depth[*b], &self.inv.groups[*b].name))
        });
        list
    }

    /// The group the host is filed under and the path leading to it.
    fn placement(&self, host: &Host) -> (Vec<String>, Vec<String>) {
        let direct: Vec<usize> = host
            .groups
            .iter()
            .copied()
            .filter(|&g| !is_implicit(&self.inv.groups[g].name))
            .collect();
        // Deepest wins; `max_by_key` keeps the last maximum, so reverse.
        let Some(&primary) = direct.iter().rev().max_by_key(|&&g| self.depth[g]) else {
            return (Vec::new(), Vec::new());
        };
        let mut chain = vec![primary];
        let mut seen: HashSet<usize> = HashSet::from([primary]);
        let mut at = primary;
        while let Some(&p) = self.parents[at]
            .iter()
            .find(|&&p| !is_implicit(&self.inv.groups[p].name) && !seen.contains(&p))
        {
            chain.push(p);
            seen.insert(p);
            at = p;
        }
        chain.reverse();
        let tags = direct
            .iter()
            .filter(|g| !chain.contains(g))
            .map(|&g| self.inv.groups[g].name.clone())
            .collect();
        let path = chain
            .iter()
            .map(|&g| self.inv.groups[g].name.trim().to_owned())
            .filter(|n| !n.is_empty())
            .collect();
        (path, tags)
    }
}

fn is_templated(text: &str) -> bool {
    text.contains("{{") || text.contains("{%")
}

/// A variable's text from `names` (first present wins); warns for
/// templated and non-text values and returns `None` for them.
fn lookup(
    report: &mut ImportReport,
    record: &str,
    vars: &HashMap<&str, &Var>,
    names: &[&str],
) -> Option<String> {
    for name in names {
        match vars.get(name) {
            Some(Var::Text(text)) if text.trim().is_empty() => {}
            Some(Var::Text(text)) if is_templated(text) => {
                report.warn(ImportWarning::for_record(
                    record,
                    format!("`{name}` is templated ({text}) and was not imported"),
                ));
                return None;
            }
            Some(Var::Text(text)) => return Some(text.trim().to_owned()),
            Some(Var::Opaque) => {
                report.warn(ImportWarning::for_record(
                    record,
                    format!("`{name}` is encrypted or not a plain value and was not imported"),
                ));
                return None;
            }
            None => {}
        }
    }
    None
}

fn split_hops(list: &str, jump: &mut Vec<String>) {
    jump.extend(
        list.split(',')
            .map(|h| h.trim().to_owned())
            .filter(|h| !h.is_empty()),
    );
}

/// Applies one `-o` option (`Key=value` or `Key value`).
fn apply_option(option: &str, jump: &mut Vec<String>, proxy_command: &mut bool) {
    let option = option.trim();
    let (key, value) = option
        .split_once(['=', ' ', '\t'])
        .map_or((option, ""), |(k, v)| {
            (k, v.trim().trim_start_matches('=').trim())
        });
    if key.eq_ignore_ascii_case("proxyjump") {
        if !value.eq_ignore_ascii_case("none") {
            split_hops(value, jump);
        }
    } else if key.eq_ignore_ascii_case("proxycommand") {
        *proxy_command = true;
    }
}

/// Jump hosts and whether a `ProxyCommand` appears in ssh arguments.
fn proxy_from_args(args: &str) -> (Vec<String>, bool) {
    let tokens = shell_split(args, false);
    let mut jump = Vec::new();
    let mut proxy_command = false;
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i].as_str();
        if t == "-o" || t == "-J" {
            if let Some(next) = tokens.get(i + 1) {
                if t == "-J" {
                    split_hops(next, &mut jump);
                } else {
                    apply_option(next, &mut jump, &mut proxy_command);
                }
                i += 1;
            }
        } else if let Some(rest) = t.strip_prefix("-J") {
            split_hops(rest, &mut jump);
        } else if let Some(rest) = t.strip_prefix("-o") {
            apply_option(rest, &mut jump, &mut proxy_command);
        }
        i += 1;
    }
    (jump, proxy_command)
}

fn connection_is_ssh(value: &str) -> bool {
    let name = value.rsplit('.').next().unwrap_or(value).trim();
    SSH_CONNECTIONS.contains(&name.to_ascii_lowercase().as_str())
}

pub fn parse(bytes: &[u8]) -> ImportReport {
    let mut report = ImportReport::default();
    if bytes.len() > MAX_BYTES {
        report.warn(ImportWarning::new(format!(
            "inventory larger than {} MiB, not read",
            MAX_BYTES / (1024 * 1024)
        )));
        return report.finish();
    }
    let text = decode_text(bytes);
    let mut inv = Inventory::default();
    if looks_like_yaml(&text) || starts_like_yaml(&text) {
        parse_yaml(&text, &mut inv, &mut report);
    } else {
        parse_ini(&text, &mut inv, &mut report);
    }
    if inv.truncated_hosts {
        report.warn(ImportWarning::new(format!(
            "more than {MAX_HOSTS} hosts, the rest were ignored"
        )));
    }
    build(&inv, &mut report);
    report.finish()
}

fn build(inv: &Inventory, report: &mut ImportReport) {
    let resolver = Resolver::new(inv);
    for host in &inv.hosts {
        let label = host.name.as_str();
        if is_templated(label) {
            report.skipped += 1;
            report.warn(ImportWarning::for_record(
                label,
                "templated host name was not imported",
            ));
            continue;
        }
        // Effective variables: groups (lowest precedence first), then host.
        let mut effective: HashMap<&str, &Var> = HashMap::new();
        for g in resolver.var_groups(host) {
            for (k, v) in &inv.groups[g].vars {
                effective.insert(k, v);
            }
        }
        for (k, v) in &host.vars {
            effective.insert(k, v);
        }

        if let Some(Var::Text(connection)) = effective.get("ansible_connection") {
            if !connection_is_ssh(connection) {
                report.skipped += 1;
                report.warn(ImportWarning::for_record(
                    label,
                    format!("ansible_connection `{connection}` is not SSH, host skipped"),
                ));
                continue;
            }
        }

        let hostname = lookup(
            report,
            label,
            &effective,
            &["ansible_host", "ansible_ssh_host"],
        );
        let templated_host = effective
            .get("ansible_host")
            .or(effective.get("ansible_ssh_host"))
            .is_some_and(|v| matches!(v, Var::Text(t) if is_templated(t)));
        let mut out = ImportedHost::new(label, hostname.unwrap_or_else(|| label.to_owned()));
        if templated_host {
            report.warn(ImportWarning::for_record(
                label,
                "using the inventory name as the hostname",
            ));
        }
        out.username = lookup(
            report,
            label,
            &effective,
            &["ansible_user", "ansible_ssh_user"],
        );
        if let Some(port) = lookup(
            report,
            label,
            &effective,
            &["ansible_port", "ansible_ssh_port"],
        ) {
            match parse_port(&port) {
                Some(port) => out.port = port,
                None => report.warn(ImportWarning::for_record(
                    label,
                    format!("invalid port `{port}`, using 22"),
                )),
            }
        }
        out.identity_file = lookup(
            report,
            label,
            &effective,
            &["ansible_ssh_private_key_file", "ansible_private_key_file"],
        )
        .map(PathBuf::from);

        for name in ["ansible_ssh_common_args", "ansible_ssh_extra_args"] {
            let Some(args) = lookup(report, label, &effective, &[name]) else {
                continue;
            };
            let (jump, proxy_command) = proxy_from_args(&args);
            if proxy_command {
                report.warn(ImportWarning::for_record(
                    label,
                    format!("ProxyCommand in `{name}` was not imported"),
                ));
            }
            for hop in jump {
                if hop.starts_with('-')
                    || is_templated(&hop)
                    || hop.chars().any(|c| c.is_control() || c.is_whitespace())
                {
                    report.warn(ImportWarning::for_record(
                        label,
                        format!("jump host {hop:?} is not acceptable and was not imported"),
                    ));
                } else if !out.jump.contains(&hop) {
                    out.jump.push(hop);
                }
            }
        }

        let (path, tags) = resolver.placement(host);
        out.group_path = path;
        if !tags.is_empty() {
            out.notes = format!("Tags: {}", tags.join(", "));
        }
        report.push_host(out);
    }
}
