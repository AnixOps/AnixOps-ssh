//! `ssh -G` options beyond the basics the plan needs, and what they mean for
//! the russh engine: honoured (`IdentityAgent`, `StrictHostKeyChecking yes`,
//! `UserKnownHostsFile`), or a reason to run the host on OpenSSH instead
//! ([`Unsupported`]). Also the expansion of `%` tokens and `${ENV}` in paths,
//! which `ssh -G` leaves to connection time for identity files.
//!
//! `StrictHostKeyChecking accept-new` keeps the engine's question about an
//! unknown key (the app's dialog is the stricter answer), and
//! `GlobalKnownHostsFile` is not read.
//!
//! OpenSSH defaults never count as unsupported: `ssh -G` prints them for
//! every host (`forwardagent no`, `stricthostkeychecking ask`, the default
//! algorithm lists), and treating them as settings would send every host to
//! OpenSSH.

use super::choice::Unsupported;
use easyssh_ssh::known_hosts::{fingerprint, KnownHostStatus, KnownHosts};
use russh::keys::ssh_key::{Algorithm, PublicKey};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How unknown and changed host keys are treated (`StrictHostKeyChecking`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HostKeyPolicy {
    /// `ask`, the default: the user decides about an unknown key.
    #[default]
    Ask,
    /// `yes`: an unknown key is refused without asking.
    Strict,
    /// `accept-new`: OpenSSH records an unknown key without asking. The
    /// engine still asks (the app's host key dialog is the stricter
    /// choice, and nothing is accepted behind the user's back); a changed
    /// key is refused either way.
    AcceptNew,
    /// `no`/`off`: any key is accepted, even a changed one. The russh engine
    /// does not do that; OpenSSH runs the host.
    Off,
}

/// A known_hosts store that applies `StrictHostKeyChecking yes`: an unknown
/// key is refused without asking. A changed key is left to the engine, which
/// refuses it.
struct PolicyKnownHosts {
    inner: Arc<dyn KnownHosts>,
    policy: HostKeyPolicy,
}

/// `store` with `policy` applied; `ask` leaves it as it is.
pub fn with_policy(store: Arc<dyn KnownHosts>, policy: HostKeyPolicy) -> Arc<dyn KnownHosts> {
    match policy {
        HostKeyPolicy::Strict => Arc::new(PolicyKnownHosts {
            inner: store,
            policy,
        }),
        HostKeyPolicy::Ask | HostKeyPolicy::AcceptNew | HostKeyPolicy::Off => store,
    }
}

impl KnownHosts for PolicyKnownHosts {
    fn check(
        &self,
        host: &str,
        port: u16,
        key: &PublicKey,
    ) -> easyssh_ssh::Result<KnownHostStatus> {
        let status = self.inner.check(host, port, key)?;
        if status != KnownHostStatus::Unknown {
            return Ok(status);
        }
        match self.policy {
            HostKeyPolicy::Strict => Err(easyssh_ssh::Error::HostKeyRejected {
                host: host.to_owned(),
                port,
                fingerprint: format!(
                    "{}; StrictHostKeyChecking is on and the key is not in known_hosts",
                    fingerprint(key)
                ),
            }),
            HostKeyPolicy::Ask | HostKeyPolicy::AcceptNew | HostKeyPolicy::Off => Ok(status),
        }
    }

    fn add(&self, host: &str, port: u16, key: &PublicKey) -> easyssh_ssh::Result<()> {
        self.inner.add(host, port, key)
    }

    fn replace(&self, host: &str, port: u16, key: &PublicKey) -> easyssh_ssh::Result<()> {
        self.inner.replace(host, port, key)
    }

    fn recorded_algorithms(&self, host: &str, port: u16) -> easyssh_ssh::Result<Vec<Algorithm>> {
        self.inner.recorded_algorithms(host, port)
    }
}

/// Where the agent is (`IdentityAgent`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityAgent {
    /// Not set, or `SSH_AUTH_SOCK`: the agent of the environment.
    Environment,
    /// `none`: no agent.
    Disabled,
    /// A socket path (or a Windows pipe name), not yet expanded.
    Path(String),
}

/// The options of `ssh -G` this module looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigOptions {
    /// `host`: the name as given to ssh (an alias), for `%n`.
    pub original_host: String,
    pub forward_agent: bool,
    pub forward_x11: bool,
    pub identity_agent: IdentityAgent,
    pub send_env: Vec<String>,
    pub set_env: Vec<String>,
    pub host_key_alias: Option<String>,
    /// `UserKnownHostsFile`, as printed (space-separated, maybe `~`).
    pub user_known_hosts: Option<String>,
    pub host_key_policy: HostKeyPolicy,
    pub kex: Vec<String>,
    pub ciphers: Vec<String>,
    pub macs: Vec<String>,
    pub host_key_algorithms: Vec<String>,
}

impl Default for ConfigOptions {
    fn default() -> Self {
        ConfigOptions {
            original_host: String::new(),
            forward_agent: false,
            forward_x11: false,
            identity_agent: IdentityAgent::Environment,
            send_env: Vec::new(),
            set_env: Vec::new(),
            host_key_alias: None,
            user_known_hosts: None,
            host_key_policy: HostKeyPolicy::Ask,
            kex: Vec::new(),
            ciphers: Vec::new(),
            macs: Vec::new(),
            host_key_algorithms: Vec::new(),
        }
    }
}

fn list(value: &str) -> Vec<String> {
    // `ssh -G` prints the resolved list; a leading `+`, `-` or `^` only
    // appears in hand-written text.
    value
        .trim_start_matches(['+', '-', '^'])
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

impl ConfigOptions {
    /// Takes one `key value` line of `ssh -G`. `false` when the key is not
    /// one of these options.
    pub fn parse(&mut self, key: &str, value: &str) -> bool {
        let lower = value.to_ascii_lowercase();
        match key {
            "host" => self.original_host = value.to_owned(),
            "forwardagent" => self.forward_agent = !matches!(lower.as_str(), "no" | "false"),
            "forwardx11" => self.forward_x11 = matches!(lower.as_str(), "yes" | "true"),
            "identityagent" => {
                self.identity_agent = match lower.as_str() {
                    "none" => IdentityAgent::Disabled,
                    "ssh_auth_sock" | "$ssh_auth_sock" | "${ssh_auth_sock}" => {
                        IdentityAgent::Environment
                    }
                    _ => IdentityAgent::Path(value.to_owned()),
                }
            }
            "sendenv" => self
                .send_env
                .extend(value.split_whitespace().map(str::to_owned)),
            "setenv" => self.set_env.push(value.to_owned()),
            "hostkeyalias" => {
                self.host_key_alias =
                    (!lower.is_empty() && lower != "none").then(|| value.to_owned())
            }
            "userknownhostsfile" => self.user_known_hosts = Some(value.to_owned()),
            // Not read by the engine (see the module documentation).
            "globalknownhostsfile" => {}
            "stricthostkeychecking" => {
                self.host_key_policy = match lower.as_str() {
                    "yes" | "true" => HostKeyPolicy::Strict,
                    "accept-new" => HostKeyPolicy::AcceptNew,
                    "no" | "false" | "off" => HostKeyPolicy::Off,
                    _ => HostKeyPolicy::Ask,
                }
            }
            "kexalgorithms" => self.kex = list(value),
            "ciphers" => self.ciphers = list(value),
            "macs" => self.macs = list(value),
            "hostkeyalgorithms" => self.host_key_algorithms = list(value),
            _ => return false,
        }
        true
    }

    /// What of these options the russh engine cannot honour.
    pub fn unsupported(&self) -> Vec<Unsupported> {
        let mut found = Vec::new();
        if self.forward_agent {
            found.push(Unsupported::AgentForwarding);
        }
        if self.forward_x11 {
            found.push(Unsupported::X11Forwarding);
        }
        if !self.set_env.is_empty() || self.send_env.iter().any(|name| !default_send_env(name)) {
            found.push(Unsupported::Environment);
        }
        if self.host_key_alias.is_some() {
            found.push(Unsupported::HostKeyAlias);
        }
        if self.host_key_policy == HostKeyPolicy::Off {
            found.push(Unsupported::HostKeyChecking);
        }
        if legacy_algorithms(self).next().is_some() {
            found.push(Unsupported::LegacyAlgorithm);
        }
        found
    }
}

/// `SendEnv` lines the distributions put in `/etc/ssh/ssh_config` (Debian,
/// Ubuntu, Fedora): the locale and colour hints. Servers mostly refuse them
/// (`AcceptEnv` is off by default), so leaving them out changes nothing for
/// most hosts, and counting them would send every host to OpenSSH there.
fn default_send_env(name: &str) -> bool {
    name.starts_with('-')
        || matches!(
            name,
            "LANG" | "LANGUAGE" | "LC_*" | "LC_ALL" | "COLORTERM" | "NO_COLOR" | "XMODIFIERS"
        )
        || name.starts_with("LC_")
}

/// The algorithms OpenSSH clients offer by default (the union over the
/// releases still in use, so an older client's defaults do not count as a
/// choice). Anything else in a list was added on purpose.
const OPENSSH_KEX: &[&str] = &[
    "mlkem768x25519-sha256",
    "sntrup761x25519-sha512",
    "sntrup761x25519-sha512@openssh.com",
    "curve25519-sha256",
    "curve25519-sha256@libssh.org",
    "ecdh-sha2-nistp256",
    "ecdh-sha2-nistp384",
    "ecdh-sha2-nistp521",
    "diffie-hellman-group-exchange-sha256",
    "diffie-hellman-group16-sha512",
    "diffie-hellman-group18-sha512",
    "diffie-hellman-group14-sha256",
    "ext-info-c",
    "kex-strict-c-v00@openssh.com",
];
const OPENSSH_CIPHERS: &[&str] = &[
    "chacha20-poly1305@openssh.com",
    "aes128-ctr",
    "aes192-ctr",
    "aes256-ctr",
    "aes128-gcm@openssh.com",
    "aes256-gcm@openssh.com",
];
const OPENSSH_MACS: &[&str] = &[
    "umac-64-etm@openssh.com",
    "umac-128-etm@openssh.com",
    "hmac-sha2-256-etm@openssh.com",
    "hmac-sha2-512-etm@openssh.com",
    "hmac-sha1-etm@openssh.com",
    "umac-64@openssh.com",
    "umac-128@openssh.com",
    "hmac-sha2-256",
    "hmac-sha2-512",
    "hmac-sha1",
];
const OPENSSH_HOST_KEYS: &[&str] = &[
    "ssh-ed25519-cert-v01@openssh.com",
    "ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "ecdsa-sha2-nistp384-cert-v01@openssh.com",
    "ecdsa-sha2-nistp521-cert-v01@openssh.com",
    "sk-ssh-ed25519-cert-v01@openssh.com",
    "sk-ecdsa-sha2-nistp256-cert-v01@openssh.com",
    "rsa-sha2-512-cert-v01@openssh.com",
    "rsa-sha2-256-cert-v01@openssh.com",
    "ssh-rsa-cert-v01@openssh.com",
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "sk-ssh-ed25519@openssh.com",
    "sk-ecdsa-sha2-nistp256@openssh.com",
    "rsa-sha2-512",
    "rsa-sha2-256",
    "ssh-rsa",
];

/// Algorithms in the host's lists that neither OpenSSH offers by default
/// nor the russh engine negotiates: the user enabled a legacy algorithm
/// (`diffie-hellman-group1-sha1`, `aes128-cbc`, `ssh-dss`...) for a server
/// that needs it, which only OpenSSH can talk to.
pub fn legacy_algorithms(options: &ConfigOptions) -> impl Iterator<Item = &str> {
    fn legacy<'a>(items: &'a [String], openssh: &[&str], ours: &[String]) -> Vec<&'a str> {
        items
            .iter()
            .map(String::as_str)
            .filter(|item| !openssh.contains(item) && !ours.iter().any(|o| o == item))
            .collect()
    }
    let russh = russh::Preferred::DEFAULT;
    let kex: Vec<String> = russh.kex.iter().map(|n| n.as_ref().to_owned()).collect();
    let ciphers: Vec<String> = russh.cipher.iter().map(|n| n.as_ref().to_owned()).collect();
    let macs: Vec<String> = russh.mac.iter().map(|n| n.as_ref().to_owned()).collect();
    let keys: Vec<String> = russh.key.iter().map(|a| a.as_str().to_owned()).collect();
    legacy(&options.kex, OPENSSH_KEX, &kex)
        .into_iter()
        .chain(legacy(&options.ciphers, OPENSSH_CIPHERS, &ciphers))
        .chain(legacy(&options.macs, OPENSSH_MACS, &macs))
        .chain(legacy(
            &options.host_key_algorithms,
            OPENSSH_HOST_KEYS,
            &keys,
        ))
}

/// The values `%` tokens stand for, as OpenSSH defines them for
/// `IdentityFile`, `CertificateFile` and `IdentityAgent`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tokens {
    /// `%h`: the remote host name.
    pub host: String,
    /// `%p`
    pub port: u16,
    /// `%r`: the remote user.
    pub remote_user: String,
    /// `%n`: the host as given (an alias).
    pub original_host: String,
    /// `%u`: the local user.
    pub local_user: String,
    /// `%d`: the local home directory.
    pub home: String,
    /// `%l`: the local host name, with domain.
    pub local_host: String,
    /// `%j`: the jump host, empty without one.
    pub jump: String,
    /// `%i`: the local user id (Unix).
    pub uid: String,
}

impl Tokens {
    /// `%C`: SHA-1 of `%l%h%p%r%j`, in hex.
    fn connection_hash(&self) -> String {
        use sha1::{Digest, Sha1};
        let mut hash = Sha1::new();
        for part in [
            self.local_host.as_str(),
            self.host.as_str(),
            &self.port.to_string(),
            self.remote_user.as_str(),
            self.jump.as_str(),
        ] {
            hash.update(part.as_bytes());
        }
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Expands `%` tokens and `${NAME}` environment references in `path`, the
/// way OpenSSH does for key files. `None` when a token is unknown or a
/// variable is unset (OpenSSH refuses such a path).
pub fn expand_tokens(
    path: &str,
    tokens: &Tokens,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '%' => {
                let value = match chars.next()? {
                    '%' => "%".to_owned(),
                    'h' => tokens.host.clone(),
                    'p' => tokens.port.to_string(),
                    'r' => tokens.remote_user.clone(),
                    'n' => tokens.original_host.clone(),
                    'u' => tokens.local_user.clone(),
                    'd' => tokens.home.clone(),
                    'l' => tokens.local_host.clone(),
                    'L' => tokens
                        .local_host
                        .split('.')
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                    'j' => tokens.jump.clone(),
                    'i' => tokens.uid.clone(),
                    'C' => tokens.connection_hash(),
                    _ => return None,
                };
                out.push_str(&value);
            }
            '$' if chars.peek() == Some(&'{') => {
                chars.next();
                let mut name = String::new();
                loop {
                    match chars.next()? {
                        '}' => break,
                        c => name.push(c),
                    }
                }
                out.push_str(&env(&name)?);
            }
            c => out.push(c),
        }
    }
    Some(out)
}

/// The local host name, for `%l` and `%L`.
pub fn local_hostname() -> String {
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        // SAFETY: the buffer is valid for its length; gethostname writes at
        // most that many bytes.
        let ok = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0;
        if ok {
            let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            return String::from_utf8_lossy(&buf[..end]).into_owned();
        }
        String::new()
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// The local user id, for `%i`.
pub fn local_uid() -> String {
    #[cfg(unix)]
    {
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() }.to_string()
    }
    #[cfg(not(unix))]
    {
        String::new()
    }
}

/// The keys OpenSSH tries when the config names none, in its order. Only
/// used when `ssh -G` could not be run: its output lists them itself.
pub const DEFAULT_IDENTITY_FILES: &[&str] = &[
    "~/.ssh/id_rsa",
    "~/.ssh/id_ecdsa",
    "~/.ssh/id_ecdsa_sk",
    "~/.ssh/id_ed25519",
    "~/.ssh/id_ed25519_sk",
    "~/.ssh/id_dsa",
];

/// Splits a config line into words, keeping double-quoted words whole (and
/// without their quotes), as OpenSSH's argv splitting does.
pub fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for c in line.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// Whether the criteria of a `Match` line include one `ssh -G` cannot be
/// trusted to have evaluated as the real connection would: `exec` (runs a
/// command, whose answer can change), `canonical` and `final` (evaluated
/// after host name canonicalisation), `localnetwork` (depends on the
/// network at connect time) and `tagged`.
pub fn match_needs_openssh(criteria: &[String]) -> bool {
    let mut words = criteria.iter();
    while let Some(word) = words.next() {
        let keyword = word.trim_start_matches('!').to_ascii_lowercase();
        match keyword.as_str() {
            "all" => {}
            "canonical" | "final" => return true,
            "exec" | "localnetwork" | "tagged" => return true,
            _ => {
                // `host`, `user`, `localuser`, `originalhost`, `address`...
                // take one argument, which `ssh -G` has already matched.
                words.next();
            }
        }
    }
    false
}

/// Whether `name` matches a glob pattern with `*` and `?` (no `[...]`). A
/// leading `.` must be matched literally, as glob(3) wants.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    if name.starts_with('.') && !pattern.starts_with('.') {
        return false;
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The files a path with `*` or `?` in any component names, sorted.
pub fn glob_paths(path: &Path) -> Vec<PathBuf> {
    let mut found = vec![PathBuf::new()];
    for component in path.components() {
        let text = component.as_os_str().to_string_lossy();
        if !text.contains(['*', '?']) {
            for base in &mut found {
                base.push(component.as_os_str());
            }
            continue;
        }
        let mut next = Vec::new();
        for base in &found {
            let mut matches: Vec<PathBuf> = std::fs::read_dir(base)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| glob_match(&text, &entry.file_name().to_string_lossy()))
                .map(|entry| entry.path())
                .collect();
            matches.sort();
            next.extend(matches);
        }
        found = next;
    }
    found.retain(|path| path.is_file());
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(text: &str) -> ConfigOptions {
        let mut options = ConfigOptions::default();
        for line in text.lines() {
            let (key, value) = line.split_once(' ').unwrap();
            assert!(options.parse(key, value.trim()), "{key}");
        }
        options
    }

    /// What `ssh -G` prints for a host without any setting (OpenSSH 9.9,
    /// Debian's system config).
    const DEFAULTS: &str = "\
host plain
forwardagent no
forwardx11 no
sendenv LANG
sendenv LC_*
sendenv COLORTERM
sendenv NO_COLOR
userknownhostsfile /home/me/.ssh/known_hosts /home/me/.ssh/known_hosts2
globalknownhostsfile /etc/ssh/ssh_known_hosts /etc/ssh/ssh_known_hosts2
stricthostkeychecking ask
kexalgorithms mlkem768x25519-sha256,sntrup761x25519-sha512,sntrup761x25519-sha512@openssh.com,curve25519-sha256,curve25519-sha256@libssh.org,ecdh-sha2-nistp256,ecdh-sha2-nistp384,ecdh-sha2-nistp521,diffie-hellman-group-exchange-sha256,diffie-hellman-group16-sha512,diffie-hellman-group18-sha512,diffie-hellman-group14-sha256
ciphers chacha20-poly1305@openssh.com,aes128-ctr,aes192-ctr,aes256-ctr,aes128-gcm@openssh.com,aes256-gcm@openssh.com
macs umac-64-etm@openssh.com,umac-128-etm@openssh.com,hmac-sha2-256-etm@openssh.com,hmac-sha2-512-etm@openssh.com,hmac-sha1-etm@openssh.com,umac-64@openssh.com,umac-128@openssh.com,hmac-sha2-256,hmac-sha2-512,hmac-sha1
hostkeyalgorithms ssh-ed25519-cert-v01@openssh.com,ecdsa-sha2-nistp256-cert-v01@openssh.com,ecdsa-sha2-nistp384-cert-v01@openssh.com,ecdsa-sha2-nistp521-cert-v01@openssh.com,sk-ssh-ed25519-cert-v01@openssh.com,sk-ecdsa-sha2-nistp256-cert-v01@openssh.com,rsa-sha2-512-cert-v01@openssh.com,rsa-sha2-256-cert-v01@openssh.com,ssh-ed25519,ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521,sk-ssh-ed25519@openssh.com,sk-ecdsa-sha2-nistp256@openssh.com,rsa-sha2-512,rsa-sha2-256
";

    #[test]
    fn openssh_defaults_are_not_unsupported() {
        let parsed = options(DEFAULTS);
        assert_eq!(parsed.original_host, "plain");
        assert_eq!(parsed.host_key_policy, HostKeyPolicy::Ask);
        assert_eq!(parsed.identity_agent, IdentityAgent::Environment);
        assert_eq!(parsed.unsupported(), vec![]);
    }

    #[test]
    fn options_russh_cannot_honour_fall_back() {
        let cases: &[(&str, Unsupported)] = &[
            ("forwardagent yes", Unsupported::AgentForwarding),
            ("forwardagent /run/agent.sock", Unsupported::AgentForwarding),
            ("forwardx11 yes", Unsupported::X11Forwarding),
            ("sendenv EDITOR", Unsupported::Environment),
            ("setenv FOO=bar", Unsupported::Environment),
            ("hostkeyalias other", Unsupported::HostKeyAlias),
            ("stricthostkeychecking false", Unsupported::HostKeyChecking),
            (
                "kexalgorithms curve25519-sha256,diffie-hellman-group1-sha1",
                Unsupported::LegacyAlgorithm,
            ),
            (
                "ciphers aes128-ctr,aes128-cbc",
                Unsupported::LegacyAlgorithm,
            ),
            (
                "hostkeyalgorithms ssh-ed25519,ssh-dss",
                Unsupported::LegacyAlgorithm,
            ),
            ("macs hmac-md5", Unsupported::LegacyAlgorithm),
        ];
        for (line, expected) in cases {
            assert_eq!(options(line).unsupported(), vec![*expected], "{line}");
        }
    }

    #[test]
    fn honoured_options_do_not_fall_back() {
        for line in [
            "stricthostkeychecking true",
            "stricthostkeychecking accept-new",
            // Read as the OpenSSH file the engine consults.
            "userknownhostsfile /tmp/test_known_hosts",
            "userknownhostsfile /dev/null",
            "globalknownhostsfile /dev/null",
            "identityagent none",
            "identityagent ~/.1password/agent.sock",
            "hostkeyalias none",
            // Restricting to fewer algorithms, or adding one russh speaks.
            "ciphers aes256-gcm@openssh.com",
            "hostkeyalgorithms ssh-ed25519,ssh-rsa",
        ] {
            assert_eq!(options(line).unsupported(), vec![], "{line}");
        }
        assert_eq!(
            options("stricthostkeychecking true").host_key_policy,
            HostKeyPolicy::Strict
        );
        assert_eq!(
            options("stricthostkeychecking accept-new").host_key_policy,
            HostKeyPolicy::AcceptNew
        );
        assert_eq!(
            options("identityagent none").identity_agent,
            IdentityAgent::Disabled
        );
        assert_eq!(
            options("identityagent SSH_AUTH_SOCK").identity_agent,
            IdentityAgent::Environment
        );
        assert_eq!(
            options("identityagent ~/a.sock").identity_agent,
            IdentityAgent::Path("~/a.sock".into())
        );
    }

    #[test]
    fn tokens_expand_like_openssh() {
        let tokens = Tokens {
            host: "example.org".into(),
            port: 2222,
            remote_user: "deploy".into(),
            original_host: "ex".into(),
            local_user: "me".into(),
            home: "/home/me".into(),
            local_host: "laptop.lan".into(),
            jump: String::new(),
            uid: "1000".into(),
        };
        let env = |name: &str| (name == "KEYS").then(|| "/keys".to_owned());
        let at = |p: &str| expand_tokens(p, &tokens, &env);
        assert_eq!(
            at("%d/.ssh/%r@%h:%p").as_deref(),
            Some("/home/me/.ssh/deploy@example.org:2222")
        );
        assert_eq!(at("${KEYS}/%n_%u").as_deref(), Some("/keys/ex_me"));
        assert_eq!(
            at("%l-%L-%i-100%%").as_deref(),
            Some("laptop.lan-laptop-1000-100%")
        );
        assert_eq!(at("${UNSET}/k"), None);
        assert_eq!(at("%Z"), None);
        assert_eq!(at("/plain/path").as_deref(), Some("/plain/path"));
        // SHA-1 of "laptop.lanexample.org2222deploy".
        assert_eq!(at("%C").as_deref(), Some(tokens.connection_hash().as_str()));
        assert_eq!(tokens.connection_hash().len(), 40);
    }

    #[test]
    fn only_runtime_match_criteria_need_openssh() {
        let m = |line: &str| match_needs_openssh(&split_words(line));
        assert!(!m("host a,b user root"));
        assert!(!m("all"));
        assert!(!m("originalhost exec"));
        assert!(!m("host \"x y\" localuser me"));
        assert!(m("exec \"test -f /tmp/x\""));
        assert!(m("host a !exec \"true\""));
        assert!(m("canonical host *.lan"));
        assert!(m("final all"));
        assert!(m("localnetwork 10.0.0.0/8"));
        assert!(m("tagged work"));
    }

    fn test_key() -> PublicKey {
        PublicKey::from_openssh(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl",
        )
        .unwrap()
    }

    #[test]
    fn strict_host_key_checking_refuses_unknown_keys() {
        let memory: Arc<dyn KnownHosts> = Arc::new(easyssh_ssh::MemoryKnownHosts::new());
        let strict = with_policy(memory.clone(), HostKeyPolicy::Strict);
        let error = strict.check("h", 22, &test_key()).unwrap_err();
        assert!(
            matches!(error, easyssh_ssh::Error::HostKeyRejected { .. }),
            "{error}"
        );
        memory.add("h", 22, &test_key()).unwrap();
        assert_eq!(
            strict.check("h", 22, &test_key()).unwrap(),
            KnownHostStatus::Trusted
        );
    }

    #[test]
    fn accept_new_still_asks_the_user() {
        let memory: Arc<dyn KnownHosts> = Arc::new(easyssh_ssh::MemoryKnownHosts::new());
        let accept = with_policy(memory.clone(), HostKeyPolicy::AcceptNew);
        assert_eq!(
            accept.check("h", 22, &test_key()).unwrap(),
            KnownHostStatus::Unknown
        );
        // Nothing was recorded behind the user's back.
        assert_eq!(
            memory.check("h", 22, &test_key()).unwrap(),
            KnownHostStatus::Unknown
        );
    }

    #[test]
    fn words_keep_quoted_spaces() {
        assert_eq!(
            split_words("Include \"~/My Configs/*.conf\"  other"),
            vec!["Include", "~/My Configs/*.conf", "other"]
        );
    }

    #[test]
    fn globs_match_star_and_question_mark() {
        assert!(glob_match("*.conf", "a.conf"));
        assert!(glob_match("h?st-*-*.conf", "host-a-b.conf"));
        assert!(!glob_match("h?st-*-*.conf", "host-a.conf"));
        assert!(glob_match("*", "x"));
        assert!(!glob_match("*", ".hidden"));
        assert!(glob_match(".*", ".hidden"));
        assert!(!glob_match("a?", "a"));
    }

    #[test]
    fn glob_paths_walk_wildcard_directories() {
        let dir = tempfile::tempdir().unwrap();
        for file in ["d1/a.conf", "d2/b.conf", "d2/c.txt", "e/d.conf"] {
            let path = dir.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        assert_eq!(
            glob_paths(&dir.path().join("d?/*.conf")),
            vec![dir.path().join("d1/a.conf"), dir.path().join("d2/b.conf")]
        );
    }
}
