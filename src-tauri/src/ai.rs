//! Ask AI with the user's own key.
//!
//! The webview prepares a question and the context the user picked; this
//! module scrubs it and hands back the exact request for review
//! (`ai_prepare`), and sends only that reviewed request when the user presses
//! Send (`ai_send`). The key is read from the vault right before the call, on
//! this side only; nothing that goes to the webview contains it. Prompts and
//! answers are streamed to the window and kept nowhere: not in a file, not in
//! the log, not in telemetry.

use crate::hosts::open_store;
use crate::vault::app_vault;
use easyssh_ai::provider::build_request;
use easyssh_ai::settings as keys;
use easyssh_ai::{
    prepare, stream, AiConfig, AiSettings, Cancel, ContextPiece, Limits, Prepared, Provider,
    Redaction,
};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use tauri::{ipc::Channel, AppHandle, Manager, State};
use zeroize::Zeroizing;

/// Reviews waiting for Send. Old ones are dropped when this many are open.
const MAX_REVIEWS: usize = 16;

#[derive(Clone)]
struct Review {
    config: AiConfig,
    prepared: Prepared,
}

#[derive(Default)]
pub struct Ai {
    reviews: Mutex<HashMap<String, Review>>,
    running: Mutex<HashMap<String, Cancel>>,
}

fn lock_error<T>(_: T) -> String {
    "the AI state is unavailable".to_owned()
}

/// Settings as the form shows them. The key is never part of it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigView {
    #[serde(flatten)]
    settings: AiSettings,
    has_key: bool,
}

fn config_view(store: &easyssh_store::Store, settings: AiSettings) -> ConfigView {
    let has_key = keys::has_key(store, settings.provider);
    ConfigView { settings, has_key }
}

#[tauri::command]
pub fn ai_config_get() -> Result<ConfigView, String> {
    let store = open_store()?;
    Ok(config_view(&store, AiSettings::load(&store)))
}

#[tauri::command]
pub fn ai_config_save(settings: AiSettings) -> Result<ConfigView, String> {
    let store = open_store()?;
    settings.save(&store).map_err(|error| error.to_string())?;
    Ok(config_view(&store, settings))
}

/// Saves the API key of `provider` in the vault; `None` or empty removes it.
#[tauri::command]
pub async fn ai_key_set(provider: Provider, key: Option<String>) -> Result<ConfigView, String> {
    let key = key.map(Zeroizing::new);
    tauri::async_runtime::spawn_blocking(move || {
        let store = open_store()?;
        let vault = app_vault();
        keys::set_key(
            &store,
            &vault,
            provider,
            key.as_ref().map(|key| key.as_str()),
        )
        .map_err(|error| error.to_string())?;
        Ok(config_view(&store, AiSettings::load(&store)))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The exact request, for the "Review and send" step.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewView {
    review_id: String,
    provider: Provider,
    model: String,
    /// The address the request goes to.
    endpoint: String,
    system: String,
    user: String,
    redactions: Vec<Redaction>,
    truncated: usize,
}

/// Scrubs the context, builds the prompt and keeps it for `ai_send`. Nothing
/// is sent.
#[tauri::command]
pub fn ai_prepare(
    ai: State<'_, Ai>,
    question: String,
    pieces: Vec<ContextPiece>,
) -> Result<ReviewView, String> {
    let store = open_store()?;
    let settings = AiSettings::load(&store);
    if !settings.enabled {
        return Err("Ask AI is off. Turn it on in Settings, AI.".to_owned());
    }
    let config = settings.config();
    config.validate()?;
    if config.needs_key() && !keys::has_key(&store, config.provider) {
        return Err("Add an API key in Settings, AI, first.".to_owned());
    }
    let prepared = prepare(&question, &pieces)?;
    let review_id = uuid::Uuid::new_v4().simple().to_string();
    let view = ReviewView {
        review_id: review_id.clone(),
        provider: config.provider,
        model: config.model.clone(),
        endpoint: config.base_url(),
        system: prepared.system.clone(),
        user: prepared.user.clone(),
        redactions: prepared.redactions.clone(),
        truncated: prepared.truncated,
    };
    let mut reviews = ai.reviews.lock().map_err(lock_error)?;
    if reviews.len() >= MAX_REVIEWS {
        reviews.clear();
    }
    reviews.insert(review_id, Review { config, prepared });
    Ok(view)
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum AiEvent {
    Delta { text: String },
    Done,
    Error { message: String },
    Cancelled,
}

/// Sends a reviewed request and streams the answer on `events`. Returns the
/// id to cancel it with. A locked vault is an error here (the review stays,
/// so the user can unlock and press Send again).
#[tauri::command]
pub async fn ai_send(
    app: AppHandle,
    ai: State<'_, Ai>,
    review_id: String,
    events: Channel<AiEvent>,
) -> Result<String, String> {
    let review = ai
        .reviews
        .lock()
        .map_err(lock_error)?
        .get(&review_id)
        .cloned()
        .ok_or_else(|| "This review has expired. Review the request again.".to_owned())?;
    let provider = review.config.provider;
    let (request, key) = tauri::async_runtime::spawn_blocking(move || {
        let store = open_store()?;
        let key = keys::read_key(&store, &app_vault(), review.config.provider)
            .map_err(|error| error.to_string())?;
        let request = build_request(
            &review.config,
            key.as_ref().map(|key| key.as_str()),
            &review.prepared.messages(),
        )
        .map_err(|error| error.to_string())?;
        Ok::<_, String>((request, key))
    })
    .await
    .map_err(|error| error.to_string())??;

    ai.reviews.lock().map_err(lock_error)?.remove(&review_id);
    let id = uuid::Uuid::new_v4().simple().to_string();
    let cancel = Cancel::new();
    ai.running
        .lock()
        .map_err(lock_error)?
        .insert(id.clone(), cancel.clone());
    let finished = id.clone();
    std::thread::spawn(move || {
        let result = stream(
            provider,
            &request,
            key.as_ref().map(|key| key.as_str()),
            Limits::default(),
            &cancel,
            &mut |text| {
                let delta = AiEvent::Delta {
                    text: text.to_owned(),
                };
                if events.send(delta).is_err() {
                    // The window is gone; stop asking for more.
                    cancel.cancel();
                }
            },
        );
        let last = match result {
            Ok(()) => AiEvent::Done,
            Err(error) if error.is_cancelled() => AiEvent::Cancelled,
            Err(error) => AiEvent::Error {
                message: error.to_string(),
            },
        };
        let _ = events.send(last);
        if let Ok(mut running) = app.state::<Ai>().running.lock() {
            running.remove(&finished);
        }
    });
    Ok(id)
}

#[tauri::command]
pub fn ai_cancel(ai: State<'_, Ai>, request_id: String) -> Result<(), String> {
    if let Some(cancel) = ai.running.lock().map_err(lock_error)?.get(&request_id) {
        cancel.cancel();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_the_window_receives_can_hold_a_key() {
        let key = "sk-ant-super-secret-key";
        // The settings the form gets have no key field at all.
        let view = ConfigView {
            settings: AiSettings::default(),
            has_key: true,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            !json.contains(key) && !json.to_lowercase().contains("apikey"),
            "{json}"
        );
        assert!(json.contains("\"hasKey\":true"), "{json}");
        // Events carry text and messages only.
        for event in [
            AiEvent::Delta { text: "ls".into() },
            AiEvent::Done,
            AiEvent::Cancelled,
            AiEvent::Error {
                message: "The provider rejected the API key (HTTP 401)".into(),
            },
        ] {
            let json = serde_json::to_string(&event).unwrap();
            assert!(!json.contains(key), "{json}");
            assert!(json.contains("\"kind\""), "{json}");
        }
        // The review shows the request: no key either.
        let prepared = prepare("why?", &[]).unwrap();
        let view = ReviewView {
            review_id: "r".into(),
            provider: Provider::Anthropic,
            model: "claude-sonnet-5-5".into(),
            endpoint: "https://api.anthropic.com".into(),
            system: prepared.system,
            user: prepared.user,
            redactions: prepared.redactions,
            truncated: 0,
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(
            !json.contains(key) && json.contains("\"reviewId\""),
            "{json}"
        );
    }
}
