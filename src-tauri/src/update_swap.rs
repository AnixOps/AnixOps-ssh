//! Replacing the running macOS `.app` bundle, recoverably, and the file mode
//! of a replaced AppImage. No Tauri here: the file system goes through
//! [`SwapFs`] so the tests can cut the sequence anywhere.
//!
//! The swap prefers one atomic exchange (`renamex_np` with `RENAME_SWAP`):
//! the official path never goes missing. Where that is not available it falls
//! back to two renames with a *unique* backup name and a marker file beside
//! it. The marker says which backup belongs to which bundle, so
//! [`recover`] (run before the next install and at startup) can put the old
//! version back if the process died between the renames, or finish the
//! cleanup if it died after. A backup is deleted only once the official path
//! holds a complete app (`Contents/Info.plist`).

#![allow(dead_code)] // the bundle swap is used on macOS only

use std::io;
use std::path::{Path, PathBuf};

/// The file system operations the swap uses.
pub trait SwapFs {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Exchanges two directories in one step. `Unsupported` when the file
    /// system or the OS cannot.
    fn swap(&self, a: &Path, b: &Path) -> io::Result<()>;
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn write(&self, path: &Path, contents: &str) -> io::Result<()>;
    fn read(&self, path: &Path) -> io::Result<String>;
    fn exists(&self, path: &Path) -> bool;
}

/// The real file system.
pub struct RealFs;

impl SwapFs for RealFs {
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    #[cfg(target_os = "macos")]
    fn swap(&self, a: &Path, b: &Path) -> io::Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let c = |p: &Path| {
            std::ffi::CString::new(p.as_os_str().as_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
        };
        let (a, b) = (c(a)?, c(b)?);
        // SAFETY: both are valid NUL-terminated paths that outlive the call.
        let rc = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn swap(&self, _: &Path, _: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir_all(path)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn read(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

/// A bundle that can be opened: the official entry is "present" only with this.
fn complete(fs: &dyn SwapFs, bundle: &Path) -> bool {
    fs.exists(&bundle.join("Contents").join("Info.plist"))
}

/// The marker next to the bundle.
pub fn marker_path(parent: &Path, name: &str) -> PathBuf {
    parent.join(format!(".{name}.swap"))
}

/// Where the new version is staged before the swap.
pub fn staged_path(parent: &Path, name: &str) -> PathBuf {
    parent.join(format!(".{name}.update"))
}

fn backup_path(parent: &Path, name: &str, unique: &str) -> PathBuf {
    parent.join(format!(".{name}.old-{unique}"))
}

/// Marker text: the phase and the backup it belongs to.
fn marker_text(phase: &str, backup: &Path) -> String {
    format!("{phase}\n{}\n", backup.display())
}

fn parse_marker(text: &str) -> Option<PathBuf> {
    let mut lines = text.lines();
    lines.next()?;
    let backup = lines.next()?;
    (!backup.is_empty()).then(|| PathBuf::from(backup))
}

/// Puts `staged` in the place of `bundle`. `unique` makes the backup name
/// (a pid and a time, say). On error the old app is in place, or, if even
/// that failed, the error names the backup that still holds it.
pub fn replace_bundle(
    fs: &dyn SwapFs,
    bundle: &Path,
    staged: &Path,
    unique: &str,
) -> Result<(), String> {
    let parent = bundle.parent().ok_or("the app has no folder")?;
    let name = bundle
        .file_name()
        .ok_or("the app has no name")?
        .to_string_lossy()
        .into_owned();
    // An earlier interrupted swap is put right first; if that cannot be done
    // nothing more is touched.
    recover(fs, parent, &name)?;

    // One atomic exchange: afterwards `staged` holds the old app.
    if fs.swap(staged, bundle).is_ok() {
        if complete(fs, bundle) {
            let _ = fs.remove_dir_all(staged);
            return Ok(());
        }
        // The new app is unusable: exchange back.
        return match fs.swap(staged, bundle) {
            Ok(()) => {
                let _ = fs.remove_dir_all(staged);
                Err("the new version is incomplete; the old one is kept".into())
            }
            Err(e) => Err(format!(
                "the new version is incomplete and the old one could not be restored ({e}); it is at {}",
                staged.display()
            )),
        };
    }

    let backup = backup_path(parent, &name, unique);
    let marker = marker_path(parent, &name);
    fs.write(&marker, &marker_text("moving", &backup))
        .map_err(|e| format!("cannot replace the app: {}: {e}", marker.display()))?;
    if let Err(e) = fs.rename(bundle, &backup) {
        let _ = fs.remove_file(&marker);
        let _ = fs.remove_dir_all(staged);
        return Err(format!("cannot replace the app: {e}"));
    }
    let moved = fs.rename(staged, bundle).and_then(|()| {
        if complete(fs, bundle) {
            Ok(())
        } else {
            Err(io::Error::other("the new version is incomplete"))
        }
    });
    if let Err(e) = moved {
        // Roll back. A bundle that was moved in but is unusable is set aside.
        if fs.exists(bundle) {
            let _ = fs.rename(bundle, staged);
        }
        return match fs.rename(&backup, bundle) {
            Ok(()) => {
                let _ = fs.remove_file(&marker);
                let _ = fs.remove_dir_all(staged);
                Err(format!("cannot replace the app: {e}"))
            }
            // The marker and the backup stay for `recover`.
            Err(rollback) => Err(format!(
                "cannot replace the app: {e}; and the old version could not be put back ({rollback}). It is kept at {}",
                backup.display()
            )),
        };
    }
    // The official entry is verified: the old copy can go.
    if fs.remove_dir_all(&backup).is_ok() {
        let _ = fs.remove_file(&marker);
    }
    Ok(())
}

/// Finishes or undoes a swap that was cut short. Safe to call any time.
pub fn recover(fs: &dyn SwapFs, parent: &Path, name: &str) -> Result<(), String> {
    let marker = marker_path(parent, name);
    let Ok(text) = fs.read(&marker) else {
        return Ok(());
    };
    let bundle = parent.join(name);
    let Some(backup) = parse_marker(&text) else {
        // Unreadable marker: nothing to restore from; do not keep blocking.
        let _ = fs.remove_file(&marker);
        return Ok(());
    };
    if complete(fs, &bundle) {
        // The new version is in place (the cut came after the second rename).
        if fs.exists(&backup) && fs.remove_dir_all(&backup).is_err() {
            return Ok(()); // cleaned up next time
        }
        let _ = fs.remove_file(&marker);
        return Ok(());
    }
    if !fs.exists(&backup) {
        let _ = fs.remove_file(&marker);
        return Err(format!(
            "the app is missing at {} and no backup was found; install it again",
            bundle.display()
        ));
    }
    // A half-moved bundle is set aside rather than deleted over the backup.
    if fs.exists(&bundle) {
        let aside = staged_path(parent, name);
        let _ = fs.remove_dir_all(&aside);
        fs.rename(&bundle, &aside).map_err(|e| {
            format!(
                "cannot restore the app: {}: {e}; the old version is at {}",
                bundle.display(),
                backup.display()
            )
        })?;
    }
    fs.rename(&backup, &bundle).map_err(|e| {
        format!(
            "cannot restore the app: {e}; the old version is at {}",
            backup.display()
        )
    })?;
    let _ = fs.remove_file(&marker);
    Ok(())
}

/// The mode of the replaced AppImage: what the old file had, and the owner
/// may run it. Group and other bits are never widened.
pub fn appimage_mode(old: u32) -> u32 {
    (old & 0o7777) | 0o100
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The real file system with a fault: the nth mutating call fails and,
    /// with `dead`, so does every later one, as after a crash.
    struct Faulty {
        calls: Cell<usize>,
        fail_at: Option<usize>,
        dead: bool,
        atomic: bool,
    }

    impl Faulty {
        fn new() -> Self {
            Faulty {
                calls: Cell::new(0),
                fail_at: None,
                dead: false,
                atomic: false,
            }
        }
        fn crash_at(n: usize) -> Self {
            Faulty {
                fail_at: Some(n),
                dead: true,
                ..Self::new()
            }
        }
        fn step(&self, op: &str) -> io::Result<()> {
            let n = self.calls.get() + 1;
            self.calls.set(n);
            if self
                .fail_at
                .is_some_and(|at| n == at || (self.dead && n > at))
            {
                Err(io::Error::other(format!("injected {op} failure")))
            } else {
                Ok(())
            }
        }
    }

    impl SwapFs for Faulty {
        fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
            self.step("rename")?;
            std::fs::rename(a, b)
        }
        fn swap(&self, a: &Path, b: &Path) -> io::Result<()> {
            if !self.atomic {
                return Err(io::ErrorKind::Unsupported.into());
            }
            // Emulates the exchange (the test file system has no such call).
            let tmp = a.with_extension("swaptmp");
            std::fs::rename(a, &tmp)?;
            std::fs::rename(b, a)?;
            std::fs::rename(&tmp, b)
        }
        fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
            self.step("remove")?;
            std::fs::remove_dir_all(p)
        }
        fn remove_file(&self, p: &Path) -> io::Result<()> {
            self.step("remove_file")?;
            std::fs::remove_file(p)
        }
        fn write(&self, p: &Path, c: &str) -> io::Result<()> {
            self.step("write")?;
            std::fs::write(p, c)
        }
        fn read(&self, p: &Path) -> io::Result<String> {
            std::fs::read_to_string(p)
        }
        fn exists(&self, p: &Path) -> bool {
            p.exists()
        }
    }

    fn app(dir: &Path, name: &str, version: &str) -> PathBuf {
        let app = dir.join(name);
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::write(app.join("Contents").join("Info.plist"), version).unwrap();
        app
    }

    fn version(app: &Path) -> String {
        std::fs::read_to_string(app.join("Contents").join("Info.plist")).unwrap()
    }

    const NAME: &str = "AnixOps SSH.app";

    fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let bundle = app(dir.path(), NAME, "old");
        let staged = app(dir.path(), &format!(".{NAME}.update"), "new");
        (dir, bundle, staged)
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != NAME)
            .collect();
        names.sort();
        names
    }

    #[test]
    fn two_renames_install_the_new_version_and_clean_up() {
        let (dir, bundle, staged) = setup();
        replace_bundle(&Faulty::new(), &bundle, &staged, "1").unwrap();
        assert_eq!(version(&bundle), "new");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn the_atomic_swap_is_used_when_available() {
        let (dir, bundle, staged) = setup();
        let fs = Faulty {
            atomic: true,
            ..Faulty::new()
        };
        replace_bundle(&fs, &bundle, &staged, "1").unwrap();
        assert_eq!(version(&bundle), "new");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn a_cut_between_the_renames_is_recovered_on_the_next_start() {
        // Calls: 1 write marker, 2 rename bundle->backup, 3 rename staged->bundle.
        let (dir, bundle, staged) = setup();
        let error = replace_bundle(&Faulty::crash_at(3), &bundle, &staged, "1").unwrap_err();
        assert!(error.contains("kept at"), "{error}");
        // The process died: the official path is missing, the backup and the
        // marker are on disk.
        assert!(!bundle.exists());
        assert!(marker_path(dir.path(), NAME).exists());
        // A fresh process recovers.
        recover(&Faulty::new(), dir.path(), NAME).unwrap();
        assert_eq!(version(&bundle), "old");
        assert!(!marker_path(dir.path(), NAME).exists());
        // And a new install works afterwards.
        replace_bundle(&Faulty::new(), &bundle, &staged, "2").unwrap();
        assert_eq!(version(&bundle), "new");
    }

    #[test]
    fn a_cut_after_the_second_rename_finishes_the_cleanup() {
        // Calls: 1 write, 2 rename, 3 rename, 4 remove the backup (dies).
        let (dir, bundle, staged) = setup();
        replace_bundle(&Faulty::crash_at(4), &bundle, &staged, "1").unwrap();
        assert_eq!(version(&bundle), "new");
        assert!(marker_path(dir.path(), NAME).exists());
        recover(&Faulty::new(), dir.path(), NAME).unwrap();
        assert_eq!(version(&bundle), "new");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn a_failed_second_rename_rolls_back() {
        let (dir, bundle, staged) = setup();
        let fs = Faulty {
            fail_at: Some(3),
            ..Faulty::new()
        };
        let error = replace_bundle(&fs, &bundle, &staged, "1").unwrap_err();
        assert!(error.contains("cannot replace the app"));
        assert_eq!(version(&bundle), "old");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn a_failed_rollback_keeps_the_backup_and_names_it() {
        let (dir, bundle, staged) = setup();
        // The second rename fails and so does the rollback after it.
        let error = replace_bundle(&Faulty::crash_at(3), &bundle, &staged, "9").unwrap_err();
        let backup = backup_path(dir.path(), NAME, "9");
        assert!(backup.exists(), "the backup must survive");
        assert_eq!(version(&backup), "old");
        assert!(error.contains(&backup.display().to_string()), "{error}");
        assert!(marker_path(dir.path(), NAME).exists());
    }

    #[test]
    fn the_next_install_recovers_before_it_touches_any_backup() {
        let (dir, bundle, staged) = setup();
        let _ = replace_bundle(&Faulty::crash_at(3), &bundle, &staged, "1");
        assert!(!bundle.exists());
        let backup = backup_path(dir.path(), NAME, "1");
        assert!(backup.exists());
        std::fs::remove_dir_all(&staged).unwrap();
        let staged = app(dir.path(), &format!(".{NAME}.update"), "newer");
        // Recovery itself fails: nothing is deleted, the install stops.
        let broken = Faulty {
            fail_at: Some(1),
            dead: true,
            ..Faulty::new()
        };
        let error = replace_bundle(&broken, &bundle, &staged, "2").unwrap_err();
        assert!(error.contains(&backup.display().to_string()), "{error}");
        assert!(backup.exists());
        // With a working file system the install goes through.
        replace_bundle(&Faulty::new(), &bundle, &staged, "2").unwrap();
        assert_eq!(version(&bundle), "newer");
    }

    #[test]
    fn an_incomplete_new_app_is_not_accepted() {
        let (dir, bundle, staged) = setup();
        std::fs::remove_file(staged.join("Contents").join("Info.plist")).unwrap();
        let error = replace_bundle(&Faulty::new(), &bundle, &staged, "1").unwrap_err();
        assert!(error.contains("incomplete"), "{error}");
        assert_eq!(version(&bundle), "old");
        assert!(leftovers(dir.path()).is_empty());
    }

    #[test]
    fn recovery_without_a_marker_does_nothing() {
        let (dir, bundle, _) = setup();
        recover(&Faulty::new(), dir.path(), NAME).unwrap();
        assert_eq!(version(&bundle), "old");
    }

    #[test]
    fn appimage_modes_never_widen_group_or_other() {
        for (old, new) in [
            (0o700, 0o700),
            (0o750, 0o750),
            (0o755, 0o755),
            (0o600, 0o700),
            (0o640, 0o740),
            (0o644, 0o744),
        ] {
            assert_eq!(appimage_mode(old), new, "{old:o}");
        }
    }
}
