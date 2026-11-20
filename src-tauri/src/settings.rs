//! App settings and a check of the local OpenSSH setup.

use crate::hosts::{load_config, open_store};
use easyssh_core::{
    scan_default_ssh_config, Locale, OpenSsh, SshEngine, TerminalPreferences, Theme,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    theme: Theme,
    locale: Locale,
    terminal_font_size: f32,
    terminal_scrollback: usize,
    /// `russh` (default, also when never chosen) or `openssh` (advanced).
    #[serde(default)]
    ssh_engine: SshEngine,
    /// `None` until the first-run question was answered.
    #[serde(default)]
    crash_reports: Option<bool>,
    #[serde(default)]
    usage_stats: bool,
    /// macOS: Option sends Meta instead of layout characters. Off by default.
    #[serde(default)]
    terminal_option_as_meta: bool,
    /// Empty means the built-in monospace stack.
    #[serde(default)]
    terminal_font_family: String,
    #[serde(default = "default_line_height")]
    terminal_line_height: f32,
    #[serde(default)]
    terminal_cursor_style: CursorStyle,
    #[serde(default = "default_true")]
    terminal_cursor_blink: bool,
}

fn default_line_height() -> f32 {
    1.2
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

/// Terminal behaviour only the app's terminal view reads. Kept under its own
/// key so the shared `terminal` preferences and their schema stay unchanged;
/// a missing or older value falls back to the defaults field by field.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
struct TerminalBehaviour {
    option_as_meta: bool,
    font_family: String,
    line_height: f32,
    cursor_style: CursorStyle,
    cursor_blink: bool,
}

impl Default for TerminalBehaviour {
    fn default() -> Self {
        Self {
            option_as_meta: false,
            font_family: String::new(),
            line_height: default_line_height(),
            cursor_style: CursorStyle::default(),
            cursor_blink: true,
        }
    }
}

const TERMINAL_BEHAVIOUR: &str = "terminal_behaviour";

fn terminal_behaviour() -> TerminalBehaviour {
    open_store()
        .ok()
        .and_then(|store| store.get_setting_as(TERMINAL_BEHAVIOUR).ok().flatten())
        .unwrap_or_default()
}

#[tauri::command]
pub fn settings_get() -> Result<Settings, String> {
    let config = load_config()?;
    let behaviour = terminal_behaviour();
    Ok(Settings {
        theme: config.theme,
        locale: config.locale,
        terminal_font_size: config.terminal.font_size,
        terminal_scrollback: config.terminal.scrollback_lines,
        ssh_engine: config.ssh_engine.unwrap_or_default(),
        crash_reports: config.telemetry.crash_reports,
        usage_stats: config.telemetry.usage_stats,
        terminal_option_as_meta: behaviour.option_as_meta,
        terminal_font_family: behaviour.font_family,
        terminal_line_height: behaviour.line_height,
        terminal_cursor_style: behaviour.cursor_style,
        terminal_cursor_blink: behaviour.cursor_blink,
    })
}

#[tauri::command]
pub fn settings_save(settings: Settings) -> Result<(), String> {
    if !(9.0..=32.0).contains(&settings.terminal_font_size) {
        return Err("the font size must be between 9 and 32".to_owned());
    }
    if !(1_000..=200_000).contains(&settings.terminal_scrollback) {
        return Err("scrollback must be between 1,000 and 200,000 lines".to_owned());
    }
    if !(1.0..=1.6).contains(&settings.terminal_line_height) {
        return Err("the line height must be between 1.0 and 1.6".to_owned());
    }
    if settings.terminal_font_family.len() > 200 {
        return Err("the font family is too long".to_owned());
    }
    // Only these settings are written, so a host saved by the daemon in the
    // meantime is not touched.
    let store = open_store()?;
    let mut terminal: TerminalPreferences = store
        .get_setting_as("terminal")
        .ok()
        .flatten()
        .unwrap_or_default();
    terminal.font_size = settings.terminal_font_size;
    terminal.scrollback_lines = settings.terminal_scrollback;
    let stored = |error: easyssh_store::StoreError| error.to_string();
    store
        .set_setting_as("theme", &settings.theme)
        .map_err(stored)?;
    store
        .set_setting_as("locale", &settings.locale)
        .map_err(stored)?;
    store
        .set_setting_as("terminal", &terminal)
        .map_err(stored)?;
    let behaviour = TerminalBehaviour {
        option_as_meta: settings.terminal_option_as_meta,
        font_family: settings.terminal_font_family.trim().to_owned(),
        line_height: settings.terminal_line_height,
        cursor_style: settings.terminal_cursor_style,
        cursor_blink: settings.terminal_cursor_blink,
    };
    if behaviour != terminal_behaviour() {
        store
            .set_setting_as(TERMINAL_BEHAVIOUR, &behaviour)
            .map_err(stored)?;
    }
    let mut telemetry = load_config()?.telemetry;
    telemetry.crash_reports = settings.crash_reports;
    telemetry.usage_stats = settings.usage_stats;
    store
        .set_setting_as("telemetry", &telemetry)
        .map_err(stored)?;
    // Absent means russh, so an unchanged default stays out of the store:
    // installs that never chose follow the default, an explicit `openssh`
    // stays.
    if settings.ssh_engine != SshEngine::Russh || load_config()?.ssh_engine.is_some() {
        store
            .set_setting_as("ssh_engine", &settings.ssh_engine)
            .map_err(stored)?;
    }
    crate::telemetry::apply(&telemetry);
    Ok(())
}

/// Monospace font families installed on this machine, sorted and without
/// duplicates; the terminal font picker offers them.
#[tauri::command]
pub async fn monospace_fonts() -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        monospace_families(&database)
    })
    .await
    .map_err(|error| error.to_string())
}

fn monospace_families(database: &fontdb::Database) -> Vec<String> {
    let mut families: Vec<String> = database
        .faces()
        .filter(|face| face.monospaced)
        .filter_map(|face| face.families.first().map(|(name, _)| name.clone()))
        .collect();
    families.sort_by_key(|name| name.to_lowercase());
    families.dedup();
    families
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    /// The agent answered and holds keys.
    Ready,
    /// The agent answered but holds no keys.
    Empty,
    /// No agent could be reached.
    Unavailable,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshStatus {
    ssh_version: Option<String>,
    sftp_found: bool,
    agent: AgentState,
    agent_keys: Vec<String>,
    agent_error: Option<String>,
    ssh_config_hosts: usize,
}

fn agent_state(available: bool, error: Option<&str>) -> AgentState {
    if available {
        AgentState::Ready
    } else if error.is_some_and(|error| error.contains("no identities")) {
        AgentState::Empty
    } else {
        AgentState::Unavailable
    }
}

/// Runs `ssh -V` and `ssh-add -l`; slow enough to keep off the main thread.
#[tauri::command]
pub async fn ssh_status() -> Result<SshStatus, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let diagnostics = OpenSsh.diagnostics(None);
        let (available, agent_keys, agent_error) = match diagnostics.agent_keys {
            Some(keys) => (keys.available, keys.fingerprints, keys.raw_error),
            None => (false, Vec::new(), Some("ssh-add was not found".to_owned())),
        };
        SshStatus {
            ssh_version: diagnostics.ssh_version,
            sftp_found: OpenSsh.sftp_path().is_ok(),
            agent: agent_state(available, agent_error.as_deref()),
            agent_keys,
            agent_error,
            ssh_config_hosts: scan_default_ssh_config().aliases.len(),
        }
    })
    .await
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::{agent_state, AgentState, TerminalBehaviour};

    #[test]
    fn terminal_behaviour_defaults_when_missing_or_partial() {
        let empty: TerminalBehaviour = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, TerminalBehaviour::default());
        assert!(!empty.option_as_meta);
        let on: TerminalBehaviour = serde_json::from_str(r#"{"option_as_meta":true}"#).unwrap();
        assert!(on.option_as_meta);
        // Settings saved before the font options existed keep working.
        assert!(empty.font_family.is_empty());
        assert_eq!(empty.line_height, 1.2);
        assert_eq!(empty.cursor_style, super::CursorStyle::Block);
        assert!(empty.cursor_blink);
        let bar: TerminalBehaviour =
            serde_json::from_str(r#"{"cursor_style":"bar","cursor_blink":false}"#).unwrap();
        assert_eq!(bar.cursor_style, super::CursorStyle::Bar);
        assert!(!bar.cursor_blink);
    }

    #[test]
    fn tells_an_empty_agent_from_a_missing_one() {
        assert_eq!(agent_state(true, None), AgentState::Ready);
        assert_eq!(
            agent_state(false, Some("The agent has no identities.")),
            AgentState::Empty
        );
        assert_eq!(
            agent_state(false, Some("Could not open a connection to your agent.")),
            AgentState::Unavailable
        );
    }
}
