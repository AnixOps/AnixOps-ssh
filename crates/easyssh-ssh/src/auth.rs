//! The authentication chain: public keys (agent and identity files, in
//! OpenSSH's order), keyboard-interactive, password. After a partial success
//! (the server wants a further method) the remaining methods run again.

use crate::agent::DynAgent;
use crate::error::{AuthMethod, Error, Result};
use crate::interaction::{Interaction, KbdPrompt};
use crate::params::{AgentSource, ConnectParams, MemoryIdentity};
use crate::security_key::{default_security_key, SkKeyInfo, SkSignError, SkSigner};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use russh::client::{self, AuthResult, KeyboardInteractiveAuthResponse};
use russh::keys::agent::AgentIdentity;
use russh::keys::ssh_key::public::KeyData;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use russh::MethodKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use zeroize::Zeroizing;

/// One step of the chain, in the order they are tried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Agent keys and identity files, ordered by [`order_identities`].
    PublicKey,
    KeyboardInteractive,
    Password,
}

impl Stage {
    pub const ORDER: [Stage; 3] = [
        Stage::PublicKey,
        Stage::KeyboardInteractive,
        Stage::Password,
    ];

    fn method(self) -> MethodKind {
        match self {
            Stage::PublicKey => MethodKind::PublicKey,
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
            Stage::PublicKey => self.agent || self.identity_files,
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

/// How many partial successes (a method accepted, another one required)
/// one login goes through before giving up.
pub const MAX_PARTIAL_SUCCESSES: usize = 4;

/// The next stage to run: the first enabled one the server still accepts
/// that has not run since the last partial success (`done`). After a
/// partial success the caller clears `done`, so a method that ran before
/// (with nothing to show then) can run again: the server may only accept a
/// key once a keyboard-interactive step went through.
pub fn next_stage(
    config: &StageConfig,
    server_methods: &[MethodKind],
    done: &[Stage],
) -> Option<Stage> {
    Stage::ORDER.into_iter().find(|stage| {
        config.enabled(*stage) && server_methods.contains(&stage.method()) && !done.contains(stage)
    })
}

/// A key the public-key stage offers, in [`order_identities`] order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Candidate {
    /// The agent's key at this index; the agent signs.
    Agent(usize),
    /// The identity file at this index; signed in memory, decrypted only
    /// once the server accepted the key.
    File(usize),
}

/// The order OpenSSH offers keys in: configured identities the agent holds
/// first (signed by the agent, in the configured order), then the agent's
/// other keys (left out with `IdentitiesOnly`), then the configured files
/// the agent does not hold. `files` holds the public part of each configured
/// file when it is known without a passphrase. A public key is never
/// offered twice.
pub fn order_identities<K: PartialEq>(
    files: &[Option<K>],
    agent: &[K],
    identities_only: bool,
) -> Vec<Candidate> {
    let mut order: Vec<Candidate> = Vec::new();
    let mut seen: Vec<&K> = Vec::new();
    let mut file_in_agent = vec![false; files.len()];
    for (index, file) in files.iter().enumerate() {
        let Some(key) = file else { continue };
        if let Some(held) = agent.iter().position(|a| a == key) {
            file_in_agent[index] = true;
            if !seen.contains(&key) {
                seen.push(key);
                order.push(Candidate::Agent(held));
            }
        }
    }
    if !identities_only {
        for (index, key) in agent.iter().enumerate() {
            if !seen.contains(&key) {
                seen.push(key);
                order.push(Candidate::Agent(index));
            }
        }
    }
    for (index, file) in files.iter().enumerate() {
        if file_in_agent[index] {
            continue;
        }
        match file {
            Some(key) if seen.contains(&key) => {}
            Some(key) => {
                seen.push(key);
                order.push(Candidate::File(index));
            }
            None => order.push(Candidate::File(index)),
        }
    }
    order
}

/// The type of key an OpenSSH private key file holds when it is a FIDO2
/// `sk-*` key. The public part of the key is stored unencrypted, so this
/// works for passphrase-protected files too. Whether such a key can be used
/// depends on the authenticator backend ([`crate::hardware_backend_available`]).
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

/// What the chain knows while it runs.
struct State {
    remaining: Vec<MethodKind>,
    tried: Vec<AuthMethod>,
    notes: Vec<String>,
    /// The last refusal said "partial success".
    partial: bool,
    /// Public keys already offered, so none is offered twice.
    offered: Vec<KeyData>,
    /// The agent and its keys, once asked.
    agent: Option<AgentKeys>,
    /// The public-key stage went through every key once.
    keys_done: bool,
    /// Keys whose passphrase was asked for before they could be offered.
    unlocked: Vec<String>,
    /// The agent holds user certificates, which the engine does not offer.
    agent_certificates: bool,
}

struct AgentKeys {
    client: Option<DynAgent>,
    keys: Vec<PublicKey>,
}

impl State {
    /// Keeps `remaining` current from a refused attempt; `true` on success.
    fn note(&mut self, result: &AuthResult) -> bool {
        match result {
            AuthResult::Success => true,
            AuthResult::Failure {
                remaining_methods,
                partial_success,
            } => {
                self.remaining = remaining_methods.iter().copied().collect();
                self.partial |= *partial_success;
                false
            }
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
    let mut state = State {
        remaining: Vec::new(),
        tried: Vec::new(),
        notes: Vec::new(),
        partial: false,
        offered: Vec::new(),
        agent: None,
        keys_done: false,
        unlocked: Vec::new(),
        agent_certificates: false,
    };

    let first = handle.authenticate_none(user).await?;
    if state.note(&first) {
        return Ok(AuthMethod::None);
    }
    state.partial = false;

    let config = StageConfig::from_params(params);
    let mut done: Vec<Stage> = Vec::new();
    let mut ran: Vec<Stage> = Vec::new();
    let mut partials = 0;
    while let Some(stage) = next_stage(&config, &state.remaining, &done) {
        done.push(stage);
        if !ran.contains(&stage) {
            ran.push(stage);
        }
        state.partial = false;
        let outcome = match stage {
            Stage::PublicKey => try_public_keys(handle, params, interaction, &mut state).await?,
            Stage::KeyboardInteractive => {
                try_keyboard_interactive(handle, params, interaction, &mut state).await?
            }
            Stage::Password => try_password(handle, params, interaction, &mut state).await?,
        };
        if let Some(method) = outcome {
            return Ok(method);
        }
        if state.partial {
            partials += 1;
            if partials > MAX_PARTIAL_SUCCESSES {
                state.notes.push("too many partial successes".into());
                break;
            }
            done.clear();
            // Keys already offered stay offered; the stage runs again for
            // the ones it did not reach.
            state.keys_done = false;
        }
    }
    for stage in Stage::ORDER {
        if config.enabled(stage) && !ran.contains(&stage) {
            state
                .notes
                .push(format!("server does not offer {:?}", stage.method()));
        }
    }
    if state.agent_certificates {
        // The server may want the certificate: let OpenSSH, which offers
        // it, make the connection.
        return Err(Error::Unsupported(
            "the agent holds a user certificate, and no other key or method was accepted".into(),
        ));
    }
    Err(Error::AuthFailed {
        tried: state.tried,
        notes: state.notes,
    })
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

/// Where a key's private part is, for signing once the server accepted it.
#[derive(Clone)]
enum KeySource {
    File(PathBuf),
    Memory(MemoryIdentity),
}

impl KeySource {
    fn label(&self) -> String {
        match self {
            KeySource::File(path) => path.display().to_string(),
            KeySource::Memory(identity) => identity.label.clone(),
        }
    }

    fn method(&self) -> AuthMethod {
        match self {
            KeySource::File(path) => AuthMethod::IdentityFile(path.clone()),
            KeySource::Memory(identity) => AuthMethod::StoredKey(identity.label.clone()),
        }
    }

    /// The private key, asking for the passphrase when needed.
    async fn load(&self, interaction: &dyn Interaction) -> Result<PrivateKey> {
        match self {
            KeySource::File(path) => load_identity(path, interaction).await,
            KeySource::Memory(identity) => {
                decode_identity(
                    identity.text(),
                    Path::new(&identity.label),
                    identity.passphrase(),
                    interaction,
                )
                .await
            }
        }
    }
}

/// What can be told about a key without its passphrase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicPart {
    pub key: Option<PublicKey>,
    /// Only a public key is there (an `IdentityFile` naming a `.pub` file):
    /// usable when the agent holds the key.
    pub public_only: bool,
}

/// The public key of key `text`, read without decrypting it: the OpenSSH
/// format keeps it in clear; an unencrypted PEM key is decoded; otherwise
/// the `.pub` text beside it (`public_text`) tells.
pub fn public_part(text: &str, public_text: Option<&str>) -> PublicPart {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("-----") {
        // A public key line (`ssh-ed25519 AAAA... comment`).
        return PublicPart {
            key: PublicKey::from_openssh(trimmed.trim()).ok(),
            public_only: true,
        };
    }
    let key = russh::keys::ssh_key::PrivateKey::from_openssh(text)
        .ok()
        .map(|key| key.public_key().clone())
        .or_else(|| {
            russh::keys::decode_secret_key(text, None)
                .ok()
                .map(|key| key.public_key().clone())
        })
        .or_else(|| public_text.and_then(|t| PublicKey::from_openssh(t.trim()).ok()));
    PublicPart {
        key,
        public_only: false,
    }
}

/// [`public_part`] of a key file: `path` itself, and `path.pub` beside it.
async fn read_public_part(path: &Path) -> std::result::Result<PublicPart, String> {
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut pub_path = path.as_os_str().to_owned();
    pub_path.push(".pub");
    let public_text = tokio::fs::read_to_string(PathBuf::from(pub_path))
        .await
        .ok();
    let text = Zeroizing::new(text);
    Ok(public_part(&text, public_text.as_deref()))
}

/// Signs for a key whose private part is loaded (and its passphrase asked
/// for) only when the server accepted the public key.
struct LazySigner<'a> {
    source: &'a KeySource,
    interaction: &'a dyn Interaction,
}

#[derive(Debug)]
enum LazySignError {
    Send(russh::SendError),
    Failed(Error),
}

impl From<russh::SendError> for LazySignError {
    fn from(error: russh::SendError) -> Self {
        LazySignError::Send(error)
    }
}

/// The SSH signature blob of `data` by `key`; `hash` picks the RSA variant.
fn sign_blob(key: &PrivateKey, hash: Option<HashAlg>, data: &[u8]) -> Result<Vec<u8>> {
    use russh::keys::signature::Signer;
    use russh::keys::ssh_key::encoding::Encode;
    use russh::keys::ssh_key::private::KeypairData;
    use russh::keys::ssh_key::Signature;
    let signature: Signature = match key.key_data() {
        KeypairData::Rsa(rsa) => Signer::try_sign(&(rsa, hash), data),
        other => Signer::try_sign(other, data),
    }
    .map_err(|e| Error::Protocol(format!("cannot sign: {e}")))?;
    signature
        .encode_vec()
        .map_err(|e| Error::Protocol(format!("cannot encode the signature: {e}")))
}

impl russh::Signer for LazySigner<'_> {
    type Error = LazySignError;

    #[allow(clippy::manual_async_fn)]
    fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        mut to_sign: Vec<u8>,
    ) -> impl std::future::Future<Output = std::result::Result<Vec<u8>, Self::Error>> + Send {
        async move {
            let key = self
                .source
                .load(self.interaction)
                .await
                .map_err(LazySignError::Failed)?;
            let signature = sign_blob(&key, hash_alg, &to_sign).map_err(LazySignError::Failed)?;
            to_sign.extend_from_slice(&(signature.len() as u32).to_be_bytes());
            to_sign.extend_from_slice(&signature);
            Ok(to_sign)
        }
    }
}

/// One key the stage may offer.
struct KeyEntry {
    source: KeySource,
    public: PublicPart,
}

async fn agent_keys(params: &ConnectParams, state: &mut State) {
    if state.agent.is_some() {
        return;
    }
    let mut found = AgentKeys {
        client: None,
        keys: Vec::new(),
    };
    if params.agent != AgentSource::Disabled {
        match crate::agent::connect(&params.agent).await {
            Ok((mut client, _kind)) => match client.request_identities().await {
                Ok(identities) => {
                    for identity in identities {
                        let AgentIdentity::PublicKey { key, .. } = identity else {
                            // Not offered; OpenSSH takes over if nothing
                            // else gets in.
                            state.agent_certificates = true;
                            continue;
                        };
                        if key.algorithm().as_str().starts_with("sk-") {
                            state.notes.push(
                                "agent key skipped: FIDO2 sk-* keys are not supported".into(),
                            );
                            continue;
                        }
                        found.keys.push(key);
                    }
                    if found.keys.is_empty() {
                        state.notes.push("agent holds no keys".into());
                    }
                    found.client = Some(client);
                }
                Err(e) => state.notes.push(format!("agent did not list keys: {e}")),
            },
            Err(reason) => state.notes.push(format!("agent unavailable: {reason}")),
        }
    }
    state.agent = Some(found);
}

/// Agent keys and identity files, in OpenSSH's order, each offered once.
/// A file's passphrase is asked for only after the server accepted its
/// public key; a file whose public key cannot be read without the
/// passphrase (an encrypted PEM key without a `.pub`) comes last.
async fn try_public_keys<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
    state: &mut State,
) -> Result<Option<AuthMethod>> {
    if state.keys_done {
        return Ok(None);
    }
    state.keys_done = true;
    let user = params.username.as_str();

    // Keys handed over in memory come first (a saved key is the one the
    // user chose for this host).
    let mut memory = Vec::new();
    for identity in &params.memory_identities {
        let text = identity.text();
        let mut public = public_part(text, None);
        if public.key.is_none() {
            // A PEM key with its saved passphrase: no question either way.
            if let Some(passphrase) = identity.passphrase() {
                public.key = russh::keys::decode_secret_key(text, Some(passphrase))
                    .ok()
                    .map(|key| key.public_key().clone());
            }
        }
        memory.push(KeyEntry {
            source: KeySource::Memory(identity.clone()),
            public,
        });
    }
    let mut files = Vec::new();
    for path in &params.identity_files {
        match read_public_part(path).await {
            Ok(public) => files.push(KeyEntry {
                source: KeySource::File(path.clone()),
                public,
            }),
            Err(note) => state.notes.push(note),
        }
    }
    agent_keys(params, state).await;
    let agent_held: Vec<KeyData> = state
        .agent
        .as_ref()
        .map(|a| a.keys.iter().map(|k| k.key_data().clone()).collect())
        .unwrap_or_default();
    let file_keys: Vec<Option<KeyData>> = files
        .iter()
        .map(|f| f.public.key.as_ref().map(|k| k.key_data().clone()))
        .collect();
    let order = order_identities(&file_keys, &agent_held, params.identities_only);

    let mut deferred: Vec<&KeyEntry> = Vec::new();
    for entry in &memory {
        if let Some(method) =
            offer_entry(handle, params, interaction, state, entry, &mut deferred).await?
        {
            return Ok(Some(method));
        }
    }
    for candidate in order {
        if !state.remaining.contains(&MethodKind::PublicKey) {
            return Ok(None);
        }
        match candidate {
            Candidate::Agent(index) => {
                let Some(agent) = state.agent.as_mut() else {
                    continue;
                };
                let key = agent.keys[index].clone();
                if state.offered.contains(key.key_data()) {
                    continue;
                }
                let Some(client) = agent.client.as_mut() else {
                    continue;
                };
                state.offered.push(key.key_data().clone());
                if !state.tried.contains(&AuthMethod::Agent) {
                    state.tried.push(AuthMethod::Agent);
                }
                let hash = rsa_hash(handle, key.algorithm().is_rsa()).await?;
                match handle
                    .authenticate_publickey_with(user, key, hash, client)
                    .await
                {
                    Ok(result) => {
                        if state.note(&result) {
                            return Ok(Some(AuthMethod::Agent));
                        }
                    }
                    Err(e) => state.notes.push(format!("agent could not sign: {e}")),
                }
            }
            Candidate::File(index) => {
                if let Some(method) = offer_entry(
                    handle,
                    params,
                    interaction,
                    state,
                    &files[index],
                    &mut deferred,
                )
                .await?
                {
                    return Ok(Some(method));
                }
            }
        }
    }
    // Keys whose public part needs the passphrase: last, as they ask first.
    for entry in deferred {
        if !state.remaining.contains(&MethodKind::PublicKey) {
            break;
        }
        // Asked for once per login, also when the stage runs again after a
        // partial success.
        let label = entry.source.label();
        if state.unlocked.contains(&label) {
            continue;
        }
        state.unlocked.push(label);
        let key = match entry.source.load(interaction).await {
            Ok(key) => key,
            Err(e) => {
                state.notes.push(e.to_string());
                continue;
            }
        };
        if state.offered.contains(key.public_key().key_data()) {
            continue;
        }
        state.offered.push(key.public_key().key_data().clone());
        let method = entry.source.method();
        state.tried.push(method.clone());
        match offer_key(handle, params, interaction, key, &entry.source.label()).await? {
            Ok(result) => {
                if state.note(&result) {
                    return Ok(Some(method));
                }
            }
            Err(note) => state.notes.push(note),
        }
    }
    Ok(None)
}

/// Offers one memory key or identity file that the agent does not hold.
async fn offer_entry<'e, H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
    state: &mut State,
    entry: &'e KeyEntry,
    deferred: &mut Vec<&'e KeyEntry>,
) -> Result<Option<AuthMethod>> {
    if !state.remaining.contains(&MethodKind::PublicKey) {
        return Ok(None);
    }
    let label = entry.source.label();
    if entry.public.public_only {
        state.notes.push(format!(
            "{label} is a public key and the agent does not hold its private key"
        ));
        return Ok(None);
    }
    let Some(public) = entry.public.key.clone() else {
        deferred.push(entry);
        return Ok(None);
    };
    if state.offered.contains(public.key_data()) {
        return Ok(None);
    }
    state.offered.push(public.key_data().clone());
    let method = entry.source.method();
    if public.algorithm().as_str().starts_with("sk-") {
        // The security key signer needs the key handle from the private
        // part, so it is loaded first.
        let key = match entry.source.load(interaction).await {
            Ok(key) => key,
            Err(e) => {
                state.notes.push(e.to_string());
                return Ok(None);
            }
        };
        state.tried.push(method.clone());
        return match offer_key(handle, params, interaction, key, &label).await? {
            Ok(result) => Ok(state.note(&result).then_some(method)),
            Err(note) => {
                state.notes.push(note);
                Ok(None)
            }
        };
    }
    state.tried.push(method.clone());
    let hash = rsa_hash(handle, public.algorithm().is_rsa()).await?;
    let mut signer = LazySigner {
        source: &entry.source,
        interaction,
    };
    match handle
        .authenticate_publickey_with(params.username.as_str(), public, hash, &mut signer)
        .await
    {
        Ok(result) => Ok(state.note(&result).then_some(method)),
        Err(LazySignError::Send(_)) => Err(Error::Disconnected),
        Err(LazySignError::Failed(error)) => {
            state.notes.push(error.to_string());
            Ok(None)
        }
    }
}

/// Offers `key` to the server and signs when the server accepts it. An
/// `sk-*` key is signed by the security key backend (touch and PIN go
/// through `interaction`); any other key is signed in memory. The inner
/// `Err` is a note for the failure report: the key could not be used.
async fn offer_key<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
    key: PrivateKey,
    label: &str,
) -> Result<std::result::Result<AuthResult, String>> {
    let user = params.username.as_str();
    if let Some(info) = SkKeyInfo::of(&key) {
        let Some(backend) = params.security_key.clone().or_else(default_security_key) else {
            return Ok(Err(format!(
                "{label} is a FIDO2 security key ({}), and this build has no security key \
                 backend for this platform",
                info.algorithm.name()
            )));
        };
        let mut signer = SkSigner {
            backend,
            interaction,
            label,
            info,
        };
        return match handle
            .authenticate_publickey_with(user, key.public_key().clone(), None, &mut signer)
            .await
        {
            Ok(result) => Ok(Ok(result)),
            Err(SkSignError::Send(_)) => Err(Error::Disconnected),
            Err(SkSignError::Failed(error)) => Ok(Err(format!("{label}: {error}"))),
        };
    }
    let hash = rsa_hash(handle, key.algorithm().is_rsa()).await?;
    let result = handle
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
        .await?;
    Ok(Ok(result))
}

const MAX_KBD_ROUNDS: usize = 8;

/// Whether a keyboard-interactive round only asks for the login password:
/// one hidden prompt that says "password" (and is not a password change).
pub fn asks_only_for_password(prompts: &[KbdPrompt]) -> bool {
    let [prompt] = prompts else {
        return false;
    };
    let text = prompt.text.to_lowercase();
    !prompt.echo
        && text.contains("password")
        && ![
            "new", "again", "retype", "confirm", "old", "current", "expired",
        ]
        .iter()
        .any(|word| text.contains(word))
}

fn password_prompt(params: &ConnectParams) -> String {
    format!("{}@{}'s password", params.username, params.host)
}

async fn try_keyboard_interactive<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
    state: &mut State,
) -> Result<Option<AuthMethod>> {
    if !state.tried.contains(&AuthMethod::KeyboardInteractive) {
        state.tried.push(AuthMethod::KeyboardInteractive);
    }
    let mut saved_offered = false;
    // A second attempt only when the saved password was refused: then the
    // user is asked, as after a refused saved password for `password`.
    for _attempt in 0..2 {
        let mut saved_used = false;
        let mut response = handle
            .authenticate_keyboard_interactive_start(params.username.as_str(), None)
            .await?;
        let mut rounds = 0;
        loop {
            match response {
                KeyboardInteractiveAuthResponse::Success => {
                    return Ok(Some(AuthMethod::KeyboardInteractive))
                }
                KeyboardInteractiveAuthResponse::Failure {
                    remaining_methods,
                    partial_success,
                } => {
                    state.remaining = remaining_methods.iter().copied().collect();
                    state.partial |= partial_success;
                    if saved_used
                        && !partial_success
                        && state.remaining.contains(&MethodKind::KeyboardInteractive)
                    {
                        break;
                    }
                    return Ok(None);
                }
                KeyboardInteractiveAuthResponse::InfoRequest {
                    name,
                    instructions,
                    prompts,
                } => {
                    rounds += 1;
                    if rounds > MAX_KBD_ROUNDS {
                        state
                            .notes
                            .push("keyboard-interactive did not finish".into());
                        return Ok(None);
                    }
                    let prompts: Vec<KbdPrompt> = prompts
                        .into_iter()
                        .map(|p| KbdPrompt {
                            text: p.prompt,
                            echo: p.echo,
                        })
                        .collect();
                    // A server that asks for the password this way (PAM)
                    // gets the saved one, once, as the password method would.
                    let mut answers = None;
                    if !saved_offered && asks_only_for_password(&prompts) {
                        saved_offered = true;
                        if let Some(password) =
                            interaction.saved_password(&password_prompt(params)).await
                        {
                            saved_used = true;
                            answers = Some(vec![password]);
                        }
                    }
                    if answers.is_none() {
                        answers = if prompts.is_empty() {
                            Some(Vec::new())
                        } else {
                            interaction
                                .keyboard_interactive(&name, &instructions, &prompts)
                                .await
                        };
                    }
                    let Some(answers) = answers else {
                        state.notes.push("keyboard-interactive cancelled".into());
                        return Ok(None);
                    };
                    response = handle
                        .authenticate_keyboard_interactive_respond(answers)
                        .await?;
                }
            }
        }
    }
    Ok(None)
}

async fn try_password<H: client::Handler>(
    handle: &mut client::Handle<H>,
    params: &ConnectParams,
    interaction: &dyn Interaction,
    state: &mut State,
) -> Result<Option<AuthMethod>> {
    let prompt = password_prompt(params);
    for _ in 0..params.password_attempts.max(1) {
        let Some(password) = interaction.password(&prompt).await else {
            state.notes.push("password prompt cancelled".into());
            break;
        };
        if !state.tried.contains(&AuthMethod::Password) {
            state.tried.push(AuthMethod::Password);
        }
        let result = handle
            .authenticate_password(params.username.as_str(), password)
            .await?;
        if state.note(&result) {
            return Ok(Some(AuthMethod::Password));
        }
        if state.partial || !state.remaining.contains(&MethodKind::Password) {
            break;
        }
    }
    Ok(None)
}

#[cfg(test)]
#[path = "auth_tests.rs"]
mod login_tests;

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
                Stage::PublicKey,
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
            vec![Stage::PublicKey]
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
            vec![
                Stage::PublicKey,
                Stage::KeyboardInteractive,
                Stage::Password
            ]
        );
        params.agent = AgentSource::Disabled;
        params.identity_files.push("k".into());
        params.password = false;
        assert_eq!(
            plan(&StageConfig::from_params(&params), &ALL),
            vec![Stage::PublicKey, Stage::KeyboardInteractive]
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
            vec![Stage::PublicKey]
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
