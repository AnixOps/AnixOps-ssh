//! What a connection needs, from the host record and the OpenSSH view of
//! it (`ssh -G`), turned into a decision (russh or OpenSSH) and, for russh,
//! the parameters of every hop.
//!
//! `ssh -G` prints the effective configuration for a host without
//! connecting, with every `Host` and `Match` already applied, so the engine
//! sees what OpenSSH would use: the real host name, port, user, identity
//! files and jump hosts of a config alias.

use super::choice::{choose_engine, unsupported_by_connection, wants_russh, Choice, Unsupported};
use super::options::{
    expand_tokens, glob_paths, local_hostname, local_uid, match_needs_openssh, split_words,
    ConfigOptions, HostKeyPolicy, IdentityAgent, Tokens, DEFAULT_IDENTITY_FILES,
};
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
    /// Options that are honoured or send the host to OpenSSH.
    pub options: ConfigOptions,
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
    /// `ServerAliveInterval`. `ssh -G` prints 0 both when it is unset and
    /// when the config turns keepalives off, so 0 cannot mean "off" here:
    /// it is `None` and the engine keeps its default (60 s). A dead path is
    /// what keepalives are for, and the engine must notice one.
    pub keepalive: Option<Duration>,
    /// `ServerAliveCountMax` (3 unless the config says otherwise).
    pub keepalive_max: Option<usize>,
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
        options: ConfigOptions::default(),
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
        keepalive_max: None,
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
            "serveraliveinterval" => {
                facts.keepalive = value
                    .parse::<u64>()
                    .ok()
                    .filter(|seconds| *seconds > 0)
                    .map(Duration::from_secs)
            }
            "serveralivecountmax" => {
                facts.keepalive_max = value.parse::<usize>().ok().filter(|count| *count > 0)
            }
            _ => {
                facts.options.parse(key, value);
            }
        }
    }
    (!facts.hostname.is_empty()).then_some(facts)
}

fn expand_home(path: &str) -> PathBuf {
    expand_home_in(path, dirs::home_dir().as_deref())
}

/// `~`, `~/…` and (as written on Windows) `~\…` start at `home`.
pub(super) fn expand_home_in(path: &str, home: Option<&Path>) -> PathBuf {
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

/// `~`, then `%` tokens and `${ENV}`, as OpenSSH expands key paths. `None`
/// when a token or variable cannot be expanded.
fn expand_path(path: &str, tokens: &Tokens, home: Option<&Path>) -> Option<PathBuf> {
    let path = expand_home_in(path, home);
    expand_tokens(&path.to_string_lossy(), tokens, &|name| {
        std::env::var(name).ok()
    })
    .map(PathBuf::from)
}

fn existing(paths: &[String], tokens: &Tokens, home: Option<&Path>) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    for path in paths {
        let Some(path) = expand_path(path, tokens, home) else {
            continue;
        };
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
    /// The values of `%` tokens for this host.
    fn tokens(&self, home: Option<&Path>) -> Tokens {
        let jump = self
            .proxy_jump
            .as_deref()
            .and_then(|jump| jump.rsplit(',').next())
            .map(|hop| split_hop(hop.trim()).1)
            .unwrap_or_default();
        Tokens {
            host: self.hostname.clone(),
            port: self.port,
            remote_user: self.user.clone(),
            original_host: if self.options.original_host.is_empty() {
                self.hostname.clone()
            } else {
                self.options.original_host.clone()
            },
            local_user: whoami(),
            home: home
                .map(|home| home.to_string_lossy().into_owned())
                .unwrap_or_default(),
            local_host: local_hostname(),
            jump,
            uid: local_uid(),
        }
    }

    /// The identity (or certificate) files of `paths` that exist, expanded.
    fn files(&self, paths: &[String]) -> Vec<PathBuf> {
        let home = dirs::home_dir();
        existing(paths, &self.tokens(home.as_deref()), home.as_deref())
    }

    /// The agent `IdentityAgent` names: `None` for the environment's,
    /// `Some(None)` for no agent.
    fn identity_agent(&self) -> Option<Option<PathBuf>> {
        let IdentityAgent::Path(path) = &self.options.identity_agent else {
            return match self.options.identity_agent {
                IdentityAgent::Disabled => Some(None),
                _ => None,
            };
        };
        // `IdentityAgent $NAME`: the socket named by a variable.
        if let Some(name) = path.strip_prefix('$').filter(|n| !n.starts_with('{')) {
            return Some(std::env::var_os(name).map(PathBuf::from));
        }
        let home = dirs::home_dir();
        Some(expand_path(
            path,
            &self.tokens(home.as_deref()),
            home.as_deref(),
        ))
    }

    /// The first `UserKnownHostsFile`, expanded. `None` when `ssh -G` did
    /// not say (the default applies); `Some(None)` for `/dev/null` or
    /// `none`.
    fn user_known_hosts(&self) -> Option<Option<PathBuf>> {
        let files = self.options.user_known_hosts.as_deref()?;
        let first = files.split_whitespace().next()?;
        if first.eq_ignore_ascii_case("none") || first == "/dev/null" || first == "NUL" {
            return Some(None);
        }
        let home = dirs::home_dir();
        Some(expand_path(
            first,
            &self.tokens(home.as_deref()),
            home.as_deref(),
        ))
    }

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
        found.extend(self.options.unsupported());
        if !self.files(&self.certificate_files).is_empty() {
            found.push(Unsupported::Certificate);
        }
        for key in self.files(&self.identity_files) {
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

/// Whether the user's ssh config contains a `Match` block whose criteria
/// `ssh -G` cannot evaluate as the connection would (`exec`, `canonical`,
/// `final`, `localnetwork`, `tagged`). Other `Match` blocks are already
/// applied in the `ssh -G` output. Follows `Include` lines a few levels deep.
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
        if line.starts_with('#') {
            continue;
        }
        let (keyword, rest) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((keyword, rest)) => (keyword, rest.trim().trim_start_matches('=').trim()),
            None => (line, ""),
        };
        let words = split_words(rest);
        if keyword.eq_ignore_ascii_case("match") && match_needs_openssh(&words) {
            return true;
        }
        if keyword.eq_ignore_ascii_case("include") {
            for pattern in &words {
                for file in expand_include(pattern, ssh_dir) {
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
/// with `*` and `?` allowed in any part of the path.
fn expand_include(pattern: &str, ssh_dir: &Path) -> Vec<PathBuf> {
    let path = if pattern == "~" || pattern.starts_with("~/") || pattern.starts_with("~\\") {
        expand_home(pattern)
    } else if Path::new(pattern).is_absolute() {
        PathBuf::from(pattern)
    } else {
        ssh_dir.join(pattern)
    };
    if !pattern.contains(['*', '?']) {
        return vec![path];
    }
    glob_paths(&path)
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
    /// The agent socket `IdentityAgent` names, instead of the environment's.
    pub agent_path: Option<PathBuf>,
    /// `StrictHostKeyChecking`.
    pub host_key_policy: HostKeyPolicy,
    /// The OpenSSH known_hosts file to read (the first `UserKnownHostsFile`):
    /// `None` for the default, `Some(None)` for none (`/dev/null`).
    pub user_known_hosts: Option<Option<PathBuf>>,
    pub password: bool,
    pub keyboard_interactive: bool,
    pub password_attempts: u32,
    pub connect_timeout: Option<Duration>,
    pub keepalive: Option<Duration>,
    pub keepalive_max: Option<usize>,
    /// What prompts call this host.
    pub label: String,
}

impl HostSpec {
    fn from_facts(facts: &HostFacts, label: String) -> Self {
        let identity_files = if facts.pubkey {
            facts.files(&facts.identity_files)
        } else {
            Vec::new()
        };
        let identity_agent = facts.identity_agent();
        HostSpec {
            host: facts.hostname.clone(),
            port: facts.port,
            user: facts.user.clone(),
            // `IdentitiesOnly yes`: only the configured keys, never the
            // agent's others, even when none of the files exists.
            agent: facts.pubkey && !facts.identities_only && !matches!(identity_agent, Some(None)),
            agent_path: identity_agent.flatten(),
            host_key_policy: facts.options.host_key_policy,
            user_known_hosts: facts.user_known_hosts(),
            identity_files,
            password: facts.password,
            keyboard_interactive: facts.keyboard_interactive,
            password_attempts: facts.password_attempts.unwrap_or(3).max(1),
            connect_timeout: facts.connect_timeout,
            keepalive: facts.keepalive,
            keepalive_max: facts.keepalive_max,
            label,
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
        } => run_ssh_g(username.as_deref(), Some(*port), hostname)
            .or_else(|_| Ok(facts_without_ssh(hostname, *port, username.as_deref()))),
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
        shard: 0,
    };
    Resolution {
        choice,
        plan: Some(RusshPlan { key, hops, target }),
    }
}

/// The facts of a saved host when `ssh -G` cannot be run: the record itself
/// says everything russh needs, and the keys are OpenSSH's default ones.
fn facts_without_ssh(hostname: &str, port: u16, user: Option<&str>) -> HostFacts {
    HostFacts {
        hostname: hostname.to_owned(),
        port,
        user: user.map(str::to_owned).unwrap_or_else(whoami),
        identity_files: DEFAULT_IDENTITY_FILES
            .iter()
            .map(|file| (*file).to_owned())
            .collect(),
        ..parse_ssh_g("hostname x\n").expect("static facts")
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
        assert_eq!(facts.keepalive_max, None);
        // `none` values do not count as set.
        assert!(!facts.proxy_command && !facts.local_command);
        assert!(facts.unsupported().is_empty());
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
        // Not even when none of the configured files exists.
        let text = "hostname h\nuser u\nidentityfile /nonexistent/id\nidentitiesonly yes\n";
        let spec = HostSpec::from_facts(&parse_ssh_g(text).unwrap(), "h".into());
        assert!(!spec.agent);
        assert!(spec.identity_files.is_empty());
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
            "Match all\n  User y\nMatch exec \"test -e /x\"\n  User z\n",
        )
        .unwrap();
        assert!(uses_match(&config, dir.path(), 0));
        std::fs::write(dir.path().join("conf.d/one.conf"), "Match all\n User y\n").unwrap();
        assert!(!uses_match(&config, dir.path(), 0));
    }

    #[test]
    fn match_blocks_ssh_g_evaluates_stay_on_russh() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            "Match host *.lan user root\n  Port 2222\nMatch originalhost exec\n  User x\n\
             # Match exec \"commented out\"\n",
        )
        .unwrap();
        assert!(!uses_match(&config, dir.path(), 0));
        for line in [
            "Match canonical all",
            "Match final",
            "Match host a !exec true",
        ] {
            std::fs::write(&config, format!("{line}\n  User x\n")).unwrap();
            assert!(uses_match(&config, dir.path(), 0), "{line}");
        }
    }

    #[test]
    fn includes_take_quotes_question_marks_and_several_stars() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir(dir.path().join("my confs")).unwrap();
        std::fs::write(
            dir.path().join("my confs/host-a-1.conf"),
            "Match exec true\n",
        )
        .unwrap();
        std::fs::write(
            &config,
            format!(
                "Include \"{}/my confs/h?st-*-*.conf\"\n",
                dir.path().display()
            ),
        )
        .unwrap();
        assert!(uses_match(&config, dir.path(), 0));
        std::fs::write(&config, "Include \"my confs/*-?.conf\"\n").unwrap();
        assert!(uses_match(&config, dir.path(), 0));
        std::fs::write(&config, "Include \"my confs/*-??.conf\"\n").unwrap();
        assert!(!uses_match(&config, dir.path(), 0));
    }

    #[test]
    fn key_paths_expand_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let key = dir.path().join("deploy@example.org-2222");
        std::fs::write(&key, "x").unwrap();
        let text = format!(
            "hostname example.org\nport 2222\nuser deploy\nidentityfile {}/%r@%h-%p\n\
             identityfile {}/%%unknown\n",
            dir.path().display(),
            dir.path().display()
        );
        let facts = parse_ssh_g(&text).unwrap();
        let spec = HostSpec::from_facts(&facts, "h".into());
        assert_eq!(spec.identity_files, vec![key]);
    }

    #[test]
    fn identity_agent_picks_or_disables_the_agent() {
        let spec = |extra: &str| {
            let text = format!("hostname h\nuser u\n{extra}");
            HostSpec::from_facts(&parse_ssh_g(&text).unwrap(), "h".into())
        };
        let plain = spec("");
        assert!(plain.agent && plain.agent_path.is_none());
        let none = spec("identityagent none\n");
        assert!(!none.agent);
        let path = spec("identityagent /run/user/1/%r.sock\n");
        assert!(path.agent);
        assert_eq!(path.agent_path, Some(PathBuf::from("/run/user/1/u.sock")));
        let env = spec("identityagent SSH_AUTH_SOCK\n");
        assert!(env.agent && env.agent_path.is_none());
    }

    #[test]
    fn the_user_known_hosts_file_of_the_config_is_read() {
        let spec = |extra: &str| {
            let text = format!("hostname h\nuser u\n{extra}");
            HostSpec::from_facts(&parse_ssh_g(&text).unwrap(), "h".into()).user_known_hosts
        };
        assert_eq!(spec(""), None);
        assert_eq!(
            spec("userknownhostsfile /tmp/e2e/known_hosts /tmp/e2e/known_hosts2\n"),
            Some(Some(PathBuf::from("/tmp/e2e/known_hosts")))
        );
        assert_eq!(spec("userknownhostsfile /dev/null\n"), Some(None));
        // Not a reason for OpenSSH, and neither is accept-new.
        let facts = parse_ssh_g(
            "hostname h\nuserknownhostsfile /tmp/k\nglobalknownhostsfile /dev/null\n\
             stricthostkeychecking accept-new\n",
        )
        .unwrap();
        assert!(facts.unsupported().is_empty());
    }

    #[test]
    fn parsed_options_reach_the_plan() {
        let facts =
            parse_ssh_g("host alias\nhostname h\nstricthostkeychecking yes\nforwardagent yes\n")
                .unwrap();
        assert_eq!(facts.options.original_host, "alias");
        assert_eq!(
            HostSpec::from_facts(&facts, "h".into()).host_key_policy,
            HostKeyPolicy::Strict
        );
        assert_eq!(facts.unsupported(), vec![Unsupported::AgentForwarding]);
    }

    #[test]
    fn without_ssh_the_default_keys_are_tried() {
        let facts = facts_without_ssh("h", 22, Some("u"));
        assert_eq!(facts.user, "u");
        assert_eq!(
            facts.identity_files,
            [
                "~/.ssh/id_rsa",
                "~/.ssh/id_ecdsa",
                "~/.ssh/id_ecdsa_sk",
                "~/.ssh/id_ed25519",
                "~/.ssh/id_ed25519_sk",
                "~/.ssh/id_dsa"
            ]
        );
        // Only the ones that exist are used.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".ssh")).unwrap();
        std::fs::write(dir.path().join(".ssh/id_ed25519"), "x").unwrap();
        let found = existing(&facts.identity_files, &Tokens::default(), Some(dir.path()));
        assert_eq!(found, vec![dir.path().join(".ssh/id_ed25519")]);
    }

    #[test]
    fn keepalives_follow_the_config_and_zero_keeps_the_default() {
        let facts =
            parse_ssh_g("hostname h\nserveraliveinterval 15\nserveralivecountmax 5\n").unwrap();
        assert_eq!(facts.keepalive, Some(Duration::from_secs(15)));
        assert_eq!(facts.keepalive_max, Some(5));
        let spec = HostSpec::from_facts(&facts, "h".into());
        assert_eq!(spec.keepalive, Some(Duration::from_secs(15)));
        assert_eq!(spec.keepalive_max, Some(5));
        // What `ssh -G` prints for a host with nothing configured.
        let facts =
            parse_ssh_g("hostname h\nserveraliveinterval 0\nserveralivecountmax 3\n").unwrap();
        assert_eq!(facts.keepalive, None);
        assert_eq!(facts.keepalive_max, Some(3));
        // Nonsense is ignored.
        let facts =
            parse_ssh_g("hostname h\nserveralivecountmax 0\nserveraliveinterval x\n").unwrap();
        assert_eq!((facts.keepalive, facts.keepalive_max), (None, None));
    }
}
