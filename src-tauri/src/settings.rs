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
    /// Programming ligatures (=> != ===) of fonts that have them.
    #[serde(default)]
    terminal_ligatures: bool,
    #[serde(default = "default_line_height")]
    terminal_line_height: f32,
    #[serde(default)]
    terminal_cursor_style: CursorStyle,
    #[serde(default = "default_true")]
    terminal_cursor_blink: bool,
    /// Ask before a multi-line paste into a shell without bracketed paste.
    #[serde(default = "yes")]
    terminal_warn_multiline_paste: bool,
    /// What a right-click in the terminal does.
    #[serde(default)]
    terminal_right_click: RightClick,
    /// Selecting text in the terminal copies it.
    #[serde(default)]
    terminal_copy_on_select: bool,
    /// Windows Terminal keys: Ctrl+C copies a selection, Ctrl+V pastes.
    #[serde(default = "default_ctrl_copy_paste")]
    terminal_ctrl_copy_paste: bool,
    /// Remote programs may write (never read) the clipboard with OSC 52.
    #[serde(default = "yes")]
    terminal_osc52: bool,
    /// Full-screen programs (btop, vim, tmux) may take mouse clicks and the wheel.
    #[serde(default = "yes")]
    terminal_mouse_reporting: bool,
    /// Reconnect by itself, with backoff, when a connection drops.
    #[serde(default = "yes")]
    terminal_auto_reconnect: bool,
    /// The editor command for "Open in editor"; empty means the system default.
    #[serde(default)]
    editor_command: String,
    /// The files pane follows the terminal's folder.
    #[serde(default)]
    follow_terminal_folder: bool,
    /// What a double-click on a file in the files pane does.
    #[serde(default)]
    files_double_click: FileDoubleClick,
    /// Compare SHA-256 of a file here and on the server after each transfer.
    #[serde(default)]
    files_verify_transfers: bool,
    /// A single click on a host connects (off: a click selects, a
    /// double-click or Enter connects, as in Termius and FinalShell).
    #[serde(default)]
    hosts_single_click: bool,
}

/// A double-click on a file: open it in the editor (FinalShell, MobaXterm,
/// WinSCP) or download it.
#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum FileDoubleClick {
    #[default]
    Edit,
    Download,
}

fn default_line_height() -> f32 {
    1.2
}

fn default_true() -> bool {
    true
}

/// On by default where Windows Terminal taught it (Windows); elsewhere Ctrl+V
/// stays the shell's (quoted insert, Vim block selection).
fn default_ctrl_copy_paste() -> bool {
    cfg!(windows)
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CursorStyle {
    #[default]
    Block,
    Bar,
    Underline,
}

fn yes() -> bool {
    true
}

/// What a right-click in the terminal does.
#[derive(Serialize, Deserialize, Default, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RightClick {
    #[default]
    Menu,
    Paste,
}

/// File pane behaviour, under its own key like `TerminalBehaviour`.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct FilesBehaviour {
    editor_command: String,
    follow_terminal_folder: bool,
    double_click: FileDoubleClick,
    verify_transfers: bool,
}

const FILES_BEHAVIOUR: &str = "files_behaviour";

fn files_behaviour() -> FilesBehaviour {
    open_store()
        .ok()
        .and_then(|store| store.get_setting_as(FILES_BEHAVIOUR).ok().flatten())
        .unwrap_or_default()
}

/// The configured editor command, empty for the system default.
pub fn editor_command() -> String {
    files_behaviour().editor_command
}

/// Host list behaviour, under its own key like `FilesBehaviour`.
#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
#[serde(default)]
struct HostsBehaviour {
    single_click: bool,
}

const HOSTS_BEHAVIOUR: &str = "hosts_behaviour";

fn hosts_behaviour() -> HostsBehaviour {
    open_store()
        .ok()
        .and_then(|store| store.get_setting_as(HOSTS_BEHAVIOUR).ok().flatten())
        .unwrap_or_default()
}

/// Terminal behaviour only the app's terminal view reads. Kept under its own
/// key so the shared `terminal` preferences and their schema stay unchanged;
/// a missing or older value falls back to the defaults field by field.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
struct TerminalBehaviour {
    option_as_meta: bool,
    font_family: String,
    ligatures: bool,
    line_height: f32,
    cursor_style: CursorStyle,
    cursor_blink: bool,
    warn_multiline_paste: bool,
    right_click: RightClick,
    copy_on_select: bool,
    ctrl_copy_paste: bool,
    osc52: bool,
    mouse_reporting: bool,
    auto_reconnect: bool,
}

impl Default for TerminalBehaviour {
    fn default() -> Self {
        Self {
            option_as_meta: false,
            font_family: String::new(),
            ligatures: false,
            line_height: default_line_height(),
            cursor_style: CursorStyle::default(),
            cursor_blink: true,
            warn_multiline_paste: true,
            right_click: RightClick::Menu,
            copy_on_select: false,
            ctrl_copy_paste: default_ctrl_copy_paste(),
            osc52: true,
            mouse_reporting: true,
            auto_reconnect: true,
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
    let files = files_behaviour();
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
        terminal_ligatures: behaviour.ligatures,
        terminal_line_height: behaviour.line_height,
        terminal_cursor_style: behaviour.cursor_style,
        terminal_cursor_blink: behaviour.cursor_blink,
        terminal_warn_multiline_paste: behaviour.warn_multiline_paste,
        terminal_right_click: behaviour.right_click,
        terminal_copy_on_select: behaviour.copy_on_select,
        terminal_ctrl_copy_paste: behaviour.ctrl_copy_paste,
        terminal_osc52: behaviour.osc52,
        terminal_mouse_reporting: behaviour.mouse_reporting,
        terminal_auto_reconnect: behaviour.auto_reconnect,
        editor_command: files.editor_command,
        follow_terminal_folder: files.follow_terminal_folder,
        files_double_click: files.double_click,
        files_verify_transfers: files.verify_transfers,
        hosts_single_click: hosts_behaviour().single_click,
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
        ligatures: settings.terminal_ligatures,
        line_height: settings.terminal_line_height,
        cursor_style: settings.terminal_cursor_style,
        cursor_blink: settings.terminal_cursor_blink,
        warn_multiline_paste: settings.terminal_warn_multiline_paste,
        right_click: settings.terminal_right_click,
        copy_on_select: settings.terminal_copy_on_select,
        ctrl_copy_paste: settings.terminal_ctrl_copy_paste,
        osc52: settings.terminal_osc52,
        mouse_reporting: settings.terminal_mouse_reporting,
        auto_reconnect: settings.terminal_auto_reconnect,
    };
    if behaviour != terminal_behaviour() {
        store
            .set_setting_as(TERMINAL_BEHAVIOUR, &behaviour)
            .map_err(stored)?;
    }
    let files = FilesBehaviour {
        editor_command: settings.editor_command.trim().to_owned(),
        follow_terminal_folder: settings.follow_terminal_folder,
        double_click: settings.files_double_click,
        verify_transfers: settings.files_verify_transfers,
    };
    if files != files_behaviour() {
        store
            .set_setting_as(FILES_BEHAVIOUR, &files)
            .map_err(stored)?;
    }
    let hosts = HostsBehaviour {
        single_click: settings.hosts_single_click,
    };
    if hosts != hosts_behaviour() {
        store
            .set_setting_as(HOSTS_BEHAVIOUR, &hosts)
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
    /// Which agent the built-in engine found: `openssh`, `pageant`, `environment`.
    agent_kind: Option<&'static str>,
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

/// Runs `ssh -V` and `ssh-add -l`, then asks the agent the way the built-in
/// engine does (so Pageant on Windows shows up); slow enough to keep off the
/// main thread.
#[tauri::command]
pub async fn ssh_status() -> Result<SshStatus, String> {
    let mut status = tauri::async_runtime::spawn_blocking(|| {
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
            agent_kind: None,
            agent_error,
            ssh_config_hosts: scan_default_ssh_config().aliases.len(),
        }
    })
    .await
    .map_err(|error| error.to_string())?;
    let probe = easyssh_ssh::probe_agent(&easyssh_ssh::AgentSource::Environment).await;
    if let Some(kind) = probe.kind {
        if probe.error.is_none() {
            status.agent = if probe.keys.is_empty() {
                AgentState::Empty
            } else {
                AgentState::Ready
            };
            status.agent_keys = probe.keys;
            status.agent_kind = Some(kind.id());
            status.agent_error = None;
        }
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::{agent_state, AgentState, RightClick, TerminalBehaviour};

    #[test]
    fn terminal_behaviour_defaults_when_missing_or_partial() {
        let empty: TerminalBehaviour = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, TerminalBehaviour::default());
        assert!(empty.warn_multiline_paste && empty.osc52 && !empty.copy_on_select);
        assert!(empty.auto_reconnect);
        // Windows Terminal's Ctrl+C / Ctrl+V: on by default on Windows only.
        assert_eq!(empty.ctrl_copy_paste, cfg!(windows));
        let partial: TerminalBehaviour =
            serde_json::from_str(r#"{"right_click":"paste"}"#).unwrap();
        assert_eq!(partial.right_click, RightClick::Paste);
        assert!(partial.osc52);
        assert!(!empty.option_as_meta);
        let on: TerminalBehaviour = serde_json::from_str(r#"{"option_as_meta":true}"#).unwrap();
        assert!(on.option_as_meta);
        // Settings saved before the font options existed keep working.
        assert!(empty.font_family.is_empty());
        assert!(!empty.ligatures);
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
