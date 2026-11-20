//! Finding the SSH agent: which one to try, in what order, and how to reach
//! it. On Windows this includes PuTTY's Pageant next to the OpenSSH agent.

use crate::params::AgentSource;
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;
use russh::keys::HashAlg;
use std::ffi::OsString;
use std::path::PathBuf;

pub(crate) type DynAgent = AgentClient<Box<dyn AgentStream + Send + Unpin>>;

/// The Microsoft OpenSSH agent's named pipe.
#[cfg(windows)]
const OPENSSH_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

/// Which kind of agent answered, for the settings page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// `IdentityAgent` from the host's configuration.
    Configured,
    /// The socket or pipe `SSH_AUTH_SOCK` names.
    Environment,
    /// The Windows OpenSSH agent pipe.
    OpenSsh,
    /// PuTTY's Pageant (Windows only).
    Pageant,
}

impl AgentKind {
    /// Stable identifier shared with the frontend.
    pub fn id(self) -> &'static str {
        match self {
            AgentKind::Configured => "configured",
            AgentKind::Environment => "environment",
            AgentKind::OpenSsh => "openssh",
            AgentKind::Pageant => "pageant",
        }
    }
}

/// One place to look for an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    Configured(PathBuf),
    Environment(OsString),
    #[cfg_attr(not(windows), allow(dead_code))]
    OpenSshPipe,
    #[cfg_attr(not(windows), allow(dead_code))]
    Pageant,
}

impl Target {
    pub(crate) fn kind(&self) -> AgentKind {
        match self {
            Target::Configured(_) => AgentKind::Configured,
            Target::Environment(_) => AgentKind::Environment,
            Target::OpenSshPipe => AgentKind::OpenSsh,
            Target::Pageant => AgentKind::Pageant,
        }
    }
}

/// The agents to try, in order: the configured `IdentityAgent`, else
/// `SSH_AUTH_SOCK`, else on Windows the OpenSSH agent and then Pageant.
pub(crate) fn candidates(
    source: &AgentSource,
    auth_sock: Option<OsString>,
    windows: bool,
) -> Vec<Target> {
    match source {
        AgentSource::Disabled => Vec::new(),
        AgentSource::Path(path) => vec![Target::Configured(path.clone())],
        AgentSource::Environment => match auth_sock.filter(|value| !value.is_empty()) {
            Some(value) => vec![Target::Environment(value)],
            None if windows => vec![Target::OpenSshPipe, Target::Pageant],
            None => Vec::new(),
        },
    }
}

async fn connect_target(target: &Target) -> std::result::Result<DynAgent, String> {
    match target {
        #[cfg(unix)]
        Target::Configured(path) => AgentClient::connect_uds(path)
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        #[cfg(unix)]
        Target::Environment(path) => AgentClient::connect_uds(path)
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        #[cfg(windows)]
        Target::Configured(path) => AgentClient::connect_named_pipe(path)
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        #[cfg(windows)]
        Target::Environment(path) => AgentClient::connect_named_pipe(path)
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        #[cfg(windows)]
        Target::OpenSshPipe => AgentClient::connect_named_pipe(OPENSSH_PIPE)
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        // Pageant 0.75+ answers on a named pipe, older versions through a
        // window message; the `pageant` crate behind russh handles both.
        #[cfg(windows)]
        Target::Pageant => AgentClient::connect_pageant()
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        #[cfg(not(any(unix, windows)))]
        _ => Err("agents are not supported on this platform".into()),
        #[cfg(unix)]
        _ => Err("this agent is only available on Windows".into()),
    }
}

/// Connects to the first agent that answers. `Err` lists why each one failed.
pub(crate) async fn connect(
    source: &AgentSource,
) -> std::result::Result<(DynAgent, AgentKind), String> {
    let list = candidates(source, std::env::var_os("SSH_AUTH_SOCK"), cfg!(windows));
    if list.is_empty() {
        return Err(match source {
            AgentSource::Disabled => "agent disabled".into(),
            _ => "SSH_AUTH_SOCK is not set".into(),
        });
    }
    let mut reasons = Vec::new();
    for target in &list {
        match connect_target(target).await {
            Ok(client) => return Ok((client, target.kind())),
            Err(reason) => reasons.push(format!("{}: {reason}", target.kind().id())),
        }
    }
    Err(reasons.join("; "))
}

/// What a look at the agent found.
#[derive(Debug, Clone, Default)]
pub struct AgentProbe {
    /// The agent that answered, if any.
    pub kind: Option<AgentKind>,
    /// `SHA256:... (ALGORITHM) comment` for each plain key it holds.
    pub keys: Vec<String>,
    /// Why no agent could be used.
    pub error: Option<String>,
}

/// Looks for an agent the way a login would and lists its keys. Used by the
/// settings page; changes nothing.
pub async fn probe_agent(source: &AgentSource) -> AgentProbe {
    let (mut client, kind) = match connect(source).await {
        Ok(found) => found,
        Err(error) => {
            return AgentProbe {
                kind: None,
                keys: Vec::new(),
                error: Some(error),
            }
        }
    };
    match client.request_identities().await {
        Ok(identities) => AgentProbe {
            kind: Some(kind),
            keys: identities
                .iter()
                .filter_map(|identity| match identity {
                    AgentIdentity::PublicKey { key, comment } => Some(format!(
                        "{} ({}) {}",
                        key.fingerprint(HashAlg::Sha256),
                        key.algorithm(),
                        comment
                    )),
                    _ => None,
                })
                .collect(),
            error: None,
        },
        Err(e) => AgentProbe {
            kind: Some(kind),
            keys: Vec::new(),
            error: Some(format!("agent did not list keys: {e}")),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sock(value: &str) -> Option<OsString> {
        Some(value.into())
    }

    #[test]
    fn disabled_looks_nowhere() {
        assert!(candidates(&AgentSource::Disabled, sock("/s"), true).is_empty());
    }

    #[test]
    fn identity_agent_wins_over_the_environment() {
        let source = AgentSource::Path("/cfg".into());
        assert_eq!(
            candidates(&source, sock("/env"), true),
            vec![Target::Configured("/cfg".into())]
        );
    }

    #[test]
    fn auth_sock_wins_over_windows_defaults() {
        assert_eq!(
            candidates(&AgentSource::Environment, sock("/env"), true),
            vec![Target::Environment("/env".into())]
        );
    }

    #[test]
    fn windows_tries_openssh_then_pageant() {
        assert_eq!(
            candidates(&AgentSource::Environment, None, true),
            vec![Target::OpenSshPipe, Target::Pageant]
        );
        assert_eq!(
            candidates(&AgentSource::Environment, sock(""), true),
            vec![Target::OpenSshPipe, Target::Pageant]
        );
    }

    #[test]
    fn unix_without_auth_sock_has_no_agent() {
        assert!(candidates(&AgentSource::Environment, None, false).is_empty());
    }
}
