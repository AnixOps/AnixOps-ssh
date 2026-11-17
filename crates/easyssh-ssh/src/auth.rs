//! The authentication chain: agent, identity files, keyboard-interactive,
//! password.

use crate::error::{AuthMethod, Error, Result};
use crate::interaction::{Interaction, KbdPrompt};
use crate::params::{AgentSource, ConnectParams};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::client::{AgentClient, AgentStream};
use russh::keys::agent::AgentIdentity;
use russh::keys::{PrivateKey, PrivateKeyWithHashAlg};
use russh::MethodKind;
use std::path::Path;
use std::sync::Arc;
use zeroize::Zeroizing;

/// One step of the chain, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Agent,
    IdentityFiles,
    KeyboardInteractive,
    Password,
}

impl Stage {
    pub const ORDER: [Stage; 4] = [
        Stage::Agent,
        Stage::IdentityFiles,
        Stage::KeyboardInteractive,
        Stage::Password,
    ];

    fn method(self) -> MethodKind {
        match self {
            Stage::Agent | Stage::IdentityFiles => MethodKind::PublicKey,
            Stage::KeyboardInteractive => MethodKind::KeyboardInteractive,
            Stage::Password => MethodKind::Password,
        }
    }
}

/// Which stages the configuration enables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageConfig {
    pub agent: bool,
    pub identity_files: bool,
    pub keyboard_interactive: bool,
    pub password: bool,
}

impl StageConfig {
    pub fn from_params(params: &ConnectParams) -> Self {
        Self {
            agent: params.agent != AgentSource::Disabled,
            identity_files: !params.identity_files.is_empty()
                || !params.memory_identities.is_empty(),
            keyboard_interactive: params.keyboard_interactive,
            password: params.password,
        }
    }

    fn enabled(&self, stage: Stage) -> bool {
        match stage {
            Stage::Agent => self.agent,
            Stage::IdentityFiles => self.identity_files,
            Stage::KeyboardInteractive => self.keyboard_interactive,
            Stage::Password => self.password,
        }
    }
}

/// The stages to run, in order, given what the server still accepts.
pub fn plan(config: &StageConfig, server_methods: &[MethodKind]) -> Vec<Stage> {
    Stage::ORDER
        .into_iter()
        .filter(|stage| config.enabled(*stage) && server_methods.contains(&stage.method()))
        .collect()
}

/// The type of key an OpenSSH private key file holds when it is a FIDO2
/// `sk-*` key, which the engine cannot use. The public part of the key is
/// stored unencrypted, so this works for passphrase-protected files too.
pub fn security_key_type(text: &str) -> Option<&'static str> {
    const TYPES: [&str; 2] = [
        "sk-ssh-ed25519@openssh.com",
        "sk-ecdsa-sha2-nistp256@openssh.com",
    ];
    let body: String = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("-----"))
        .collect();
    let bytes = B64.decode(body).ok()?;
    TYPES.into_iter().find(|name| {
        bytes
            .windows(name.len())
            .any(|window| window == name.as_bytes())
    })
}

const PASSPHRASE_ATTEMPTS: usize = 3;

/// Loads a private key file, asking `interaction` for the passphrase when it
/// is encrypted.
pub async fn load_identity(path: &Path, interaction: &dyn Interaction) -> Result<PrivateKey> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| Error::Protocol(format!("cannot read {}: {e}", path.display())))?;
    decode_identity(&text, path, None, interaction).await
}

/// Decodes key text that is already in memory. `path` only names the key in
/// prompts and errors; it is never opened. `saved_passphrase` is tried first for an
/// encrypted key; when it is missing or wrong, `interaction` is asked.
pub async fn decode_identity(
    text: &str,
    path: &Path,
    saved_passphrase: Option<&str>,
    interaction: &dyn Interaction,
) -> Result<PrivateKey> {
    // Copies of the key and passphrase that cross to the blocking thread are
    // wiped when dropped.
    let text = Zeroizing::new(text.to_owned());
    if let Some(kind) = security_key_type(&text) {
        return Err(Error::Unsupported(format!(
            "{} is a FIDO2 security key ({kind}); sk-* keys are not supported by this engine yet",
            path.display()
        )));
    }
    let attempt = |text: Zeroizing<String>, passphrase: Option<Zeroizing<String>>| async move {
        tokio::task::spawn_blocking(move || {
            russh::keys::decode_secret_key(&text, passphrase.as_ref().map(|p| p.as_str()))
        })
        .await
        .map_err(|e| Error::Protocol(format!("key decoding task failed: {e}")))
    };
    match attempt(text.clone(), None).await? {
        Ok(key) => return Ok(key),
        Err(russh::keys::Error::KeyIsEncrypted) => {}
        Err(other) => {
            return Err(Error::Protocol(format!(
                "cannot use {}: {other}",
                path.display()
            )))
        }
    }
    if let Some(saved) = saved_passphrase {
        if let Ok(key) = attempt(text.clone(), Some(Zeroizing::new(saved.to_owned()))).await? {
            return Ok(key);
        }
    }
    for _ in 0..PASSPHRASE_ATTEMPTS {
        let Some(passphrase) = interaction.passphrase(path).await else {
            return Err(Error::Cancelled(format!(
                "no passphrase for {}",
                path.display()
            )));
        };
        if let Ok(key) = attempt(text.clone(), Some(Zeroizing::new(passphrase))).await? {
            return Ok(key);
        }
    }
    Err(Error::Protocol(format!(
        "wrong passphrase for {}",
        path.display()
    )))
}

async fn connect_agent(
    source: &AgentSource,
) -> std::result::Result<AgentClient<Box<dyn AgentStream + Send + Unpin>>, String> {
    match source {
        AgentSource::Disabled => Err("agent disabled".into()),
        #[cfg(unix)]
        AgentSource::Environment => match std::env::var_os("SSH_AUTH_SOCK") {
            None => Err("SSH_AUTH_SOCK is not set".into()),
            Some(path) => AgentClient::connect_uds(path)
                .await
                .map(AgentClient::dynamic)
                .map_err(|e| e.to_string()),
        },
        #[cfg(unix)]
        AgentSource::Path(path) => AgentClient::connect_uds(path)
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        #[cfg(windows)]
        AgentSource::Environment => {
            let pipe = std::env::var_os("SSH_AUTH_SOCK")
                .unwrap_or_else(|| r"\\.\pipe\openssh-ssh-agent".into());
            AgentClient::connect_named_pipe(pipe)
                .await
                .map(AgentClient::dynamic)
                .map_err(|e| e.to_string())
        }
        #[cfg(windows)]
        AgentSource::Path(path) => AgentClient::connect_named_pipe(path)
            .await
            .map(AgentClient::dynamic)
            .map_err(|e| e.to_string()),
        #[cfg(not(any(unix, windows)))]
        _ => Err("agents are not supported on this platform".into()),
    }
}

/// Keeps `remaining` current from a refused attempt.
fn note_result(result: &AuthResult, remaining: &mut Vec<MethodKind>) -> bool {
    match result {
        AuthResult::Success => true,
        AuthResult::Failure {
            remaining_methods, ..
        } => {
            *remaining = remaining_methods.iter().copied().collect();
            false
        }
    }
}

/// Runs the chain and reports which method got in.
pub(crate) async fn authenticate<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
) -> Result<AuthMethod> {
    let user = params.username.as_str();
    let mut tried: Vec<AuthMethod> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    let first = handle.authenticate_none(user).await?;
    let mut remaining: Vec<MethodKind> = Vec::new();
    if note_result(&first, &mut remaining) {
        return Ok(AuthMethod::None);
    }

    let config = StageConfig::from_params(params);
    for stage in Stage::ORDER {
        if !config.enabled(stage) {
            continue;
        }
        if !remaining.contains(&stage.method()) {
            notes.push(format!("server does not offer {:?}", stage.method()));
            continue;
        }
        let outcome = match stage {
            Stage::Agent => {
                try_agent(handle, params, &mut remaining, &mut tried, &mut notes).await?
            }
            Stage::IdentityFiles => {
                try_identity_files(
                    handle,
                    params,
                    interaction,
                    &mut remaining,
                    &mut tried,
                    &mut notes,
                )
                .await?
            }
            Stage::KeyboardInteractive => {
                try_keyboard_interactive(
                    handle,
                    interaction,
                    user,
                    &mut remaining,
                    &mut tried,
                    &mut notes,
                )
                .await?
            }
            Stage::Password => {
                try_password(
                    handle,
                    params,
                    interaction,
                    &mut remaining,
                    &mut tried,
                    &mut notes,
                )
                .await?
            }
        };
        if let Some(method) = outcome {
            return Ok(method);
        }
    }
    Err(Error::AuthFailed { tried, notes })
}

async fn rsa_hash<H: client::Handler>(
    handle: &client::Handle<H>,
    is_rsa: bool,
) -> Result<Option<russh::keys::HashAlg>> {
    if !is_rsa {
        return Ok(None);
    }
    Ok(handle.best_supported_rsa_hash().await?.flatten())
}

async fn try_agent<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    remaining: &mut Vec<MethodKind>,
    tried: &mut Vec<AuthMethod>,
    notes: &mut Vec<String>,
) -> Result<Option<AuthMethod>> {
    let mut agent = match connect_agent(&params.agent).await {
        Ok(agent) => agent,
        Err(reason) => {
            notes.push(format!("agent unavailable: {reason}"));
            return Ok(None);
        }
    };
    let identities = match agent.request_identities().await {
        Ok(identities) => identities,
        Err(e) => {
            notes.push(format!("agent did not list keys: {e}"));
            return Ok(None);
        }
    };
    if identities.is_empty() {
        notes.push("agent holds no keys".into());
        return Ok(None);
    }
    for identity in identities {
        let AgentIdentity::PublicKey { key, .. } = identity else {
            continue; // certificates are not supported yet
        };
        if key.algorithm().as_str().starts_with("sk-") {
            notes.push("agent key skipped: FIDO2 sk-* keys are not supported".into());
            continue;
        }
        if !remaining.contains(&MethodKind::PublicKey) {
            break;
        }
        if !tried.contains(&AuthMethod::Agent) {
            tried.push(AuthMethod::Agent);
        }
        let hash = rsa_hash(handle, key.algorithm().is_rsa()).await?;
        match handle
            .authenticate_publickey_with(params.username.as_str(), key, hash, &mut agent)
            .await
        {
            Ok(result) => {
                if note_result(&result, remaining) {
                    return Ok(Some(AuthMethod::Agent));
                }
            }
            Err(e) => notes.push(format!("agent could not sign: {e}")),
        }
    }
    Ok(None)
}

async fn try_identity_files<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
    remaining: &mut Vec<MethodKind>,
    tried: &mut Vec<AuthMethod>,
    notes: &mut Vec<String>,
) -> Result<Option<AuthMethod>> {
    // Keys handed over in memory come first (a saved key is the one the
    // user chose for this host), then the files.
    for identity in &params.memory_identities {
        if !remaining.contains(&MethodKind::PublicKey) {
            break;
        }
        let key = match decode_identity(
            identity.text(),
            Path::new(&identity.label),
            identity.passphrase(),
            interaction,
        )
        .await
        {
            Ok(key) => key,
            Err(e) => {
                notes.push(e.to_string());
                continue;
            }
        };
        let method = AuthMethod::StoredKey(identity.label.clone());
        tried.push(method.clone());
        let hash = rsa_hash(handle, key.algorithm().is_rsa()).await?;
        let result = handle
            .authenticate_publickey(
                params.username.as_str(),
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await?;
        if note_result(&result, remaining) {
            return Ok(Some(method));
        }
    }
    for path in &params.identity_files {
        if !remaining.contains(&MethodKind::PublicKey) {
            break;
        }
        let key = match load_identity(path, interaction).await {
            Ok(key) => key,
            Err(e) => {
                notes.push(e.to_string());
                continue;
            }
        };
        let method = AuthMethod::IdentityFile(path.clone());
        tried.push(method.clone());
        let hash = rsa_hash(handle, key.algorithm().is_rsa()).await?;
        let result = handle
            .authenticate_publickey(
                params.username.as_str(),
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await?;
        if note_result(&result, remaining) {
            return Ok(Some(method));
        }
    }
    Ok(None)
}

const MAX_KBD_ROUNDS: usize = 8;

async fn try_keyboard_interactive<H: client::Handler>(
    handle: &mut client::Handle<H>,
    interaction: &dyn Interaction,
    user: &str,
    remaining: &mut Vec<MethodKind>,
    tried: &mut Vec<AuthMethod>,
    notes: &mut Vec<String>,
) -> Result<Option<AuthMethod>> {
    tried.push(AuthMethod::KeyboardInteractive);
    let mut response = handle
        .authenticate_keyboard_interactive_start(user, None)
        .await?;
    for _ in 0..MAX_KBD_ROUNDS {
        match response {
            KeyboardInteractiveAuthResponse::Success => {
                return Ok(Some(AuthMethod::KeyboardInteractive))
            }
            KeyboardInteractiveAuthResponse::Failure {
                remaining_methods, ..
            } => {
                *remaining = remaining_methods.iter().copied().collect();
                return Ok(None);
            }
            KeyboardInteractiveAuthResponse::InfoRequest {
                name,
                instructions,
                prompts,
            } => {
                let prompts: Vec<KbdPrompt> = prompts
                    .into_iter()
                    .map(|p| KbdPrompt {
                        text: p.prompt,
                        echo: p.echo,
                    })
                    .collect();
                let answers = if prompts.is_empty() {
                    Some(Vec::new())
                } else {
                    interaction
                        .keyboard_interactive(&name, &instructions, &prompts)
                        .await
                };
                let Some(answers) = answers else {
                    notes.push("keyboard-interactive cancelled".into());
                    return Ok(None);
                };
                response = handle
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
    notes.push("keyboard-interactive did not finish".into());
    Ok(None)
}

async fn try_password<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
    remaining: &mut Vec<MethodKind>,
    tried: &mut Vec<AuthMethod>,
    notes: &mut Vec<String>,
) -> Result<Option<AuthMethod>> {
    let prompt = format!("{}@{}'s password", params.username, params.host);
    for attempt in 0..params.password_attempts.max(1) {
        let Some(password) = interaction.password(&prompt).await else {
            notes.push("password prompt cancelled".into());
            break;
        };
        if attempt == 0 {
            tried.push(AuthMethod::Password);
        }
        let result = handle
            .authenticate_password(params.username.as_str(), password)
            .await?;
        if note_result(&result, remaining) {
            return Ok(Some(AuthMethod::Password));
        }
        if !remaining.contains(&MethodKind::Password) {
            break;
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interaction::DenyAll;
    use std::io::Write;

    const ALL: [MethodKind; 4] = [
        MethodKind::PublicKey,
        MethodKind::KeyboardInteractive,
        MethodKind::Password,
        MethodKind::HostBased,
    ];

    fn config(identity_files: bool) -> StageConfig {
        StageConfig {
            agent: true,
            identity_files,
            keyboard_interactive: true,
            password: true,
        }
    }

    #[test]
    fn stages_run_in_the_documented_order() {
        assert_eq!(
            plan(&config(true), &ALL),
            vec![
                Stage::Agent,
                Stage::IdentityFiles,
                Stage::KeyboardInteractive,
                Stage::Password
            ]
        );
    }

    #[test]
    fn stages_follow_what_the_server_offers() {
        assert_eq!(
            plan(&config(true), &[MethodKind::Password]),
            vec![Stage::Password]
        );
        assert_eq!(
            plan(&config(true), &[MethodKind::PublicKey]),
            vec![Stage::Agent, Stage::IdentityFiles]
        );
        assert!(plan(&config(true), &[]).is_empty());
    }

    #[test]
    fn disabled_stages_are_left_out() {
        let config = StageConfig {
            agent: false,
            identity_files: false,
            keyboard_interactive: false,
            password: true,
        };
        assert_eq!(plan(&config, &ALL), vec![Stage::Password]);
    }

    #[test]
    fn config_comes_from_params() {
        let mut params = ConnectParams::new("h", 22, "u");
        assert_eq!(
            plan(&StageConfig::from_params(&params), &ALL),
            vec![Stage::Agent, Stage::KeyboardInteractive, Stage::Password]
        );
        params.agent = AgentSource::Disabled;
        params.identity_files.push("k".into());
        params.password = false;
        assert_eq!(
            plan(&StageConfig::from_params(&params), &ALL),
            vec![Stage::IdentityFiles, Stage::KeyboardInteractive]
        );
    }

    fn fake_sk_key(name: &str) -> String {
        let mut bytes = b"openssh-key-v1\0".to_vec();
        bytes.extend_from_slice(&(name.len() as u32).to_be_bytes());
        bytes.extend_from_slice(name.as_bytes());
        format!(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n{}\n-----END OPENSSH PRIVATE KEY-----\n",
            B64.encode(bytes)
        )
    }

    #[test]
    fn detects_fido2_keys() {
        assert_eq!(
            security_key_type(&fake_sk_key("sk-ssh-ed25519@openssh.com")),
            Some("sk-ssh-ed25519@openssh.com")
        );
        assert_eq!(
            security_key_type(&fake_sk_key("sk-ecdsa-sha2-nistp256@openssh.com")),
            Some("sk-ecdsa-sha2-nistp256@openssh.com")
        );
        assert_eq!(security_key_type(&fake_sk_key("ssh-ed25519")), None);
        assert_eq!(security_key_type("not a key"), None);
    }

    #[tokio::test]
    async fn fido2_identity_gives_a_clear_unsupported_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id_ed25519_sk");
        std::fs::write(&path, fake_sk_key("sk-ssh-ed25519@openssh.com")).unwrap();
        let error = load_identity(&path, &DenyAll).await.unwrap_err();
        assert!(matches!(error, Error::Unsupported(_)), "{error:?}");
        assert!(error.to_string().contains("FIDO2"), "{error}");
    }

    // `ssh-keygen -t ed25519 -N 'pass phrase'`, test only.
    const ENCRYPTED: &str = include_str!("../tests/fixtures/encrypted_ed25519");

    struct Passphrase(&'static str, std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl Interaction for Passphrase {
        async fn verify_host_key(
            &self,
            _: &str,
            _: u16,
            _: &str,
            _: crate::known_hosts::KnownHostStatus,
        ) -> crate::interaction::Decision {
            crate::interaction::Decision::Reject
        }
        async fn password(&self, _: &str) -> Option<String> {
            None
        }
        async fn keyboard_interactive(
            &self,
            _: &str,
            _: &str,
            _: &[KbdPrompt],
        ) -> Option<Vec<String>> {
            None
        }
        async fn passphrase(&self, _: &Path) -> Option<String> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(self.0.to_owned())
        }
    }

    // `ssh-keygen -t ed25519 -N ''`, test only.
    const PLAIN: &str = include_str!("../tests/fixtures/plain_ed25519");

    #[tokio::test]
    async fn an_unencrypted_key_in_memory_decodes_without_asking() {
        let asked = Passphrase("never", Default::default());
        let key = decode_identity(PLAIN, Path::new("saved key"), None, &asked)
            .await
            .unwrap();
        assert!(key.algorithm().is_ed25519());
        assert_eq!(asked.1.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_saved_passphrase_unlocks_a_key_in_memory_without_asking() {
        let asked = Passphrase("not used", Default::default());
        let key = decode_identity(
            ENCRYPTED,
            Path::new("saved key"),
            Some("pass phrase"),
            &asked,
        )
        .await
        .unwrap();
        assert!(key.algorithm().is_ed25519());
        assert_eq!(asked.1.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_wrong_saved_passphrase_falls_back_to_asking_the_user() {
        let asked = Passphrase("pass phrase", Default::default());
        let key = decode_identity(ENCRYPTED, Path::new("saved key"), Some("stale"), &asked)
            .await
            .unwrap();
        assert!(key.algorithm().is_ed25519());
        assert_eq!(asked.1.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Nobody to ask: cancelled, not a crash and not a hang.
        let error = decode_identity(ENCRYPTED, Path::new("saved key"), Some("stale"), &DenyAll)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Cancelled(_)), "{error:?}");
        assert!(error.to_string().contains("saved key"), "{error}");
    }

    #[tokio::test]
    async fn a_bad_key_in_memory_is_an_error_that_names_the_label_only() {
        let error = decode_identity("not a key", Path::new("saved key"), None, &DenyAll)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Protocol(_)), "{error:?}");
        assert!(error.to_string().contains("saved key"), "{error}");
        assert!(!error.to_string().contains("not a key"), "{error}");
    }

    #[test]
    fn memory_identities_enable_the_key_stage_and_stay_out_of_debug_output() {
        let mut params = ConnectParams::new("h", 22, "u");
        params.agent = AgentSource::Disabled;
        params.keyboard_interactive = false;
        params.password = false;
        assert!(plan(&StageConfig::from_params(&params), &ALL).is_empty());
        let params = params.memory_identity(crate::MemoryIdentity::new(
            "saved key",
            PLAIN,
            Some("hunter2".into()),
        ));
        assert_eq!(
            plan(&StageConfig::from_params(&params), &ALL),
            vec![Stage::IdentityFiles]
        );
        let shown = format!("{params:?}");
        assert!(shown.contains("saved key"), "{shown}");
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(!shown.contains("BEGIN OPENSSH"), "{shown}");
        assert!(!shown.contains("b3BlbnNzaC1r"), "{shown}");
    }

    #[tokio::test]
    async fn encrypted_identity_asks_for_the_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(ENCRYPTED.as_bytes())
            .unwrap();

        let right = Passphrase("pass phrase", Default::default());
        let key = load_identity(&path, &right).await.unwrap();
        assert!(key.algorithm().is_ed25519());
        assert_eq!(right.1.load(std::sync::atomic::Ordering::SeqCst), 1);

        let wrong = Passphrase("nope", Default::default());
        let error = load_identity(&path, &wrong).await.unwrap_err();
        assert!(error.to_string().contains("wrong passphrase"), "{error}");
        assert_eq!(wrong.1.load(std::sync::atomic::Ordering::SeqCst), 3);

        let error = load_identity(&path, &DenyAll).await.unwrap_err();
        assert!(matches!(error, Error::Cancelled(_)), "{error:?}");
    }
}
