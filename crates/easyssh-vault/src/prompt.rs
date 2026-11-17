//! Which OpenSSH prompts the vault may answer.
//!
//! OpenSSH asks through `SSH_ASKPASS` for passwords, key passphrases, host
//! key confirmations, two-factor codes and anything a server's
//! keyboard-interactive step makes up. The vault answers exactly two kinds,
//! and only when the prompt names the host and user the secret was saved for:
//!
//! ```text
//! alice@example.test's password:            (password authentication)
//! (alice@example.test) Password:            (keyboard-interactive, same secret)
//! Enter passphrase for key '/path/to/key':  (key passphrase)
//! ```
//!
//! Everything else, in particular `Are you sure you want to continue
//! connecting (yes/no...)`, goes to the user. A prompt is matched as a whole
//! and case-sensitively for the user; there is no "contains".

use easyssh_core::{Connection, ConnectionTarget};

/// What a prompt asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretKind {
    Password,
    KeyPassphrase,
}

/// The one host a process' prompts may be answered for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub host_id: String,
    /// The user in the prompt `user@host's password:`. `None` when the host
    /// has no explicit user (or is a `~/.ssh/config` alias): password
    /// prompts then are never answered.
    pub user: Option<String>,
    /// The host name as `ssh` prints it.
    pub hostname: Option<String>,
    /// The key file this process was started with, when the vault wrote one.
    /// Passphrase prompts must then name exactly this path.
    pub key_path: Option<String>,
    /// Whether the vault holds a passphrase for the key (the daemon does not
    /// look it up until a matching prompt arrives).
    pub passphrase_expected: bool,
}

impl Binding {
    /// The binding for a saved host. `key_path` is the temporary key file the
    /// session was started with, if any.
    pub fn for_connection(connection: &Connection, key_path: Option<String>) -> Self {
        let (user, hostname) = match &connection.target {
            ConnectionTarget::Endpoint {
                hostname, username, ..
            } => (username.clone(), Some(hostname.clone())),
            ConnectionTarget::Alias { .. } => (None, None),
        };
        Binding {
            host_id: connection.id.clone(),
            user: user.filter(|u| !u.is_empty()),
            hostname: hostname.filter(|h| !h.is_empty()),
            key_path,
            passphrase_expected: true,
        }
    }
}

const MAX_PROMPT: usize = 512;

fn plain(text: &str) -> bool {
    text.len() <= MAX_PROMPT && !text.chars().any(|c| c.is_control())
}

/// Classifies `prompt` for `binding`; `None` means "do not answer".
pub fn match_prompt(binding: &Binding, prompt: &str) -> Option<SecretKind> {
    // Strip OpenSSH's trailing blank and only that.
    let text = prompt.strip_suffix(' ').unwrap_or(prompt);
    if !plain(text) || text.contains("(yes/no") {
        return None;
    }
    if let (Some(user), Some(host)) = (&binding.user, &binding.hostname) {
        let ssh = format!("{user}@{host}'s password:");
        let kbd = format!("({user}@{host}) Password:");
        if text == ssh || text == kbd {
            return Some(SecretKind::Password);
        }
    }
    let path = text
        .strip_prefix("Enter passphrase for key '")?
        .strip_suffix("':")?;
    if path.is_empty() {
        return None;
    }
    match &binding.key_path {
        Some(expected) if expected != path => None,
        _ if !binding.passphrase_expected => None,
        _ => Some(SecretKind::KeyPassphrase),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding() -> Binding {
        Binding {
            host_id: "h1".into(),
            user: Some("alice".into()),
            hostname: Some("example.test".into()),
            key_path: None,
            passphrase_expected: true,
        }
    }

    #[test]
    fn answers_the_exact_password_prompts() {
        let b = binding();
        assert_eq!(
            match_prompt(&b, "alice@example.test's password: "),
            Some(SecretKind::Password)
        );
        assert_eq!(
            match_prompt(&b, "alice@example.test's password:"),
            Some(SecretKind::Password)
        );
        assert_eq!(
            match_prompt(&b, "(alice@example.test) Password: "),
            Some(SecretKind::Password)
        );
    }

    #[test]
    fn refuses_other_users_hosts_and_decorations() {
        let b = binding();
        for prompt in [
            "bob@example.test's password: ",
            "alice@other.test's password: ",
            "alice@example.test.evil's password: ",
            "xalice@example.test's password: ",
            "Alice@example.test's password: ",
            "Password: ",
            "Verification code: ",
            "alice@example.test's password: \nsecond line",
            "please enter alice@example.test's password: ",
            "alice@example.test's password: and more",
            "(alice@example.test) Password: (yes/no)",
            "",
        ] {
            assert_eq!(match_prompt(&b, prompt), None, "{prompt:?}");
        }
    }

    #[test]
    fn never_answers_a_host_key_question() {
        let b = binding();
        for prompt in [
            "Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
            "The authenticity of host 'example.test (1.2.3.4)' can't be established.\nED25519 key fingerprint is SHA256:abc.\nAre you sure you want to continue connecting (yes/no/[fingerprint])? ",
            "alice@example.test's password: (yes/no)",
        ] {
            assert_eq!(match_prompt(&b, prompt), None);
        }
    }

    #[test]
    fn password_prompts_need_an_explicit_user_and_host() {
        let mut b = binding();
        b.user = None;
        assert_eq!(match_prompt(&b, "alice@example.test's password: "), None);
        b.user = Some("alice".into());
        b.hostname = None;
        assert_eq!(match_prompt(&b, "alice@example.test's password: "), None);
    }

    #[test]
    fn passphrase_prompts() {
        let mut b = binding();
        let prompt = "Enter passphrase for key '/tmp/run/key-1': ";
        assert_eq!(match_prompt(&b, prompt), Some(SecretKind::KeyPassphrase));
        b.key_path = Some("/tmp/run/key-1".into());
        assert_eq!(match_prompt(&b, prompt), Some(SecretKind::KeyPassphrase));
        // A different key file than the one this process was given.
        b.key_path = Some("/tmp/run/key-2".into());
        assert_eq!(match_prompt(&b, prompt), None);
        b.key_path = None;
        b.passphrase_expected = false;
        assert_eq!(match_prompt(&b, prompt), None);
        b.passphrase_expected = true;
        assert_eq!(match_prompt(&b, "Enter passphrase for key '': "), None);
        assert_eq!(
            match_prompt(&b, "Enter passphrase for key '/a': extra"),
            None
        );
        assert_eq!(match_prompt(&b, "Enter PIN for token: "), None);
    }

    #[test]
    fn binding_from_connections() {
        let alias = Connection::alias("x", "prod");
        let b = Binding::for_connection(&alias, None);
        assert!(b.user.is_none() && b.hostname.is_none());
        let mut ep = Connection::alias("x", "y");
        ep.target = ConnectionTarget::Endpoint {
            hostname: "h".into(),
            username: Some("u".into()),
            port: 22,
        };
        let b = Binding::for_connection(&ep, Some("/k".into()));
        assert_eq!(b.user.as_deref(), Some("u"));
        assert_eq!(b.hostname.as_deref(), Some("h"));
        assert_eq!(b.key_path.as_deref(), Some("/k"));
        assert_eq!(b.host_id, ep.id);
    }
}
