//! The encrypted vault for saved passwords and private keys: status and
//! settings for Settings → Security, and the per-host secrets the host
//! editor saves.
//!
//! With the daemon connected, the daemon owns the vault (it needs it to
//! start sessions and answer prompts) and these commands relay to its
//! `vault.*` and `secrets.*` commands. Without a daemon connection they use
//! an in-process vault over the same database and the same key chain item.
//! Values never come back to the UI: only which secrets exist.

use crate::daemon::DaemonState;
use crate::hosts::{daemon_down, open_store};
use easyssh_vault::{secrets_update, StoreAccess, Vault, VaultSecrets};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tauri::State;
use zeroize::Zeroizing;

/// This process's vault, for when there is no daemon. It uses the operating
/// system keychain like the daemon's, so both open the same database.
pub fn app_vault() -> Arc<Vault> {
    static VAULT: OnceLock<Arc<Vault>> = OnceLock::new();
    VAULT
        .get_or_init(|| {
            let vault = Arc::new(Vault::os());
            easyssh_unlock::attach_os_providers(&vault);
            // The saved auto-lock time; read with a plain store, as opening
            // the real one would ask for this vault.
            if let Ok(v1) = easyssh_core::ConfigStore::default_path() {
                if let Ok(opened) = easyssh_store::open_app_store(v1.path()) {
                    vault.load_settings(&opened.store);
                }
            }
            vault
        })
        .clone()
}

/// Opens the app database for each use, so no connection is held.
struct AppStoreAccess;

impl StoreAccess for AppStoreAccess {
    fn with_store(&self, f: &mut dyn FnMut(&easyssh_store::Store)) -> bool {
        match open_store() {
            Ok(store) => {
                f(&store);
                true
            }
            Err(_) => false,
        }
    }
}

/// The vault over the app database, as the in-process askpass bridge uses it.
pub fn app_answers() -> VaultSecrets {
    VaultSecrets::new(app_vault(), Arc::new(AppStoreAccess))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultStatusView {
    passphrase_set: bool,
    unlocked: bool,
    auto_lock_minutes: u32,
    keychain_available: bool,
    keychain_error: Option<String>,
    unlock_methods: Vec<UnlockMethodView>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockMethodView {
    name: String,
    available: bool,
    enabled: bool,
    items: Vec<UnlockItemView>,
}

/// One security key added to the vault.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnlockItemView {
    id: String,
    label: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsView {
    password: bool,
    private_key: bool,
    passphrase: bool,
}

fn status_view(value: &Value) -> VaultStatusView {
    VaultStatusView {
        passphrase_set: value["passphrase_set"].as_bool().unwrap_or(false),
        unlocked: value["unlocked"].as_bool().unwrap_or(false),
        auto_lock_minutes: value["auto_lock_minutes"].as_u64().unwrap_or(0) as u32,
        keychain_available: value["keychain_available"].as_bool().unwrap_or(false),
        keychain_error: value["keychain_error"].as_str().map(str::to_owned),
        unlock_methods: value["unlock_methods"]
            .as_array()
            .map(|methods| {
                methods
                    .iter()
                    .map(|m| UnlockMethodView {
                        name: m["name"].as_str().unwrap_or_default().to_owned(),
                        available: m["available"].as_bool().unwrap_or(false),
                        enabled: m["enabled"].as_bool().unwrap_or(false),
                        items: m["items"]
                            .as_array()
                            .map(|items| {
                                items
                                    .iter()
                                    .map(|i| UnlockItemView {
                                        id: i["id"].as_str().unwrap_or_default().to_owned(),
                                        label: i["label"].as_str().unwrap_or_default().to_owned(),
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn secrets_view(value: &Value) -> SecretsView {
    SecretsView {
        password: value["password"].as_bool().unwrap_or(false),
        private_key: value["private_key"].as_bool().unwrap_or(false),
        passphrase: value["passphrase"].as_bool().unwrap_or(false),
    }
}

fn local_status(vault: &Vault) -> VaultStatusView {
    let status = vault.status();
    VaultStatusView {
        passphrase_set: status.passphrase_set,
        unlocked: status.unlocked,
        auto_lock_minutes: status.auto_lock_minutes,
        keychain_available: status.keychain_available,
        keychain_error: status.keychain_error,
        unlock_methods: status
            .unlock_methods
            .iter()
            .map(|m| UnlockMethodView {
                name: m.name.to_owned(),
                available: m.available,
                enabled: m.enabled,
                items: m
                    .items
                    .iter()
                    .map(|i| UnlockItemView {
                        id: i.id.clone(),
                        label: i.label.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// `Ok(Some(reply))` when the daemon handled the command, `Ok(None)` when
/// there is no daemon to ask, `Err` with the daemon's message otherwise.
async fn via_daemon(
    daemon: &DaemonState,
    kind: &str,
    payload: Value,
) -> Result<Option<Value>, String> {
    let Some(link) = daemon.link() else {
        return Ok(None);
    };
    match link.request(kind, payload).await {
        Ok(value) => Ok(Some(value)),
        Err(error) if daemon_down(&error) => Ok(None),
        Err(error) => Err(error.message),
    }
}

async fn blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, easyssh_vault::VaultError> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn vault_status(daemon: State<'_, DaemonState>) -> Result<VaultStatusView, String> {
    if let Some(value) = via_daemon(&daemon, "vault.status", json!({})).await? {
        return Ok(status_view(&value));
    }
    blocking(|| Ok(local_status(&app_vault()))).await
}

#[tauri::command]
pub async fn vault_unlock(
    daemon: State<'_, DaemonState>,
    passphrase: String,
) -> Result<VaultStatusView, String> {
    let passphrase = Zeroizing::new(passphrase);
    if let Some(value) =
        via_daemon(&daemon, "vault.unlock", json!({"passphrase": &*passphrase})).await?
    {
        // This process reads some secrets itself (the AI key, session logs),
        // so its own vault opens too; the daemon's answer stands either way.
        let local = passphrase.clone();
        let _ = blocking(move || app_vault().unlock(&local)).await;
        return Ok(status_view(&value));
    }
    blocking(move || {
        let vault = app_vault();
        vault.unlock(&passphrase)?;
        Ok(local_status(&vault))
    })
    .await
}

/// Opens the vault with Windows Hello or Touch ID (the system prompt shows).
/// This process's own vault opens lazily on its first secret use.
#[tauri::command]
pub async fn vault_unlock_with(
    daemon: State<'_, DaemonState>,
    method: String,
) -> Result<VaultStatusView, String> {
    if let Some(value) =
        via_daemon(&daemon, "vault.unlock_with", json!({"method": &method})).await?
    {
        return Ok(status_view(&value));
    }
    blocking(move || {
        let vault = app_vault();
        vault.unlock_with(&method)?;
        Ok(local_status(&vault))
    })
    .await
}

/// Turns Windows Hello or Touch ID on; needs the vault passphrase once.
#[tauri::command]
pub async fn vault_enable_unlock(
    daemon: State<'_, DaemonState>,
    method: String,
    passphrase: String,
) -> Result<VaultStatusView, String> {
    let passphrase = Zeroizing::new(passphrase);
    if let Some(value) = via_daemon(
        &daemon,
        "vault.enable_unlock",
        json!({"method": &method, "passphrase": &*passphrase}),
    )
    .await?
    {
        return Ok(status_view(&value));
    }
    blocking(move || {
        let vault = app_vault();
        vault.enable_unlock(&method, &passphrase)?;
        Ok(local_status(&vault))
    })
    .await
}

/// Adds a security key to the vault: needs the vault passphrase once, then
/// the key asks for a touch (and its PIN). Several keys can be added.
#[tauri::command]
pub async fn vault_add_unlock_item(
    daemon: State<'_, DaemonState>,
    method: String,
    passphrase: String,
    label: String,
) -> Result<VaultStatusView, String> {
    let passphrase = Zeroizing::new(passphrase);
    if let Some(value) = via_daemon(
        &daemon,
        "vault.add_unlock_item",
        json!({"method": &method, "passphrase": &*passphrase, "label": &label}),
    )
    .await?
    {
        return Ok(status_view(&value));
    }
    blocking(move || {
        let vault = app_vault();
        vault.add_unlock_item(&method, &passphrase, &label)?;
        Ok(local_status(&vault))
    })
    .await
}

/// Removes one security key from the vault.
#[tauri::command]
pub async fn vault_remove_unlock_item(
    daemon: State<'_, DaemonState>,
    method: String,
    id: String,
) -> Result<VaultStatusView, String> {
    if let Some(value) = via_daemon(
        &daemon,
        "vault.remove_unlock_item",
        json!({"method": &method, "id": &id}),
    )
    .await?
    {
        return Ok(status_view(&value));
    }
    blocking(move || {
        let vault = app_vault();
        vault.remove_unlock_item(&method, &id)?;
        Ok(local_status(&vault))
    })
    .await
}

#[tauri::command]
pub async fn vault_disable_unlock(
    daemon: State<'_, DaemonState>,
    method: String,
) -> Result<VaultStatusView, String> {
    if let Some(value) =
        via_daemon(&daemon, "vault.disable_unlock", json!({"method": &method})).await?
    {
        return Ok(status_view(&value));
    }
    blocking(move || {
        let vault = app_vault();
        vault.disable_unlock(&method)?;
        Ok(local_status(&vault))
    })
    .await
}

#[tauri::command]
pub async fn vault_lock(daemon: State<'_, DaemonState>) -> Result<VaultStatusView, String> {
    if let Some(value) = via_daemon(&daemon, "vault.lock", json!({})).await? {
        app_vault().lock();
        return Ok(status_view(&value));
    }
    blocking(|| {
        let vault = app_vault();
        vault.lock();
        Ok(local_status(&vault))
    })
    .await
}

/// Sets the vault passphrase, or changes it when `current` is given.
#[tauri::command]
pub async fn vault_set_passphrase(
    daemon: State<'_, DaemonState>,
    passphrase: String,
    current: Option<String>,
) -> Result<VaultStatusView, String> {
    let mut payload = json!({"passphrase": passphrase});
    if let Some(current) = &current {
        payload["current"] = json!(current);
    }
    if let Some(value) = via_daemon(&daemon, "vault.set_passphrase", payload).await? {
        return Ok(status_view(&value));
    }
    let new = Zeroizing::new(passphrase);
    let current = current.map(Zeroizing::new);
    blocking(move || {
        let vault = app_vault();
        match current {
            Some(current) => vault.change_passphrase(&current, &new)?,
            None => vault.set_passphrase(&new)?,
        }
        Ok(local_status(&vault))
    })
    .await
}

#[tauri::command]
pub async fn vault_remove_passphrase(
    daemon: State<'_, DaemonState>,
    passphrase: String,
) -> Result<VaultStatusView, String> {
    if let Some(value) = via_daemon(
        &daemon,
        "vault.remove_passphrase",
        json!({"passphrase": passphrase}),
    )
    .await?
    {
        return Ok(status_view(&value));
    }
    let passphrase = Zeroizing::new(passphrase);
    blocking(move || {
        let vault = app_vault();
        vault.remove_passphrase(&passphrase)?;
        Ok(local_status(&vault))
    })
    .await
}

#[tauri::command]
pub async fn vault_set_auto_lock(
    daemon: State<'_, DaemonState>,
    minutes: u32,
) -> Result<VaultStatusView, String> {
    if let Some(value) =
        via_daemon(&daemon, "vault.set_auto_lock", json!({"minutes": minutes})).await?
    {
        return Ok(status_view(&value));
    }
    blocking(move || {
        let vault = app_vault();
        let store = open_store().map_err(easyssh_vault::VaultError::Store)?;
        vault.save_auto_lock(&store, minutes)?;
        Ok(local_status(&vault))
    })
    .await
}

/// Deletes every saved password, key and key passphrase. Returns how many
/// hosts had any.
#[tauri::command]
pub async fn vault_forget_all(daemon: State<'_, DaemonState>) -> Result<usize, String> {
    if let Some(value) = via_daemon(&daemon, "vault.forget_all", json!({})).await? {
        return Ok(value["hosts"].as_u64().unwrap_or(0) as usize);
    }
    blocking(|| {
        let store = open_store().map_err(easyssh_vault::VaultError::Store)?;
        app_vault().forget_all(&store)
    })
    .await
}

/// Which secrets a host has saved; never their values.
#[tauri::command]
pub async fn secrets_status(
    daemon: State<'_, DaemonState>,
    host_id: String,
) -> Result<SecretsView, String> {
    if let Some(value) = via_daemon(&daemon, "secrets.status", json!({"host_id": host_id})).await? {
        return Ok(secrets_view(&value));
    }
    blocking(move || {
        let store = open_store().map_err(easyssh_vault::VaultError::Store)?;
        let flags = app_vault().flags(&store, &host_id)?;
        Ok(SecretsView {
            password: flags.password,
            private_key: flags.private_key,
            passphrase: flags.passphrase,
        })
    })
    .await
}

/// Saves secrets of a host. A field left out (`None`) is kept, an empty
/// string clears it.
#[tauri::command]
pub async fn secrets_set(
    daemon: State<'_, DaemonState>,
    host_id: String,
    password: Option<String>,
    private_key: Option<String>,
    passphrase: Option<String>,
) -> Result<SecretsView, String> {
    let mut payload = json!({"host_id": host_id});
    for (name, value) in [
        ("password", &password),
        ("private_key", &private_key),
        ("passphrase", &passphrase),
    ] {
        if let Some(value) = value {
            payload[name] = json!(value);
        }
    }
    if let Some(value) = via_daemon(&daemon, "secrets.set", payload).await? {
        return Ok(secrets_view(&value));
    }
    blocking(move || {
        let update = secrets_update(password, private_key, passphrase)?;
        let store = open_store().map_err(easyssh_vault::VaultError::Store)?;
        let vault = app_vault();
        vault.set_host_secrets(&store, &host_id, &update)?;
        let flags = vault.flags(&store, &host_id)?;
        Ok(SecretsView {
            password: flags.password,
            private_key: flags.private_key,
            passphrase: flags.passphrase,
        })
    })
    .await
}
