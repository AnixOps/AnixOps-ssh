//! Which engine runs a host: the built-in russh engine or the system
//! OpenSSH. The decision is one pure function, [`choose_engine`], fed with
//! the global setting, the per-host override and what the host needs
//! ([`Unsupported`]), so the rules are easy to test and to read.

use easyssh_core::{Connection, EnginePreference, SshEngine};

/// Something a host needs that the russh engine does not provide, so the
/// host runs on OpenSSH instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Unsupported {
    ProxyCommand,
    Match,
    LocalCommand,
    /// A user certificate (`CertificateFile` or an `-cert.pub` next to a key).
    Certificate,
    /// A FIDO2 `sk-*` key.
    SecurityKey,
    /// A `RemoteCommand`, or a command stored with the host.
    RemoteCommand,
    /// `-L`, `-R` or `-D` stored with the host or in the ssh config.
    InlineForward,
}

impl Unsupported {
    pub fn describe(self) -> &'static str {
        match self {
            Unsupported::ProxyCommand => "ProxyCommand",
            Unsupported::Match => "a Match block in the ssh config",
            Unsupported::LocalCommand => "LocalCommand",
            Unsupported::Certificate => "certificate authentication",
            Unsupported::SecurityKey => "a FIDO2 security key (sk-*)",
            Unsupported::RemoteCommand => "a remote command",
            Unsupported::InlineForward => "port forwards stored with the host",
        }
    }
}

/// The engine and why, in English, for a tooltip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub engine: SshEngine,
    pub reason: String,
}

impl Choice {
    fn new(engine: SshEngine, reason: impl Into<String>) -> Self {
        Choice {
            engine,
            reason: reason.into(),
        }
    }
}

fn list(found: &[Unsupported]) -> String {
    let mut sorted = found.to_vec();
    sorted.sort();
    sorted.dedup();
    sorted
        .iter()
        .map(|item| item.describe())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The rule.
///
/// - A host set to `openssh`, or `auto` with the global setting `openssh`,
///   runs on OpenSSH.
/// - Otherwise russh is wanted (global `russh`, or the host says `russh`).
///   Anything the host needs that russh lacks (`found`) sends it to OpenSSH
///   all the same, even when the host asked for russh explicitly: the
///   connection would silently ignore that feature otherwise.
/// - A russh connection that later fails with `Error::Unsupported` also falls
///   back; see [`fallback_reason`].
pub fn choose_engine(
    global: SshEngine,
    preference: EnginePreference,
    found: &[Unsupported],
) -> Choice {
    match (preference, global) {
        (EnginePreference::Openssh, _) => {
            return Choice::new(SshEngine::Openssh, "this host is set to use OpenSSH")
        }
        (EnginePreference::Auto, SshEngine::Openssh) => {
            return Choice::new(SshEngine::Openssh, "OpenSSH is selected in the settings")
        }
        _ => {}
    }
    if !found.is_empty() {
        return Choice::new(
            SshEngine::Openssh,
            format!("OpenSSH runs this host because it uses {}", list(found)),
        );
    }
    match preference {
        EnginePreference::Russh => Choice::new(
            SshEngine::Russh,
            "this host is set to use the built-in engine",
        ),
        _ => Choice::new(
            SshEngine::Russh,
            "the built-in engine is selected in the settings",
        ),
    }
}

/// The reason shown when a russh connection fell back to OpenSSH at connect
/// time because the engine reported something unsupported.
pub fn fallback_reason(detail: &str) -> String {
    format!("OpenSSH runs this host because the built-in engine does not support it: {detail}")
}

/// Whether the global setting or the host's own setting wants russh, which is
/// when the (slow) look at the ssh config is worth doing.
pub fn wants_russh(global: SshEngine, preference: EnginePreference) -> bool {
    match preference {
        EnginePreference::Openssh => false,
        EnginePreference::Russh => true,
        EnginePreference::Auto => global == SshEngine::Russh,
    }
}

/// The things a saved host needs on its own, without looking at the ssh
/// config: a stored remote command and stored forwards are arguments of the
/// `ssh` command line, which the russh engine does not build.
pub fn unsupported_by_connection(connection: &Connection) -> Vec<Unsupported> {
    let mut found = Vec::new();
    if connection.remote_command.is_some() {
        found.push(Unsupported::RemoteCommand);
    }
    if !connection.local_forwards.is_empty()
        || !connection.remote_forwards.is_empty()
        || !connection.dynamic_forwards.is_empty()
    {
        found.push(Unsupported::InlineForward);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use EnginePreference as P;
    use SshEngine as G;

    #[test]
    fn the_global_setting_decides_for_auto_hosts() {
        let c = choose_engine(G::Openssh, P::Auto, &[]);
        assert_eq!(c.engine, G::Openssh);
        assert!(c.reason.contains("settings"), "{}", c.reason);
        let c = choose_engine(G::Russh, P::Auto, &[]);
        assert_eq!(c.engine, G::Russh);
    }

    #[test]
    fn a_host_override_wins_over_the_global_setting() {
        assert_eq!(choose_engine(G::Russh, P::Openssh, &[]).engine, G::Openssh);
        assert_eq!(choose_engine(G::Openssh, P::Russh, &[]).engine, G::Russh);
        let c = choose_engine(G::Openssh, P::Russh, &[]);
        assert!(c.reason.contains("this host"), "{}", c.reason);
    }

    #[test]
    fn every_unsupported_feature_falls_back_to_openssh() {
        for item in [
            Unsupported::ProxyCommand,
            Unsupported::Match,
            Unsupported::LocalCommand,
            Unsupported::Certificate,
            Unsupported::SecurityKey,
            Unsupported::RemoteCommand,
            Unsupported::InlineForward,
        ] {
            for preference in [P::Auto, P::Russh] {
                let c = choose_engine(G::Russh, preference, &[item]);
                assert_eq!(c.engine, G::Openssh, "{item:?} with {preference:?}");
                assert!(c.reason.contains(item.describe()), "{}", c.reason);
            }
        }
    }

    #[test]
    fn the_reason_names_everything_once() {
        let c = choose_engine(
            G::Russh,
            P::Auto,
            &[
                Unsupported::Match,
                Unsupported::ProxyCommand,
                Unsupported::Match,
            ],
        );
        assert_eq!(
            c.reason,
            "OpenSSH runs this host because it uses ProxyCommand, a Match block in the ssh config"
        );
    }

    #[test]
    fn openssh_stays_openssh_whatever_the_host_needs() {
        let c = choose_engine(G::Openssh, P::Auto, &[Unsupported::ProxyCommand]);
        assert_eq!(c.engine, G::Openssh);
        let c = choose_engine(G::Russh, P::Openssh, &[Unsupported::ProxyCommand]);
        assert!(c.reason.contains("set to use OpenSSH"));
    }

    #[test]
    fn only_wanting_russh_needs_the_config_look() {
        assert!(!wants_russh(G::Openssh, P::Auto));
        assert!(wants_russh(G::Russh, P::Auto));
        assert!(!wants_russh(G::Russh, P::Openssh));
        assert!(wants_russh(G::Openssh, P::Russh));
    }

    #[test]
    fn stored_commands_and_forwards_are_openssh_only() {
        let mut c = Connection::alias("x", "x");
        assert!(unsupported_by_connection(&c).is_empty());
        c.remote_command = Some("tmux".into());
        c.local_forwards = vec!["8080:localhost:80".into()];
        assert_eq!(
            unsupported_by_connection(&c),
            vec![Unsupported::RemoteCommand, Unsupported::InlineForward]
        );
    }

    #[test]
    fn a_connect_time_fallback_says_why() {
        let reason = fallback_reason("host certificates");
        assert!(reason.contains("host certificates"));
    }
}
