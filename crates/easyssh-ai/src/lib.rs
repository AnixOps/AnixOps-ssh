//! Ask AI with the user's own key.
//!
//! * [`provider`]: the three providers (Anthropic, OpenAI-compatible with a
//!   custom base URL, local Ollama), how each request is built, and how each
//!   streamed answer is read.
//! * [`scrub`]: removes obvious secrets from terminal context before it is
//!   shown and sent, and says what it removed.
//! * [`prompt`]: turns the question and the context the user picked into the
//!   exact prompt that is shown for review and then sent.
//! * [`run`]: the HTTP call itself, streamed, with timeouts and cancel.
//! * [`settings`]: provider settings in the app database and the API key in
//!   the vault.
//!
//! Requests are made from Rust only. The key is read from the vault right
//! before the call and is never part of anything returned to the webview.
//! Nothing here records prompts or answers anywhere.

#![forbid(unsafe_code)]

pub mod prompt;
pub mod provider;
pub mod run;
pub mod scrub;
pub mod settings;

pub use prompt::{prepare, ContextKind, ContextPiece, Prepared, Redaction};
pub use provider::{AiConfig, HttpRequest, Provider, DEFAULT_ANTHROPIC_MODEL};
pub use run::{stream, AiError, Cancel, Limits};
pub use settings::{AiSettings, SettingsError};
