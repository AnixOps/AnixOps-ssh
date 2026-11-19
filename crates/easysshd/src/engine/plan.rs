//! What a connection needs, from the host record and the OpenSSH view of
//! it (`ssh -G`), turned into a decision (russh or OpenSSH) and, for russh,
//! the parameters of every hop.
//!
//! `ssh -G` prints the effective configuration for a host without
//! connecting, with every `Host` and `Match` already applied, so the engine
//! sees what OpenSSH would use: the real host name, port, user, identity
//! files and jump hosts of a config alias.

use super::choice::{choose_engine, unsupported_by_connection, wants_russh, Choice, Unsupported};
use super::registry::Key;
use easyssh_core::process::hide_window;
use easyssh_core::{Connection, ConnectionTarget, OpenSsh, SshEngine};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// The parts of `ssh -G` output the engine looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFacts {
    pub hostname: String,
    pub port: u16,
    pub user: String,
    pub identity_files: Vec<String>,
    pub identities_only: bool,
    pub certificate_files: Vec<String>,
    pub proxy_jump: Option<String>,
    pub proxy_command: bool,
    pub local_command: bool,
    pub remote_command: bool,
    pub forwards: bool,
    pub pubkey: bool,
    pub password: bool,
    pub keyboard_interactive: bool,
    pub password_attempts: Option<u32>,
    pub connect_timeout: Option<Duration>,
    pub keepalive: Option<Duration>,
    /// `SetEnv NAME=value` pairs, sent to the shell.
    pub set_env: Vec<(String, String)>,
}

fn set(value: &str) -> bool {
    !value.is_empty() && !value.eq_ignore_ascii_case("none")
}

fn yes(value: &str) -> bool {
    matches!(value.to_ascii_lowercase().as_str(), "yes" | "true")
}

/// Parses the output of `ssh -G`. `None` when it has no host name (not
/// output of `ssh -G`).
pub fn parse_ssh_g(text: &str) -> Option<HostFacts> {
    let mut facts = HostFacts {
        hostname: String::new(),
        port: 22,
        user: String::new(),
        identity_files: Vec::new(),
        identities_only: false,
        certificate_files: Vec::new(),
        proxy_jump: None,
        proxy_command: false,
        local_command: false,
        remote_command: false,
        forwards: false,
        pubkey: true,
        password: true,
        keyboard_interactive: true,
        password_attempts: None,
        connect_timeout: None,
        keepalive: None,
        set_env: Vec::new(),
    };
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let value = value.trim();
        match key {
            "hostname" => facts.hostname = value.to_owned(),
            "port" => facts.port = value.parse().unwrap_or(22),
            "user" => facts.user = value.to_owned(),
            "identityfile" => facts.identity_files.push(value.to_owned()),
            "identitiesonly" => facts.identities_only = yes(value),
            "certificatefile" => facts.certificate_files.push(value.to_owned()),
            "proxyjump" if set(value) => facts.proxy_jump = Some(value.to_owned()),
            "proxycommand" if set(value) => facts.proxy_command = true,
            "localcommand" if set(value) => facts.local_command = true,
            "remotecommand" if set(value) => facts.remote_command = true,
            "localforward" | "remoteforward" | "dynamicforward" => facts.forwards = true,
            "pubkeyauthentication" => facts.pubkey = yes(value),
            "passwordauthentication" => facts.password = yes(value),
            "kbdinteractiveauthentication" => facts.keyboard_interactive = yes(value),
            "numberofpasswordprompts" => facts.password_attempts = value.parse().ok(),
            "connecttimeout" => facts.connect_timeout = value.parse().ok().map(Duration::from_secs),
            "setenv" => facts.set_env.extend(parse_set_env(value)),
            "serveraliveinterval" => {
                facts.keepalive = value
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .map(Duration::from_secs)
            }
            _ => {}
        }
    }
    (!facts.hostname.is_empty()).then_some(facts)
}

/// `SetEnv` as `ssh -G` prints it: `NAME=value` entries on one line,
/// separated by spaces, a value with spaces in double quotes.
fn parse_set_env(value: &str) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for c in value.chars() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    entries.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        entries.push(current);
    }
    entries
        .into_iter()
        .filter_map(|entry| {
            let (name, value) = entry.split_once('=')?;
            (!name.is_empty()).then(|| (name.to_owned(), value.to_owned()))
        })
        .collect()
}

fn expand_home(path: &str) -> PathBuf {
    expand_home_in(path, dirs::home_dir().as_deref())
}

/// `~`, `~/…` and (as written on Windows) `~\…` start at `home`.
fn expand_home_in(path: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(path);
    };
    if path == "~" {
        return home.to_path_buf();
    }
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

fn existing(paths: &[String]) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for path in paths {
        let path = expand_home(path);
        if path.is_file() && !found.contains(&path) {
            found.push(path);
        }
    }
    found
}

fn cert_beside(key: &Path) -> bool {
    let mut name = key.as_os_str().to_owned();
    name.push("-cert.pub");
    Path::new(&name).is_file()
}

impl HostFacts {
    /// What this host needs that the russh engine lacks. Reads the identity
    /// files (to spot FIDO2 keys), so it touches the disk. FIDO2 `sk-*` keys
    /// count only on platforms where the engine has no authenticator backend.
    pub fn unsupported(&self) -> Vec<Unsupported> {
        self.unsupported_with(easyssh_ssh::hardware_backend_available())
    }

    fn unsupported_with(&self, security_keys: bool) -> Vec<Unsupported> {
        let mut found = Vec::new();
        if self.proxy_command {
            found.push(Unsupported::ProxyCommand);
        }
        if self.local_command {
            found.push(Unsupported::LocalCommand);
        }
        if self.remote_command {
            found.push(Unsupported::RemoteCommand);
        }
        if self.forwards {
            found.push(Unsupported::InlineForward);
        }
        if !existing(&self.certificate_files).is_empty() {
            found.push(Unsupported::Certificate);
        }
        for key in existing(&self.identity_files) {
            if cert_beside(&key) {
                found.push(Unsupported::Certificate);
            }
            if !security_keys
                && std::fs::read_to_string(&key)
                    .ok()
                    .and_then(|text| easyssh_ssh::security_key_type(&text))
                    .is_some()
            {
                found.push(Unsupported::SecurityKey);
            }
        }
        found
    }
}

/// Runs `ssh -G` for a target. Blocking.
fn run_ssh_g(user: Option<&str>, port: Option<u16>, host: &str) -> Result<HostFacts, String> {
    let ssh = OpenSsh.ssh_path().map_err(|e| e.to_string())?;
    let mut command = Command::new(ssh);
    command.arg("-G");
    if let Some(user) = user {
        command.args(["-l", user]);
    }
    if let Some(port) = port {
        command.args(["-p", &port.to_string()]);
    }
    command.arg("--").arg(host);
    hide_window(&mut command);
    let output = command
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ssh -G {host}: {}", error.trim()));
    }
    parse_ssh_g(&String::from_utf8_lossy(&output.stdout))
        .ok_or_else(|| format!("ssh -G {host}: no host name in the output"))
}

/// Whether the user's ssh config contains a `Match` block other than
/// `Match all`. Follows `Include` lines a few levels deep.
pub fn config_uses_match() -> bool {
    let Some(home) = dirs::home_dir() else {
        return false;
    };
    let ssh_dir = home.join(".ssh");
    uses_match(&ssh_dir.join("config"), &ssh_dir, 0)
}

fn uses_match(path: &Path, ssh_dir: &Path, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    for line in text.lines() {
        let line = line.trim();
        let (keyword, rest) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((keyword, rest)) => (keyword, rest.trim().trim_start_matches('=').trim()),
            None => (line, ""),
        };
        if keyword.eq_ignore_ascii_case("match") && !rest.eq_ignore_ascii_case("all") {
            return true;
        }
        if keyword.eq_ignore_ascii_case("include") {
            for pattern in rest.split_whitespace() {
                for file in expand_include(pattern.trim_matches('"'), ssh_dir) {
                    if uses_match(&file, ssh_dir, depth + 1) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// Files an `Include` pattern names: `~`-relative or relative to `~/.ssh`,
/// with `*` allowed in the file name.
fn expand_include(pattern: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = if pattern == "~" || pattern.starts_with("~/") || pattern.starts_with("~\\") {
        expand_home(pattern)
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        ssh_dir.join(pattern)
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if !name.contains('*') {
        return vec![path];
    }
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let (prefix, suffix) = name.split_once('*').unwrap_or((name, ""));
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let file = entry.file_name().to_string_lossy().into_owned();
            file.len() >= prefix.len() + suffix.len()
                && file.starts_with(prefix)
                && file.ends_with(suffix)
        })
        .map(|entry| entry.path())
        .collect();
    files.sort();
    files
}

/// `[user@]host[:port]` of a jump hop, or a bare alias.
fn split_hop(spec: &str) -> (Option<String>, String, Option<u16>) {
    let (user, rest) = match spec.rsplit_once('@') {
        Some((user, rest)) => (Some(user.to_owned()), rest),
        None => (None, spec),
    };
    if let Some(inner) = rest.strip_prefix('[') {
        if let Some((host, tail)) = inner.split_once(']') {
            let port = tail.strip_prefix(':').and_then(|p| p.parse().ok());
            return (user, host.to_owned(), port);
        }
    }
    match rest.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') => match port.parse() {
            Ok(port) => (user, host.to_owned(), Some(port)),
            Err(_) => (user, rest.to_owned(), None),
        },
        _ => (user, rest.to_owned(), None),
    }
}

/// The resolved parameters of one hop (or the target).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity_files: Vec<PathBuf>,
    pub agent: bool,
    pub password: bool,
    pub keyboard_interactive: bool,
    pub password_attempts: u32,
    pub connect_timeout: Option<Duration>,
    pub keepalive: Option<Duration>,
    /// What prompts call this host.
    pub label: String,
    /// The host's `SetEnv`, sent to its shell.
    pub set_env: Vec<(String, String)>,
}

impl HostSpec {
    fn from_facts(facts: &HostFacts, label: String) -> Self {
        let identity_files = if facts.pubkey {
            existing(&facts.identity_files)
        } else {
            Vec::new()
        };
        HostSpec {
            host: facts.hostname.clone(),
            port: facts.port,
            user: facts.user.clone(),
            // `IdentitiesOnly yes` with keys on disk: do not offer the
            // agent's other keys.
            agent: facts.pubkey && !(facts.identities_only && !identity_files.is_empty()),
            identity_files,
            password: facts.password,
            keyboard_interactive: facts.keyboard_interactive,
            password_attempts: facts.password_attempts.unwrap_or(3).max(1),
            connect_timeout: facts.connect_timeout,
            keepalive: facts.keepalive,
            label,
            set_env: facts.set_env.clone(),
        }
    }

    /// `user@host:port`, as the registry key and `engine.connections` show it.
    pub fn address(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// How to connect with the russh engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RusshPlan {
    pub key: Key,
    pub hops: Vec<HostSpec>,
    pub target: HostSpec,
}

/// The decision for a host, with the plan when it is russh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub choice: Choice,
    pub plan: Option<RusshPlan>,
}

fn collect_hops(
    spec: &str,
    depth: usize,
    found: &mut Vec<Unsupported>,
    hops: &mut Vec<HostSpec>,
) -> Result<(), String> {
    if depth > 4 {
        return Err("jump hosts are nested too deeply".into());
    }
    for hop in spec.split(',').map(str::trim).filter(|h| !h.is_empty()) {
        let (user, host, port) = split_hop(hop);
        let facts = run_ssh_g(user.as_deref(), port, &host)?;
        found.extend(facts.unsupported());
        if let Some(inner) = &facts.proxy_jump {
            collect_hops(inner, depth + 1, found, hops)?;
        }
        hops.push(HostSpec::from_facts(&facts, host));
    }
    Ok(())
}

/// Decides how `connection` runs and prepares the russh plan. Blocking: it
/// runs `ssh -G` and reads files, so call it from `spawn_blocking`.
pub fn resolve(connection: &Connection, global: SshEngine) -> Resolution {
    let preference = connection.engine.unwrap_or_default();
    if !wants_russh(global, preference) {
        return Resolution {
            choice: choose_engine(global, preference, &[]),
            plan: None,
        };
    }
    let mut found = unsupported_by_connection(connection);
    let openssh = |found: &[Unsupported], extra: Option<String>| {
        let mut choice = choose_engine(global, preference, found);
        if let Some(extra) = extra {
            choice = Choice {
                engine: SshEngine::Openssh,
                reason: extra,
            };
        }
        Resolution { choice, plan: None }
    };

    let target_facts = match &connection.target {
        // Opened by the serial path before an engine is ever chosen.
        ConnectionTarget::Serial(_) => {
            return openssh(&found, Some("serial ports do not use an SSH engine".into()))
        }
        ConnectionTarget::Alias { alias } => run_ssh_g(None, None, alias),
        ConnectionTarget::Endpoint {
            hostname,
            username,
            port,
        } => run_ssh_g(username.as_deref(), Some(*port), hostname).or_else(|_| {
            // No usable `ssh`: the record itself says everything russh needs.
            Ok(HostFacts {
                hostname: hostname.clone(),
                port: *port,
                user: username.clone().unwrap_or_else(whoami),
                ..parse_ssh_g("hostname x\n").expect("static facts")
            })
        }),
    };
    let facts = match target_facts {
        Ok(facts) => facts,
        Err(error) => {
            return openssh(
                &found,
                Some(format!(
                    "OpenSSH runs this host because its settings could not be read ({error})"
                )),
            )
        }
    };
    found.extend(facts.unsupported());
    if config_uses_match() {
        found.push(Unsupported::Match);
    }

    let mut hops = Vec::new();
    let jump = connection.proxy_jump.clone().or(facts.proxy_jump.clone());
    if let Some(jump) = jump {
        if let Err(error) = collect_hops(&jump, 0, &mut found, &mut hops) {
            return openssh(
                &found,
                Some(format!(
                    "OpenSSH runs this host because its jump hosts could not be read ({error})"
                )),
            );
        }
    }
    let choice = choose_engine(global, preference, &found);
    if choice.engine != SshEngine::Russh {
        return Resolution { choice, plan: None };
    }
    let target = HostSpec::from_facts(&facts, connection.name.clone());
    let key = Key {
        host_id: connection.id.clone(),
        target: target.address(),
        jump: hops.iter().map(HostSpec::address).collect(),
    };
    Resolution {
        choice,
        plan: Some(RusshPlan { key, hops, target }),
    }
}

fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "root".to_owned())
}

#[cfg(test)]
mod tests {
    #[test]
    fn home_relative_paths_expand_in_every_spelling() {
        use super::expand_home_in;
        use std::path::{Path, PathBuf};
        let home = Path::new("/home/me");
        let at = |p: &str| expand_home_in(p, Some(home));
        assert_eq!(at("~"), PathBuf::from("/home/me"));
        assert_eq!(at("~/.ssh/id_ed25519"), home.join(".ssh/id_ed25519"));
        assert_eq!(at("~\\.ssh\\id_ed25519"), home.join(".ssh\\id_ed25519"));
        assert_eq!(at("/etc/ssh/key"), PathBuf::from("/etc/ssh/key"));
        // `~user` is another user's home, which is not looked up.
        assert_eq!(at("~bob/key"), PathBuf::from("~bob/key"));
        assert_eq!(expand_home_in("~/k", None), PathBuf::from("~/k"));
    }

    use super::*;

    const SAMPLE: &str = "\
host e2e
user keyuser
hostname 127.0.0.1
port 2222
identityfile /nonexistent/id
identitiesonly yes
passwordauthentication yes
kbdinteractiveauthentication yes
pubkeyauthentication true
numberofpasswordprompts 1
connecttimeout 20
serveraliveinterval 0
proxyjump bastion
localcommand none
proxycommand none
";

    #[test]
    fn parses_what_the_engine_needs() {
        let facts = parse_ssh_g(SAMPLE).unwrap();
        assert_eq!(facts.hostname, "127.0.0.1");
        assert_eq!(facts.port, 2222);
        assert_eq!(facts.user, "keyuser");
        assert_eq!(facts.proxy_jump.as_deref(), Some("bastion"));
        assert!(facts.identities_only);
        assert_eq!(facts.password_attempts, Some(1));
        assert_eq!(facts.connect_timeout, Some(Duration::from_secs(20)));
        assert_eq!(facts.keepalive, None);
        // `none` values do not count as set.
        assert!(!facts.proxy_command && !facts.local_command);
        assert!(facts.unsupported().is_empty());
    }

    #[test]
    fn reads_set_env() {
        let facts = parse_ssh_g(
            "hostname h\nsetenv LANG=ja_JP.UTF-8 GREETING=\"hello world\" EMPTY= =bad\n",
        )
        .unwrap();
        assert_eq!(
            facts.set_env,
            vec![
                ("LANG".to_owned(), "ja_JP.UTF-8".to_owned()),
                ("GREETING".to_owned(), "hello world".to_owned()),
                ("EMPTY".to_owned(), String::new()),
            ]
        );
        assert!(parse_ssh_g(SAMPLE).unwrap().set_env.is_empty());
    }

    #[test]
    fn spots_proxycommand_forwards_and_commands() {
        let facts = parse_ssh_g(
            "hostname h\nproxycommand ssh -W x:22 y\nlocalcommand echo\nremotecommand ls\n\
             localforward 1 [x]:2\n",
        )
        .unwrap();
        assert_eq!(
            facts.unsupported(),
            vec![
                Unsupported::ProxyCommand,
                Unsupported::LocalCommand,
                Unsupported::RemoteCommand,
                Unsupported::InlineForward
            ]
        );
    }

    #[test]
    fn certificate_files_must_exist_to_count() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("c-cert.pub");
        let text = format!("hostname h\ncertificatefile {}\n", cert.display());
        assert!(parse_ssh_g(&text).unwrap().unsupported().is_empty());
        std::fs::write(&cert, "x").unwrap();
        assert_eq!(
            parse_ssh_g(&text).unwrap().unsupported(),
            vec![Unsupported::Certificate]
        );
    }

    #[test]
    fn a_certificate_beside_an_identity_counts() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id");
        std::fs::write(&key, "x").unwrap();
        let text = format!("hostname h\nidentityfile {}\n", key.display());
        assert!(parse_ssh_g(&text).unwrap().unsupported().is_empty());
        std::fs::write(dir.path().join("id-cert.pub"), "x").unwrap();
        assert_eq!(
            parse_ssh_g(&text).unwrap().unsupported(),
            vec![Unsupported::Certificate]
        );
    }

    #[test]
    fn a_fido2_identity_counts_only_without_an_authenticator_backend() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id_sk");
        // Not a real key: only the type name inside the base64 body matters.
        let mut body = b"openssh-key-v1\0".to_vec();
        body.extend_from_slice(b"sk-ssh-ed25519@openssh.com");
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(body);
        std::fs::write(
            &key,
            format!("-----BEGIN OPENSSH PRIVATE KEY-----\n{encoded}\n-----END OPENSSH PRIVATE KEY-----\n"),
        )
        .unwrap();
        let text = format!("hostname h\nidentityfile {}\n", key.display());
        let facts = parse_ssh_g(&text).unwrap();
        assert_eq!(
            facts.unsupported_with(false),
            vec![Unsupported::SecurityKey]
        );
        assert!(facts.unsupported_with(true).is_empty());
    }

    #[test]
    fn output_without_a_host_is_refused() {
        assert!(parse_ssh_g("user x\n").is_none());
    }

    #[test]
    fn splits_jump_hops() {
        assert_eq!(
            split_hop("me@bastion.example:2200"),
            (Some("me".into()), "bastion.example".into(), Some(2200))
        );
        assert_eq!(split_hop("alias"), (None, "alias".into(), None));
        assert_eq!(
            split_hop("u@[::1]:22"),
            (Some("u".into()), "::1".into(), Some(22))
        );
        assert_eq!(split_hop("::1"), (None, "::1".into(), None));
    }

    #[test]
    fn identities_only_keeps_the_agent_out() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("id");
        std::fs::write(&key, "x").unwrap();
        let text = format!(
            "hostname h\nuser u\nidentityfile {}\nidentitiesonly yes\n",
            key.display()
        );
        let spec = HostSpec::from_facts(&parse_ssh_g(&text).unwrap(), "h".into());
        assert!(!spec.agent);
        assert_eq!(spec.identity_files, vec![key.clone()]);
        let text = format!("hostname h\nuser u\nidentityfile {}\n", key.display());
        let spec = HostSpec::from_facts(&parse_ssh_g(&text).unwrap(), "h".into());
        assert!(spec.agent);
        assert_eq!(spec.address(), "u@h:22");
    }

    #[test]
    fn match_blocks_are_found_in_includes() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(&config, "Host a\n  User x\nInclude conf.d/*.conf\n").unwrap();
        std::fs::create_dir(dir.path().join("conf.d")).unwrap();
        std::fs::write(
            dir.path().join("conf.d/one.conf"),
            "Match all\n  User y\nMatch host b\n  User z\n",
        )
        .unwrap();
        assert!(uses_match(&config, dir.path(), 0));
        std::fs::write(dir.path().join("conf.d/one.conf"), "Match all\n User y\n").unwrap();
        assert!(!uses_match(&config, dir.path(), 0));
    }
}
