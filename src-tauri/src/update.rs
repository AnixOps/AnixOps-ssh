//! Update check: the app's side. The work (feed, signatures, fetching) is in
//! `easyssh-update`; this file keeps the small local state, runs the daily
//! background check and answers the Updates row of the settings. It notifies
//! only: "Download" opens the release asset in the browser.
//!
//! The state is a local file, not a setting: settings may be synced between
//! devices, the last accepted feed sequence must not be.

use easyssh_update::{build_from_version, Checked, Update, Verifier};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri_plugin_opener::OpenerExt;

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The background check runs at most this often.
const INTERVAL_SECS: i64 = 24 * 60 * 60;

/// Checks and writes of the state file do not interleave.
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
struct State {
    /// `None` until the user chose; the default is on.
    auto_check: Option<bool>,
    /// The highest feed sequence accepted: an older feed is a replay.
    last_sequence: i64,
    /// Unix time of the last check that succeeded.
    last_checked: i64,
    found: Option<Update>,
}

impl State {
    fn auto(&self) -> bool {
        self.auto_check.unwrap_or(true)
    }

    fn due(&self, now: i64) -> bool {
        self.auto() && (now < self.last_checked || now - self.last_checked >= INTERVAL_SECS)
    }

    fn accept(&mut self, checked: Checked, now: i64) {
        self.last_sequence = self.last_sequence.max(checked.sequence);
        self.last_checked = now;
        self.found = checked.update;
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    /// The build can check: a desktop platform with a feed and a key
    /// compiled in. Otherwise the user checks the GitHub releases by hand.
    available: bool,
    auto_check: bool,
    current: String,
    /// Unix seconds of the last successful check.
    last_checked: Option<i64>,
    /// Only a build newer than this one.
    update: Option<Update>,
}

fn state_path() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("EasySSH")
        .join("update.json")
}

/// A missing or damaged file is the default state.
fn load(path: &Path) -> State {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(path: &Path, state: &State) -> Result<(), String> {
    let error = |e: std::io::Error| e.to_string();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(error)?;
    }
    let temp = path.with_extension("json.tmp");
    std::fs::write(
        &temp,
        serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?,
    )
    .map_err(error)?;
    std::fs::rename(&temp, path).map_err(error)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn current_build() -> Option<i64> {
    build_from_version(VERSION)
}

fn status_of(state: &State, available: bool, current_build: Option<i64>) -> UpdateStatus {
    UpdateStatus {
        available,
        auto_check: state.auto(),
        current: VERSION.to_owned(),
        last_checked: (state.last_checked > 0).then_some(state.last_checked),
        update: state
            .found
            .clone()
            .filter(|found| current_build.is_some_and(|build| found.build > build)),
    }
}

fn verifier() -> Option<Verifier> {
    easyssh_update::build_verifier()
}

/// One check: fetch, verify, compare, remember. Nothing changes on failure.
fn run_check(verifier: &Verifier) -> Result<State, String> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let build = current_build().ok_or("unknown app version")?;
    let product = easyssh_update::product().ok_or("no feed for this platform")?;
    let system = easyssh_update::system_version().ok_or("unknown system version")?;
    let path = state_path();
    let mut state = load(&path);
    let checked = easyssh_update::check(
        &easyssh_update::Http,
        verifier,
        &easyssh_update::feed_url(product),
        build,
        &system,
        state.last_sequence,
    )?;
    state.accept(checked, now());
    save(&path, &state)?;
    Ok(state)
}

/// Runs the daily check on a background thread when it is due. Never waits
/// for the network and never reports a failure: the user can check by hand.
pub fn start_background() {
    let Some(verifier) = verifier() else { return };
    if !load(&state_path()).due(now()) {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("update-check".into())
        .spawn(move || {
            let _ = run_check(&verifier);
        });
}

#[tauri::command]
pub fn update_status() -> UpdateStatus {
    status_of(&load(&state_path()), verifier().is_some(), current_build())
}

#[tauri::command]
pub fn update_set_auto(on: bool) -> Result<UpdateStatus, String> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let path = state_path();
    let mut state = load(&path);
    state.auto_check = Some(on);
    save(&path, &state)?;
    Ok(status_of(&state, verifier().is_some(), current_build()))
}

/// "Check now". An error names why the feed could not be used.
#[tauri::command]
pub async fn update_check() -> Result<UpdateStatus, String> {
    let verifier = verifier().ok_or("this build does not check for updates")?;
    let state = tauri::async_runtime::spawn_blocking(move || run_check(&verifier))
        .await
        .map_err(|e| e.to_string())??;
    Ok(status_of(&state, true, current_build()))
}

/// Opens the verified item's URL in the browser. The page names no URL: only
/// the one that passed verification can be opened.
#[tauri::command]
pub fn update_open(app: tauri::AppHandle) -> Result<(), String> {
    let status = update_status();
    let update = status.update.ok_or("no update to open")?;
    if !easyssh_update::feed::is_https(&update.url) {
        return Err("the update address is not HTTPS".to_owned());
    }
    app.opener()
        .open_url(update.url, None::<&str>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(build: i64) -> Update {
        Update {
            version: "2.2.0".into(),
            build,
            url: "https://github.com/AnixOps/EasySSH/releases/tag/v2.2.0".into(),
            notes: String::new(),
            published_at: None,
        }
    }

    #[test]
    fn the_check_is_on_by_default_and_runs_once_a_day() {
        let mut state = State::default();
        assert!(state.auto() && state.due(1_000_000));
        state.last_checked = 1_000_000;
        assert!(!state.due(1_000_000 + INTERVAL_SECS - 1));
        assert!(state.due(1_000_000 + INTERVAL_SECS));
        // A clock set back does not stop checks for days.
        assert!(state.due(10));
        state.auto_check = Some(false);
        assert!(!state.due(i64::MAX / 2));
    }

    #[test]
    fn the_highest_sequence_is_kept_and_a_failed_check_changes_nothing() {
        let mut state = State {
            last_sequence: 9,
            ..State::default()
        };
        state.accept(
            Checked {
                sequence: 5,
                update: None,
            },
            100,
        );
        assert_eq!(state.last_sequence, 9);
        state.accept(
            Checked {
                sequence: 12,
                update: Some(found(2_002_000)),
            },
            200,
        );
        assert_eq!((state.last_sequence, state.last_checked), (12, 200));
        assert!(state.found.is_some());
    }

    #[test]
    fn an_update_already_installed_is_not_shown() {
        let state = State {
            found: Some(found(2_002_000)),
            last_checked: 5,
            ..State::default()
        };
        assert!(status_of(&state, true, Some(2_001_000)).update.is_some());
        assert!(status_of(&state, true, Some(2_002_000)).update.is_none());
        assert!(status_of(&state, true, None).update.is_none());
        assert_eq!(status_of(&state, true, None).last_checked, Some(5));
    }

    #[test]
    fn state_survives_a_restart_and_a_damaged_file_is_the_default() {
        let dir = std::env::temp_dir().join(format!("easyssh-update-test-{}", now()));
        let path = dir.join("update.json");
        let state = State {
            auto_check: Some(false),
            last_sequence: 7,
            last_checked: 8,
            found: Some(found(2_002_000)),
        };
        save(&path, &state).unwrap();
        assert_eq!(load(&path), state);
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(load(&path), State::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
