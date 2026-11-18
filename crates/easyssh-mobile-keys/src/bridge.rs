//! The seam between Rust and the Kotlin and Swift code.
//!
//! The app registers one Tauri mobile plugin (`src-tauri/plugins/device-key`)
//! whose Kotlin and Swift classes keep the secrets. Rust reaches them through
//! [`NativeBridge`]: a command name and a JSON object in, a JSON object out.
//! Keeping the trait here, with no Tauri in it, is what lets the selection,
//! fallback and lock logic run in plain unit tests with a fake bridge.

use serde_json::Value;

/// The commands the native side implements. The names are the Kotlin
/// `@Command` functions and the Swift `@objc` methods of the plugin.
pub mod command {
    /// `{service, account}` -> `{value: base64 | null}`.
    pub const SECURE_GET: &str = "secureGet";
    /// `{service, account, value: base64}` -> `{}`.
    pub const SECURE_SET: &str = "secureSet";
    /// `{service, account}` -> `{}`; a missing item is not an error.
    pub const SECURE_DELETE: &str = "secureDelete";
    /// `{}` -> `{available: bool}`: a sensor is present and enrolled.
    pub const BOUND_STATUS: &str = "boundStatus";
    /// `{account}` -> `{present: bool}`; never raises a prompt.
    pub const BOUND_CONTAINS: &str = "boundContains";
    /// `{account, value: base64, reason}` -> `{}`; raises the prompt.
    pub const BOUND_SEAL: &str = "boundSeal";
    /// `{account, reason}` -> `{value: base64 | null}`; raises the prompt.
    pub const BOUND_OPEN: &str = "boundOpen";
    /// `{account}` -> `{}`; a missing item is not an error.
    pub const BOUND_REMOVE: &str = "boundRemove";
}

/// The error codes the native side rejects with.
pub mod code {
    /// The user dismissed the prompt or chose the passphrase.
    pub const CANCELLED: &str = "cancelled";
    /// The platform key is gone for good (biometric enrolment changed).
    pub const INVALIDATED: &str = "invalidated";
    /// No sensor, none enrolled, locked out, or no secure storage at all.
    pub const UNAVAILABLE: &str = "unavailable";
}

/// A rejected native command. `message` is the platform's text and never
/// contains a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeError {
    pub code: String,
    pub message: String,
}

impl BridgeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.message.is_empty() {
            write!(f, "[{}]", self.code)
        } else {
            f.write_str(&self.message)
        }
    }
}

impl std::error::Error for BridgeError {}

/// Calls the native plugin. Blocks until it answers (a biometric prompt can
/// take as long as the user does), so call it from a blocking thread.
pub trait NativeBridge: Send + Sync {
    fn call(&self, command: &str, args: Value) -> Result<Value, BridgeError>;
}
