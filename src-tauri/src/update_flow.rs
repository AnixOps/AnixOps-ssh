//! The state of an in-app update, with no Tauri in it so it can be tested:
//! which phase the install is in, and what happens when starting the new
//! version fails. Once the files are replaced the install is *done*; a failed
//! restart leaves the recoverable phase `restartFailed`, from which the page
//! can ask to start again (never to install again) or tell the user to open
//! the app by hand.

use serde::Serialize;
use std::ffi::OsString;
use std::sync::{Mutex, MutexGuard};

/// Where the install is: shown while it runs, read again after the page
/// was closed and opened.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// "downloading", "verifying", "installing", "restarting",
    /// "restartFailed" (installed, the new version did not start); `None`
    /// when idle.
    pub phase: Option<&'static str>,
    pub downloaded: u64,
    pub total: u64,
    /// Why the restart failed (phase "restartFailed").
    pub error: Option<String>,
}

pub const IDLE: Progress = Progress {
    phase: None,
    downloaded: 0,
    total: 0,
    error: None,
};

pub const RESTART_FAILED: &str = "restartFailed";

/// How the program can start again once the old process is gone.
#[allow(dead_code)] // each platform uses one
#[derive(Debug, Clone, PartialEq)]
pub enum Relaunch {
    /// The installer starts it (Windows `/R`).
    ByInstaller,
    /// Run this command once this process has exited.
    After(Vec<OsString>),
}

/// Starts the new version once this process is gone.
pub trait Launcher {
    fn spawn(&self, command: &[OsString]) -> Result<(), String>;
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// The install phase and, after a failed restart, the command to try again.
pub struct Machine {
    progress: Mutex<Progress>,
    pending: Mutex<Option<Vec<OsString>>>,
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

impl Machine {
    pub const fn new() -> Self {
        Machine {
            progress: Mutex::new(IDLE),
            pending: Mutex::new(None),
        }
    }

    pub fn get(&self) -> Progress {
        lock(&self.progress).clone()
    }

    /// Stores and announces `next`.
    pub fn set(&self, next: Progress, emit: &dyn Fn(&Progress)) {
        *lock(&self.progress) = next.clone();
        emit(&next);
    }

    /// Takes the install slot: only one install, and none while a finished
    /// install waits for its restart.
    pub fn begin(&self) -> Result<(), String> {
        let mut progress = lock(&self.progress);
        if progress.phase == Some(RESTART_FAILED) {
            return Err("the update is installed; restart the app instead".into());
        }
        if progress.phase.is_some() {
            return Err("an update is already being installed".into());
        }
        progress.phase = Some("downloading");
        Ok(())
    }

    /// The files are replaced: start the new version. On failure the phase
    /// becomes `restartFailed` (not stuck in "restarting") and the error is
    /// returned. `Ok` means the caller should now quit.
    pub fn restart(
        &self,
        relaunch: Relaunch,
        launcher: &dyn Launcher,
        emit: &dyn Fn(&Progress),
    ) -> Result<(), String> {
        self.set(
            Progress {
                phase: Some("restarting"),
                error: None,
                ..self.get()
            },
            emit,
        );
        let Relaunch::After(command) = relaunch else {
            return Ok(());
        };
        self.spawn(command, launcher, emit)
    }

    /// Starts the new version again after a failed restart. Does not touch
    /// the installed files.
    pub fn retry_restart(
        &self,
        launcher: &dyn Launcher,
        emit: &dyn Fn(&Progress),
    ) -> Result<(), String> {
        let command = {
            let mut progress = lock(&self.progress);
            if progress.phase != Some(RESTART_FAILED) {
                return Err("there is no finished update to restart".into());
            }
            let Some(command) = lock(&self.pending).take() else {
                return Err("there is no finished update to restart".into());
            };
            // Taken under the lock: a second click finds "restarting".
            progress.phase = Some("restarting");
            progress.error = None;
            command
        };
        emit(&self.get());
        self.spawn(command, launcher, emit)
    }

    fn spawn(
        &self,
        command: Vec<OsString>,
        launcher: &dyn Launcher,
        emit: &dyn Fn(&Progress),
    ) -> Result<(), String> {
        match launcher.spawn(&command) {
            Ok(()) => Ok(()),
            Err(error) => {
                *lock(&self.pending) = Some(command);
                self.set(
                    Progress {
                        phase: Some(RESTART_FAILED),
                        error: Some(error.clone()),
                        ..self.get()
                    },
                    emit,
                );
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    /// Fails the first `failures` spawns, counts every call.
    struct Fake {
        failures: Cell<usize>,
        spawned: RefCell<Vec<Vec<OsString>>>,
    }

    impl Fake {
        fn failing(times: usize) -> Self {
            Fake {
                failures: Cell::new(times),
                spawned: RefCell::new(Vec::new()),
            }
        }
    }

    impl Launcher for Fake {
        fn spawn(&self, command: &[OsString]) -> Result<(), String> {
            self.spawned.borrow_mut().push(command.to_vec());
            if self.failures.get() > 0 {
                self.failures.set(self.failures.get() - 1);
                Err("cannot restart: no /bin/sh".into())
            } else {
                Ok(())
            }
        }
    }

    fn command() -> Relaunch {
        Relaunch::After(vec!["/usr/bin/open".into(), "-n".into()])
    }

    fn installed(machine: &Machine) {
        machine.begin().unwrap();
        machine.set(
            Progress {
                phase: Some("installing"),
                downloaded: 5,
                total: 5,
                error: None,
            },
            &|_| {},
        );
    }

    #[test]
    fn a_new_machine_is_idle() {
        assert_eq!(Machine::new().get(), IDLE);
    }

    #[test]
    fn a_spawn_failure_leaves_a_recoverable_state_not_a_stuck_one() {
        let machine = Machine::new();
        installed(&machine);
        let launcher = Fake::failing(1);
        let events = RefCell::new(Vec::new());
        let error = machine
            .restart(command(), &launcher, &|p| {
                events.borrow_mut().push(p.clone())
            })
            .unwrap_err();
        assert!(error.contains("cannot restart"));
        let state = machine.get();
        assert_eq!(state.phase, Some(RESTART_FAILED));
        assert_eq!(state.error.as_deref(), Some(error.as_str()));
        // The page was told, last, that it is restartFailed.
        assert_eq!(events.borrow().last().unwrap().phase, Some(RESTART_FAILED));
        // A new install is refused with a message that says why.
        assert!(machine.begin().unwrap_err().contains("restart"));
    }

    #[test]
    fn retrying_restarts_only_and_does_not_install_again() {
        let machine = Machine::new();
        installed(&machine);
        let launcher = Fake::failing(1);
        machine.restart(command(), &launcher, &|_| {}).unwrap_err();
        assert_eq!(launcher.spawned.borrow().len(), 1);
        machine.retry_restart(&launcher, &|_| {}).unwrap();
        // Spawned again with the same command; nothing else ran (the fake
        // launcher is the only side effect there is).
        assert_eq!(launcher.spawned.borrow().len(), 2);
        assert_eq!(launcher.spawned.borrow()[0], launcher.spawned.borrow()[1]);
        let state = machine.get();
        assert_eq!(state.phase, Some("restarting"));
        assert_eq!(state.error, None);
        // Nothing left to retry.
        assert!(machine.retry_restart(&launcher, &|_| {}).is_err());
    }

    #[test]
    fn a_failing_retry_stays_recoverable() {
        let machine = Machine::new();
        installed(&machine);
        let launcher = Fake::failing(2);
        machine.restart(command(), &launcher, &|_| {}).unwrap_err();
        machine.retry_restart(&launcher, &|_| {}).unwrap_err();
        assert_eq!(machine.get().phase, Some(RESTART_FAILED));
        machine.retry_restart(&launcher, &|_| {}).unwrap();
    }

    #[test]
    fn retry_is_refused_unless_a_restart_failed() {
        let machine = Machine::new();
        let launcher = Fake::failing(0);
        assert!(machine.retry_restart(&launcher, &|_| {}).is_err());
        installed(&machine);
        assert!(machine.retry_restart(&launcher, &|_| {}).is_err());
        assert!(launcher.spawned.borrow().is_empty());
    }

    #[test]
    fn the_installer_restarts_by_itself() {
        let machine = Machine::new();
        installed(&machine);
        let launcher = Fake::failing(0);
        machine
            .restart(Relaunch::ByInstaller, &launcher, &|_| {})
            .unwrap();
        assert!(launcher.spawned.borrow().is_empty());
        assert_eq!(machine.get().phase, Some("restarting"));
    }
}
