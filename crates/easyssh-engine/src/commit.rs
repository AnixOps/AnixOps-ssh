//! The last step of an upload: putting the finished temporary file in the
//! place of the target without ever leaving the original unreachable.
//!
//! A plain SFTP rename does not replace an existing file on OpenSSH (it
//! answers `Failure`). Deleting the target and renaming again loses the
//! original when the second step fails. Here the original is only ever
//! renamed, never deleted, before the new file is in place:
//!
//! 1. The server has `posix-rename@openssh.com`: one atomic step.
//! 2. Otherwise a plain rename; if that is refused because the target is
//!    there, the target is renamed to a backup next to it, the new file is
//!    renamed into place, and the backup is removed. A failure after the
//!    backup was made puts the original back; if even that fails the error
//!    names the backup so the person can get the file.

use async_trait::async_trait;
use easyssh_ssh::{EntryKind, Error as SshError, Sftp};

/// What the commit needs from the remote file system.
#[async_trait]
pub(crate) trait CommitFs: Sync {
    /// A plain rename (fails when `to` exists on OpenSSH).
    async fn rename(&self, from: &str, to: &str) -> Result<(), SshError>;
    /// Does the server replace atomically (`posix-rename@openssh.com`)?
    async fn has_posix_rename(&self) -> bool;
    async fn posix_rename(&self, from: &str, to: &str) -> Result<(), SshError>;
    /// What is at `path` (not following links); `None` when nothing is.
    async fn kind(&self, path: &str) -> Result<Option<EntryKind>, SshError>;
    async fn remove_file(&self, path: &str) -> Result<(), SshError>;
}

#[async_trait]
impl CommitFs for Sftp {
    async fn rename(&self, from: &str, to: &str) -> Result<(), SshError> {
        Sftp::rename(self, from, to).await
    }
    async fn has_posix_rename(&self) -> bool {
        self.supports_posix_rename().await
    }
    async fn posix_rename(&self, from: &str, to: &str) -> Result<(), SshError> {
        Sftp::posix_rename(self, from, to).await
    }
    async fn kind(&self, path: &str) -> Result<Option<EntryKind>, SshError> {
        match self.lstat(path).await {
            Ok(stat) => Ok(Some(stat.kind)),
            Err(SshError::NotFound(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }
    async fn remove_file(&self, path: &str) -> Result<(), SshError> {
        Sftp::remove_file(self, path).await
    }
}

/// `.<name>.easyssh-bak-<token>` in the folder of `target`.
pub(crate) fn backup_path(target: &str, token: &str) -> String {
    let (dir, name) = match target.rfind('/') {
        Some(at) => (&target[..=at], &target[at + 1..]),
        None => ("", target),
    };
    format!("{dir}.{name}.easyssh-bak-{token}")
}

fn failed(what: &str, path: &str, error: &SshError) -> SshError {
    SshError::Sftp(format!("{what} {path}: {error}"))
}

/// Puts `part` at `target`, replacing what is there. `token` makes the backup
/// name unique. On any error the original is still reachable: at `target`,
/// or at the backup path the error names.
pub(crate) async fn commit<F: CommitFs>(
    fs: &F,
    part: &str,
    target: &str,
    token: &str,
) -> Result<(), SshError> {
    if fs.has_posix_rename().await {
        // Atomic: on an error the target is as it was.
        return fs
            .posix_rename(part, target)
            .await
            .map_err(|error| failed("renaming to", target, &error));
    }
    let first = match fs.rename(part, target).await {
        Ok(()) => return Ok(()),
        Err(error) => error,
    };
    // Only "failure" is how a server says the target is in the way; a
    // permission error, a lost connection or a timeout is reported as it is.
    if !matches!(first, SshError::SftpFailure(_)) {
        return Err(failed("renaming to", target, &first));
    }
    // The earlier look at the target may be stale: ask again.
    match fs.kind(target).await {
        Ok(Some(EntryKind::File | EntryKind::Symlink)) => {}
        _ => return Err(failed("renaming to", target, &first)),
    }
    replace(fs, part, target, &backup_path(target, token), &first).await
}

async fn replace<F: CommitFs>(
    fs: &F,
    part: &str,
    target: &str,
    backup: &str,
    first: &SshError,
) -> Result<(), SshError> {
    // Step 1: the original moves aside.
    if let Err(error) = fs.rename(target, backup).await {
        // The answer may have been lost after the server did it.
        match (fs.kind(target).await, fs.kind(backup).await) {
            (Ok(None), Ok(Some(_))) => {}
            (Ok(Some(_)), _) => {
                return Err(SshError::Sftp(format!(
                    "{target} already exists and could not be replaced ({first}; moving it aside: \
                     {error}); the original was kept"
                )))
            }
            _ => {
                return Err(SshError::Sftp(format!(
                    "replacing {target}: {error}; the connection did not say whether the original \
                     is still there or at {backup}"
                )))
            }
        }
    }
    // Step 2: the new file takes its place.
    if let Err(error) = fs.rename(part, target).await {
        // Same doubt: if the part is gone and the target is there it worked.
        if matches!(fs.kind(part).await, Ok(None)) && matches!(fs.kind(target).await, Ok(Some(_))) {
            let _ = fs.remove_file(backup).await;
            return Ok(());
        }
        return match fs.rename(backup, target).await {
            Ok(()) => Err(SshError::Sftp(format!(
                "renaming to {target}: {error}; the original was kept"
            ))),
            Err(restore) => Err(SshError::Sftp(format!(
                "renaming to {target}: {error}; putting the original back failed ({restore}): \
                 the original is kept at {backup}"
            ))),
        };
    }
    // Step 3: the original is no longer needed. If this fails the new file
    // is in place and the old one stays beside it, harmless.
    let _ = fs.remove_file(backup).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    const TOKEN: &str = "t0";
    const PART: &str = "/srv/.a.txt.part";
    const TARGET: &str = "/srv/a.txt";
    const BACKUP: &str = "/srv/.a.txt.easyssh-bak-t0";

    /// A remote folder in memory with OpenSSH's rename rules and faults that
    /// can be injected into any call.
    #[derive(Default)]
    struct Fake {
        files: Mutex<HashMap<String, Vec<u8>>>,
        dirs: Mutex<Vec<String>>,
        posix: bool,
        faults: Mutex<Vec<Fault>>,
        calls: Mutex<HashMap<&'static str, usize>>,
    }

    /// The `nth` call (from 1) of `op` fails with `error`. With `applied`
    /// the server did the work and only the answer was lost.
    struct Fault {
        op: &'static str,
        nth: usize,
        error: fn() -> SshError,
        applied: bool,
    }

    fn denied() -> SshError {
        SshError::PermissionDenied("Permission denied".into())
    }
    fn dropped() -> SshError {
        SshError::Network("connection lost".into())
    }

    impl Fake {
        fn new(files: &[(&str, &str)]) -> Fake {
            let fake = Fake::default();
            for (path, text) in files {
                fake.files
                    .lock()
                    .unwrap()
                    .insert((*path).to_owned(), text.as_bytes().to_vec());
            }
            fake
        }
        fn with_posix(mut self) -> Fake {
            self.posix = true;
            self
        }
        fn fail(&self, op: &'static str, nth: usize, error: fn() -> SshError, applied: bool) {
            self.faults.lock().unwrap().push(Fault {
                op,
                nth,
                error,
                applied,
            });
        }
        /// The network is fine again (and the call counts start over).
        fn heal(&self) {
            self.faults.lock().unwrap().clear();
            self.calls.lock().unwrap().clear();
        }
        fn due(&self, op: &'static str) -> Option<(SshError, bool)> {
            let mut calls = self.calls.lock().unwrap();
            let n = calls.entry(op).or_default();
            *n += 1;
            let n = *n;
            self.faults
                .lock()
                .unwrap()
                .iter()
                .find(|f| f.op == op && f.nth == n)
                .map(|f| ((f.error)(), f.applied))
        }
        fn text(&self, path: &str) -> Option<String> {
            self.files
                .lock()
                .unwrap()
                .get(path)
                .map(|bytes| String::from_utf8(bytes.clone()).unwrap())
        }
        fn names(&self) -> Vec<String> {
            let mut names: Vec<_> = self.files.lock().unwrap().keys().cloned().collect();
            names.sort();
            names
        }
        fn do_move(&self, from: &str, to: &str, replace: bool) -> Result<(), SshError> {
            let mut files = self.files.lock().unwrap();
            if !files.contains_key(from) {
                return Err(SshError::NotFound(from.into()));
            }
            let in_the_way =
                files.contains_key(to) || self.dirs.lock().unwrap().contains(&to.to_owned());
            if !replace && in_the_way {
                return Err(SshError::SftpFailure("Failure".into()));
            }
            let data = files.remove(from).unwrap();
            files.insert(to.to_owned(), data);
            Ok(())
        }
        fn go(
            &self,
            op: &'static str,
            from: &str,
            to: &str,
            replace: bool,
        ) -> Result<(), SshError> {
            match self.due(op) {
                Some((error, true)) => {
                    let _ = self.do_move(from, to, replace);
                    Err(error)
                }
                Some((error, false)) => Err(error),
                None => self.do_move(from, to, replace),
            }
        }
    }

    #[async_trait]
    impl CommitFs for Fake {
        async fn rename(&self, from: &str, to: &str) -> Result<(), SshError> {
            self.go("rename", from, to, false)
        }
        async fn has_posix_rename(&self) -> bool {
            self.posix
        }
        async fn posix_rename(&self, from: &str, to: &str) -> Result<(), SshError> {
            self.go("posix", from, to, true)
        }
        async fn kind(&self, path: &str) -> Result<Option<EntryKind>, SshError> {
            if let Some((error, _)) = self.due("kind") {
                return Err(error);
            }
            if self.dirs.lock().unwrap().contains(&path.to_owned()) {
                return Ok(Some(EntryKind::Dir));
            }
            Ok(self
                .files
                .lock()
                .unwrap()
                .contains_key(path)
                .then_some(EntryKind::File))
        }
        async fn remove_file(&self, path: &str) -> Result<(), SshError> {
            if let Some((error, _)) = self.due("remove") {
                return Err(error);
            }
            self.files.lock().unwrap().remove(path);
            Ok(())
        }
    }

    fn fixture() -> Fake {
        Fake::new(&[(TARGET, "old"), (PART, "new")])
    }

    /// The original text is reachable at the target or at the backup.
    fn original_reachable(fake: &Fake) -> bool {
        fake.text(TARGET).as_deref() == Some("old") || fake.text(BACKUP).as_deref() == Some("old")
    }

    #[test]
    fn the_backup_sits_beside_the_target_and_is_hidden() {
        assert_eq!(backup_path("/srv/a.txt", "x"), "/srv/.a.txt.easyssh-bak-x");
        assert_eq!(backup_path("a.txt", "x"), ".a.txt.easyssh-bak-x");
    }

    #[tokio::test]
    async fn a_new_file_is_a_plain_rename() {
        let fake = Fake::new(&[(PART, "new")]);
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.names(), [TARGET]);
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn an_unsupported_overwrite_goes_through_a_backup_and_leaves_none() {
        let fake = fixture();
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.names(), [TARGET]);
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn posix_rename_replaces_in_one_step_and_an_error_leaves_the_original() {
        let fake = fixture().with_posix();
        fake.fail("posix", 1, dropped, false);
        let error = commit(&fake, PART, TARGET, TOKEN).await.unwrap_err();
        assert!(error.to_string().contains("connection lost"), "{error}");
        assert_eq!(fake.text(TARGET).as_deref(), Some("old"));
        assert_eq!(fake.text(PART).as_deref(), Some("new"));
        fake.heal();
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.names(), [TARGET]);
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn permission_denied_is_reported_as_it_is_and_nothing_moves() {
        let fake = fixture();
        fake.fail("rename", 1, denied, false);
        let error = commit(&fake, PART, TARGET, TOKEN).await.unwrap_err();
        assert!(error.to_string().contains("Permission denied"), "{error}");
        assert!(!error.to_string().contains("easyssh-bak"), "{error}");
        assert_eq!(fake.names(), [PART, TARGET]);
        assert_eq!(fake.text(TARGET).as_deref(), Some("old"));
        fake.heal();
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn a_refusal_without_a_target_is_not_a_replace() {
        let fake = Fake::new(&[(PART, "new")]);
        fake.fail(
            "rename",
            1,
            || SshError::SftpFailure("Failure".into()),
            false,
        );
        assert!(commit(&fake, PART, TARGET, TOKEN).await.is_err());
        assert_eq!(fake.names(), [PART]);
    }

    #[tokio::test]
    async fn a_folder_in_the_way_is_never_moved_aside() {
        let fake = Fake::new(&[(PART, "new")]);
        fake.dirs.lock().unwrap().push(TARGET.to_owned());
        assert!(commit(&fake, PART, TARGET, TOKEN).await.is_err());
        assert_eq!(fake.names(), [PART]);
    }

    #[tokio::test]
    async fn a_failure_moving_the_original_aside_keeps_it_in_place() {
        let fake = fixture();
        // rename #1 is the refused plain one, #2 moves the original aside.
        fake.fail("rename", 2, denied, false);
        let error = commit(&fake, PART, TARGET, TOKEN).await.unwrap_err();
        assert!(error.to_string().contains("original was kept"), "{error}");
        assert_eq!(fake.text(TARGET).as_deref(), Some("old"));
        assert_eq!(fake.text(PART).as_deref(), Some("new"));
        fake.heal();
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
        assert_eq!(fake.names(), [TARGET]);
    }

    #[tokio::test]
    async fn a_failure_putting_the_new_file_in_place_restores_the_original() {
        let fake = fixture();
        fake.fail("rename", 3, dropped, false);
        let error = commit(&fake, PART, TARGET, TOKEN).await.unwrap_err();
        assert!(error.to_string().contains("original was kept"), "{error}");
        assert_eq!(fake.text(TARGET).as_deref(), Some("old"));
        assert_eq!(fake.text(PART).as_deref(), Some("new"));
        assert_eq!(fake.names(), [PART, TARGET], "no backup left behind");
        fake.heal();
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn when_restoring_fails_too_the_error_names_the_backup() {
        let fake = fixture();
        fake.fail("rename", 3, dropped, false);
        fake.fail("rename", 4, dropped, false);
        let error = commit(&fake, PART, TARGET, TOKEN).await.unwrap_err();
        assert!(error.to_string().contains(BACKUP), "{error}");
        assert!(original_reachable(&fake));
        assert_eq!(fake.text(BACKUP).as_deref(), Some("old"));
        // A retry works: the target is free and the part is intact.
        fake.heal();
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
        assert_eq!(fake.text(BACKUP).as_deref(), Some("old"));
    }

    #[tokio::test]
    async fn a_lost_answer_after_the_new_file_was_put_in_place_is_a_success() {
        let fake = fixture();
        fake.fail("rename", 3, dropped, true);
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
        assert_eq!(fake.names(), [TARGET]);
    }

    #[tokio::test]
    async fn a_lost_answer_after_the_original_moved_aside_carries_on() {
        let fake = fixture();
        fake.fail("rename", 2, dropped, true);
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
        assert_eq!(fake.names(), [TARGET]);
    }

    #[tokio::test]
    async fn a_dead_connection_after_the_original_moved_names_the_backup() {
        let fake = fixture();
        // Moving aside works but its answer is lost, and so is every look.
        fake.fail("rename", 2, dropped, true);
        fake.fail("kind", 2, dropped, false);
        let error = commit(&fake, PART, TARGET, TOKEN).await.unwrap_err();
        assert!(error.to_string().contains(BACKUP), "{error}");
        assert!(original_reachable(&fake));
    }

    #[tokio::test]
    async fn a_failure_removing_the_backup_keeps_the_new_file_and_succeeds() {
        let fake = fixture();
        fake.fail("remove", 1, dropped, false);
        commit(&fake, PART, TARGET, TOKEN).await.unwrap();
        assert_eq!(fake.text(TARGET).as_deref(), Some("new"));
        assert_eq!(fake.text(BACKUP).as_deref(), Some("old"));
    }
}
