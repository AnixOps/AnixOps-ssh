//! The vault commands of the mobile app: lock state, passphrase, biometric
//! unlock, the lock-in-the-background policy and the secrets of each host.
//!
//! Same command names and JSON shapes as the desktop's `vault.rs` where the
//! meaning is the same, so the React side shares its types. The device key
//! sits in the Android Keystore or the iOS Keychain (`easyssh-mobile-keys`
//! over the `device-key` plugin); a command that can raise a biometric prompt
//! runs on a blocking thread, because the prompt needs the main thread to
//! show itself and the command must not be holding it.

use crate::mobile::{Mobile, Shared};
use easyssh_mobile_keys::{mark_setup_done, setup_done, status_json};
use easyssh_vault::{proxy_password_change, secrets_update, Vault, VaultError};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::SystemTime;
use tauri::{AppHandle, Emitter, State};
use zeroize::Zeroizing;

/// Runs `task` where it may block on a system prompt.
async fn blocking<T: Send + 'static>(
    task: impl FnOnce() -> Result<T, VaultError> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(task)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// The vault's state for the UI, with the first-run flag from the database.
fn status(mobile: &Shared) -> Value {
    status_json(&mobile.vault, mobile.with_store(setup_done))
}

/// Tells every window the vault locked or unlocked.
pub fn announce(app: &AppHandle, vault: &Vault) {
    let _ = app.emit("vault-changed", json!({ "unlocked": vault.is_unlocked() }));
}

#[tauri::command]
pub async fn vault_status(state: State<'_, Mobile>) -> Result<Value, String> {
    let mobile = state.share();
    blocking(move || Ok(status(&mobile))).await
}

#[tauri::command]
pub async fn vault_unlock(
    app: AppHandle,
    state: State<'_, Mobile>,
    passphrase: String,
) -> Result<Value, String> {
    let passphrase = Zeroizing::new(passphrase);
    let mobile = state.share();
    let reply = blocking(move || {
        mobile.vault.unlock(&passphrase)?;
        Ok(status(&mobile))
    })
    .await?;
    announce(&app, &state.vault);
    Ok(reply)
}

/// Opens the vault with an unlock method (biometrics); the system prompt shows.
#[tauri::command]
pub async fn vault_unlock_with(
    app: AppHandle,
    state: State<'_, Mobile>,
    method: String,
) -> Result<Value, String> {
    let mobile = state.share();
    let reply = blocking(move || {
        mobile.vault.unlock_with(&method)?;
        Ok(status(&mobile))
    })
    .await?;
    announce(&app, &state.vault);
    Ok(reply)
}

/// Turns an unlock method on; needs the vault passphrase once.
#[tauri::command]
pub async fn vault_enable_unlock(
    app: AppHandle,
    state: State<'_, Mobile>,
    method: String,
    passphrase: String,
) -> Result<Value, String> {
    let passphrase = Zeroizing::new(passphrase);
    let mobile = state.share();
    let reply = blocking(move || {
        mobile.vault.enable_unlock(&method, &passphrase)?;
        Ok(status(&mobile))
    })
    .await?;
    announce(&app, &state.vault);
    Ok(reply)
}

#[tauri::command]
pub async fn vault_disable_unlock(
    state: State<'_, Mobile>,
    method: String,
) -> Result<Value, String> {
    let mobile = state.share();
    blocking(move || {
        mobile.vault.disable_unlock(&method)?;
        Ok(status(&mobile))
    })
    .await
}

#[tauri::command]
pub async fn vault_lock(app: AppHandle, state: State<'_, Mobile>) -> Result<Value, String> {
    let mobile = state.share();
    let reply = blocking(move || {
        mobile.vault.lock();
        Ok(status(&mobile))
    })
    .await?;
    announce(&app, &state.vault);
    Ok(reply)
}

/// Sets the vault passphrase, or changes it when `current` is given. Either
/// way the first-run choice is made.
#[tauri::command]
pub async fn vault_set_passphrase(
    app: AppHandle,
    state: State<'_, Mobile>,
    passphrase: String,
    current: Option<String>,
) -> Result<Value, String> {
    let new = Zeroizing::new(passphrase);
    let current = current.map(Zeroizing::new);
    let mobile = state.share();
    let reply = blocking(move || {
        match current {
            Some(current) => mobile.vault.change_passphrase(&current, &new)?,
            None => mobile.vault.set_passphrase(&new)?,
        }
        mobile.with_store(mark_setup_done)?;
        Ok(status(&mobile))
    })
    .await?;
    announce(&app, &state.vault);
    Ok(reply)
}

#[tauri::command]
pub async fn vault_remove_passphrase(
    app: AppHandle,
    state: State<'_, Mobile>,
    passphrase: String,
) -> Result<Value, String> {
    let passphrase = Zeroizing::new(passphrase);
    let mobile = state.share();
    let reply = blocking(move || {
        mobile.vault.remove_passphrase(&passphrase)?;
        Ok(status(&mobile))
    })
    .await?;
    announce(&app, &state.vault);
    Ok(reply)
}

/// The first-run choice "use this device only": no passphrase, the key stays
/// in the Keystore or Keychain. Makes the question go away.
#[tauri::command]
pub async fn vault_setup_skip(state: State<'_, Mobile>) -> Result<Value, String> {
    let mobile = state.share();
    blocking(move || {
        mobile.with_store(mark_setup_done)?;
        Ok(status(&mobile))
    })
    .await
}

#[tauri::command]
pub async fn vault_set_auto_lock(state: State<'_, Mobile>, minutes: u32) -> Result<Value, String> {
    let mobile = state.share();
    blocking(move || {
        mobile.with_store(|store| mobile.vault.save_auto_lock(store, minutes))?;
        Ok(status(&mobile))
    })
    .await
}

/// Seconds the app may stay in the background before the vault locks.
#[tauri::command]
pub async fn vault_set_background_lock(
    state: State<'_, Mobile>,
    seconds: u32,
) -> Result<Value, String> {
    let mobile = state.share();
    blocking(move || {
        mobile.with_store(|store| mobile.vault.save_background_lock(store, seconds))?;
        Ok(status(&mobile))
    })
    .await
}

/// The web view reports that the app left the screen.
#[tauri::command]
pub fn vault_background(app: AppHandle, state: State<'_, Mobile>) {
    if state.vault.entered_background(SystemTime::now()) {
        announce(&app, &state.vault);
    }
}

/// The web view reports that the app is back. Locks when it was away too long.
#[tauri::command]
pub fn vault_foreground(app: AppHandle, state: State<'_, Mobile>) {
    if state.vault.returned_to_foreground(SystemTime::now()) {
        announce(&app, &state.vault);
    }
}

/// Deletes every saved password, key and key passphrase. Returns how many
/// hosts had any.
#[tauri::command]
pub async fn vault_forget_all(state: State<'_, Mobile>) -> Result<usize, String> {
    let mobile = state.share();
    blocking(move || mobile.with_store(|store| mobile.vault.forget_all(store))).await
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsView {
    password: bool,
    private_key: bool,
    passphrase: bool,
    proxy_password: bool,
}

fn secrets_view(flags: easyssh_store::SecretFlags) -> SecretsView {
    SecretsView {
        password: flags.password,
        private_key: flags.private_key,
        passphrase: flags.passphrase,
        proxy_password: flags.proxy_password,
    }
}

/// Which secrets a host has saved; never their values.
#[tauri::command]
pub async fn secrets_status(
    state: State<'_, Mobile>,
    host_id: String,
) -> Result<SecretsView, String> {
    let mobile = state.share();
    blocking(move || {
        mobile
            .with_store(|store| mobile.vault.flags(store, &host_id))
            .map(secrets_view)
    })
    .await
}

/// Saves secrets of a host. A field left out (`None`) is kept, an empty
/// string clears it. With the vault locked and a passphrase set the answer is
/// the error "the vault is locked".
#[tauri::command]
pub async fn secrets_set(
    state: State<'_, Mobile>,
    host_id: String,
    password: Option<String>,
    private_key: Option<String>,
    passphrase: Option<String>,
    proxy_password: Option<String>,
) -> Result<SecretsView, String> {
    let mobile = state.share();
    blocking(move || {
        let mut update = secrets_update(password, private_key, passphrase)?;
        update.proxy_password = proxy_password_change(proxy_password)?;
        // Open the vault first, outside the database lock: a biometric prompt
        // can take as long as the person does.
        if !update.is_empty() {
            mobile.vault.ensure_unlocked()?;
        }
        mobile
            .with_store(|store| {
                mobile.vault.set_host_secrets(store, &host_id, &update)?;
                mobile.vault.flags(store, &host_id)
            })
            .map(secrets_view)
    })
    .await
}

/// Starts the idle timer: every 15 seconds the vault checks whether it has
/// been unused for the auto-lock time and tells the windows when it locks.
pub fn start_idle_timer(app: AppHandle, vault: Arc<Vault>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(15));
        if vault.tick() {
            announce(&app, &vault);
        }
    });
}
