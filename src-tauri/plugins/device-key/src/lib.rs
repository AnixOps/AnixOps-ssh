//! The native half of the mobile vault.
//!
//! A Tauri mobile plugin with a Kotlin class (Android Keystore,
//! `BiometricPrompt`) and a Swift class (Keychain, `LocalAuthentication`).
//! It exposes no command to the web view. Rust reaches it through
//! [`DeviceKeys`], which implements the [`NativeBridge`] of
//! `easyssh-mobile-keys`; the logic that decides what to store and when to
//! prompt lives there and is unit tested.
//!
//! Desktop builds never use this crate (the app depends on it for Android and
//! iOS only); it is empty there.

#![cfg(any(target_os = "android", target_os = "ios"))]

use easyssh_mobile_keys::{BridgeError, NativeBridge};
use serde_json::Value;
use std::sync::Arc;
use tauri::plugin::mobile::PluginInvokeError;
use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{AppHandle, Manager, Runtime};

#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.anixops.ssh.devicekey";

#[cfg(target_os = "ios")]
tauri::ios_plugin_binding!(init_plugin_device_key);

/// The plugin's Kotlin or Swift side.
pub struct DeviceKeys<R: Runtime>(PluginHandle<R>);

fn bridge_error(error: PluginInvokeError) -> BridgeError {
    match error {
        PluginInvokeError::InvokeRejected(rejected) => BridgeError::new(
            rejected.code.unwrap_or_else(|| "failed".to_owned()),
            rejected.message.unwrap_or_default(),
        ),
        other => BridgeError::new("failed", other.to_string()),
    }
}

impl<R: Runtime> NativeBridge for DeviceKeys<R> {
    fn call(&self, command: &str, args: Value) -> Result<Value, BridgeError> {
        self.0
            .run_mobile_plugin::<Value>(command, args)
            .map_err(bridge_error)
    }
}

/// The native bridge of a running app. Only valid after [`init`] was added to
/// the builder.
pub fn bridge<R: Runtime>(app: &AppHandle<R>) -> Arc<dyn NativeBridge> {
    app.state::<Arc<DeviceKeys<R>>>().inner().clone()
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("device-key")
        .setup(|app, api| {
            #[cfg(target_os = "android")]
            let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "DeviceKeyPlugin")?;
            #[cfg(target_os = "ios")]
            let handle = api.register_ios_plugin(init_plugin_device_key)?;
            app.manage(Arc::new(DeviceKeys(handle)));
            Ok(())
        })
        .build()
}
