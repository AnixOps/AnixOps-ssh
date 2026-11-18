//! The mobile app (Android and iOS): the engine runs in this process.
//!
//! There is no daemon, no OpenSSH, no local shell and no OS keychain here. The
//! commands have the names and shapes of the desktop ones (`load_hosts`,
//! `session_open`, `askpass_reply`, ...) for the screens the prototype needs,
//! so the React components that show prompts are shared. A session lives
//! exactly as long as its terminal screen: when the operating system suspends
//! the app the TCP connection dies and the UI offers to reconnect.

use crate::mobile_vault;
use easyssh_engine::hosts::{add_host, snapshot, target_for, NewHost};
use easyssh_engine::known_hosts::StoreKnownHosts;
use easyssh_engine::prompts::Prompts;
use easyssh_engine::{saved_login, Engine, ShellSink};
use easyssh_mobile_keys::mobile_vault as build_vault;
use easyssh_store::{open_app_store_with, Store};
use easyssh_vault::{SecretSource, Vault, VaultSecrets};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tauri::ipc::Channel;
use tauri::{Emitter, Manager, State};

pub(crate) struct Mobile {
    store: Arc<Mutex<Store>>,
    engine: Arc<Engine>,
    pub(crate) vault: Arc<Vault>,
    secrets: Arc<VaultSecrets>,
}

/// The parts of [`Mobile`] a blocking task needs, which must be `'static`.
#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) vault: Arc<Vault>,
    store: Arc<Mutex<Store>>,
}

impl Shared {
    /// Runs `work` with the database. Never call anything that can raise a
    /// system prompt inside it: the lock is held for as long as it runs.
    pub(crate) fn with_store<T>(&self, work: impl FnOnce(&Store) -> T) -> T {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        work(&store)
    }
}

impl Mobile {
    pub(crate) fn share(&self) -> Shared {
        Shared {
            vault: self.vault.clone(),
            store: self.store.clone(),
        }
    }

    fn with_store<T>(&self, work: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        let store = self.store.lock().unwrap_or_else(|p| p.into_inner());
        work(&store)
    }
}

/// Same shape as the desktop's session events.
#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum SessionEvent {
    Data { data: String },
    Exit { code: Option<u32> },
}

struct ChannelSink(Channel<SessionEvent>);

impl ShellSink for ChannelSink {
    fn data(&self, text: String) {
        let _ = self.0.send(SessionEvent::Data { data: text });
    }

    fn exit(&self, code: Option<u32>) {
        let _ = self.0.send(SessionEvent::Exit { code });
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OpenedSession {
    id: String,
    engine: &'static str,
    engine_reason: Option<String>,
}

#[derive(Deserialize)]
struct HostFieldsInput {
    hostname: String,
    username: Option<String>,
    port: u16,
}

/// The part of the desktop's `HostInput` the mobile form fills in.
#[derive(Deserialize)]
struct HostInput {
    name: String,
    fields: HostFieldsInput,
}

#[tauri::command]
fn load_hosts(state: State<'_, Mobile>) -> Result<Value, String> {
    state.with_store(|store| {
        let config = store.load_app_config().map_err(|error| error.to_string())?;
        Ok(snapshot(&config))
    })
}

#[tauri::command]
fn host_save(state: State<'_, Mobile>, input: HostInput) -> Result<String, String> {
    state.with_store(|store| {
        add_host(
            store,
            &NewHost {
                name: input.name,
                hostname: input.fields.hostname,
                username: input.fields.username,
                port: input.fields.port,
            },
        )
    })
}

#[tauri::command]
fn host_delete(state: State<'_, Mobile>, id: String) -> Result<(), String> {
    state.with_store(|store| {
        store
            .remove_host(&id)
            .map(|_| ())
            .map_err(|error| error.to_string())
    })
}

#[tauri::command]
async fn session_open(
    state: State<'_, Mobile>,
    host_id: Option<String>,
    cols: u16,
    rows: u16,
    events: Channel<SessionEvent>,
) -> Result<OpenedSession, String> {
    let host_id = host_id.ok_or("a phone has no local shell; open a host")?;
    let target = state.with_store(|store| {
        let config = store.load_app_config().map_err(|error| error.to_string())?;
        target_for(&config, &host_id)
    })?;
    // The saved key and password of the host, from the vault. Reading them can
    // raise the biometric prompt, so it runs off the async threads. A saved
    // key with the vault locked is the error "the vault is locked".
    let secrets: Arc<dyn SecretSource> = state.secrets.clone();
    let (mut target, login) = tauri::async_runtime::spawn_blocking(move || {
        saved_login(&secrets, &host_id, &target).map(|login| (target, login))
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.message)?;
    target.identity = login.identity;
    let id = state
        .engine
        .open_shell_with_login(
            target,
            login.password,
            cols,
            rows,
            Arc::new(ChannelSink(events)),
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(OpenedSession {
        id,
        engine: "russh",
        engine_reason: None,
    })
}

#[tauri::command]
fn session_write(state: State<'_, Mobile>, id: String, data: String) -> Result<(), String> {
    state.engine.write(&id, data.as_bytes())
}

#[tauri::command]
fn session_resize(
    state: State<'_, Mobile>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state.engine.resize(&id, cols, rows)
}

#[tauri::command]
fn session_close(state: State<'_, Mobile>, id: String) {
    state.engine.close(&id);
}

/// A phone keeps no session once its screen is gone, so detaching ends it.
#[tauri::command]
fn session_detach(state: State<'_, Mobile>, id: String) {
    state.engine.close(&id);
}

#[tauri::command]
fn sessions_list() -> Vec<Value> {
    Vec::new()
}

#[tauri::command]
fn askpass_reply(state: State<'_, Mobile>, id: u64, answer: Option<String>) {
    state.engine.prompts().reply_secret(id, answer);
}

#[tauri::command]
fn hostkey_reply(state: State<'_, Mobile>, id: u64, decision: String) {
    state.engine.prompts().reply_hostkey(id, &decision);
}

/// Crash reports are a desktop feature for now; the web view still reports
/// its errors through this command, so it must exist.
#[tauri::command]
fn telemetry_report_frontend() {}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_device_key::init())
        .invoke_handler(tauri::generate_handler![
            load_hosts,
            host_save,
            host_delete,
            session_open,
            session_write,
            session_resize,
            session_close,
            session_detach,
            sessions_list,
            askpass_reply,
            hostkey_reply,
            telemetry_report_frontend,
            mobile_vault::vault_status,
            mobile_vault::vault_unlock,
            mobile_vault::vault_unlock_with,
            mobile_vault::vault_enable_unlock,
            mobile_vault::vault_disable_unlock,
            mobile_vault::vault_lock,
            mobile_vault::vault_set_passphrase,
            mobile_vault::vault_remove_passphrase,
            mobile_vault::vault_setup_skip,
            mobile_vault::vault_set_auto_lock,
            mobile_vault::vault_set_background_lock,
            mobile_vault::vault_background,
            mobile_vault::vault_foreground,
            mobile_vault::vault_forget_all,
            mobile_vault::secrets_status,
            mobile_vault::secrets_set,
        ])
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            // The device key sits in the Android Keystore or the iOS Keychain;
            // the vault's cipher seals saved passwords and keys in the store.
            let vault = build_vault(tauri_plugin_device_key::bridge(app.handle()));
            // The v1 JSON file never exists on a phone; its path only names
            // the folder the database lives in.
            let opened = open_app_store_with(&dir.join("config.json"), vault.cipher())?;
            vault.load_settings(&opened.store);
            let store = Arc::new(Mutex::new(opened.store));
            let secrets = Arc::new(VaultSecrets::new(vault.clone(), store.clone()));
            mobile_vault::start_idle_timer(app.handle().clone(), vault.clone());
            let handle = app.handle().clone();
            let prompts = Prompts::new(Arc::new(move |name, payload| {
                let _ = handle.emit(name, payload);
            }));
            let engine = Engine::new(prompts, Arc::new(StoreKnownHosts::new(store.clone())));
            app.manage(Mobile {
                store,
                engine,
                vault,
                secrets,
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build AnixOps SSH")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                app.state::<Mobile>().engine.close_all();
            }
        });
}
