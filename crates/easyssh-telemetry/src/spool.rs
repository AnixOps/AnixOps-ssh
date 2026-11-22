//! The spool: one small JSON file per pending report, in a directory of its
//! own, bounded in count, size and age. Writes never fail loudly: a full disk
//! or a read-only directory must not turn a crash into a second crash.

use crate::report::{now, Report};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const EXT: &str = "crash";
const SENDING: &str = "sending";
const STATE: &str = "state.json";
/// A claim older than this belongs to a sender that died.
const CLAIM_TTL_SECS: i64 = 3600;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_reports: usize,
    pub max_total_bytes: u64,
    pub max_report_bytes: usize,
    pub max_age_secs: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_reports: 20,
            max_total_bytes: 256 * 1024,
            max_report_bytes: 16 * 1024,
            max_age_secs: 14 * 24 * 3600,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Spool {
    dir: PathBuf,
    limits: Limits,
    /// Who this spool handle is, written into the names of the reports it
    /// claims (process id plus a per-handle counter; never contains a dot).
    owner: String,
}

fn new_owner() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "{}x{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub report: Report,
    pub bytes: u64,
}

/// Counters the uploader and the usage ping keep between runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct State {
    /// Unix seconds of recent report uploads (rate limit).
    pub sent: Vec<i64>,
    /// No upload before this time (back-off after a failure).
    pub retry_after: i64,
    /// `YYYY-MM-DD` of the last usage ping.
    pub usage_last_sent: String,
    /// Day the session counter below belongs to.
    pub usage_day: String,
    pub usage_sessions: u32,
    /// Sessions of the last finished day, reported by the next ping.
    pub usage_prev_sessions: u32,
}

impl Spool {
    pub fn new(dir: impl Into<PathBuf>) -> Spool {
        Spool {
            dir: dir.into(),
            limits: Limits::default(),
            owner: new_owner(),
        }
    }

    pub fn with_limits(dir: impl Into<PathBuf>, limits: Limits) -> Spool {
        Spool {
            dir: dir.into(),
            limits,
            owner: new_owner(),
        }
    }

    /// `<local data dir>/EasySSH/crash-reports`, the same product folder the
    /// config lives in.
    pub fn default_dir() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("EasySSH")
            .join("crash-reports")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Stores a report (bounded to `max_report_bytes`), then prunes the spool.
    pub fn write(&self, report: &Report) -> io::Result<PathBuf> {
        let mut report = report.clone();
        let mut json = serde_json::to_vec(&report)?;
        while json.len() > self.limits.max_report_bytes && !report.frames.is_empty() {
            let keep = report.frames.len() / 2;
            report.frames.truncate(keep);
            json = serde_json::to_vec(&report)?;
        }
        if json.len() > self.limits.max_report_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "report too large",
            ));
        }
        fs::create_dir_all(&self.dir)?;
        let name = format!("{:012}-{}", report.timestamp.max(0), report.id);
        let tmp = self.dir.join(format!("{name}.tmp"));
        let path = self.dir.join(format!("{name}.{EXT}"));
        fs::write(&tmp, &json)?;
        fs::rename(&tmp, &path)?;
        self.prune();
        Ok(path)
    }

    fn files(&self, ext: &str) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == ext))
            .collect();
        files.sort();
        files
    }

    /// Pending reports, oldest first, scrubbed again. Files that do not parse
    /// are removed.
    pub fn list(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        for path in self.files(EXT) {
            let parsed = fs::read(&path).ok().and_then(|b| {
                serde_json::from_slice::<Report>(&b)
                    .ok()
                    .map(|r| (r, b.len()))
            });
            match parsed {
                Some((report, len)) => out.push(Entry {
                    path,
                    report: report.scrubbed(),
                    bytes: len as u64,
                }),
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
        }
        out
    }

    fn prune(&self) {
        let cutoff = now() - self.limits.max_age_secs;
        let mut entries = self.list();
        entries.retain(|e| {
            if e.report.timestamp < cutoff {
                let _ = fs::remove_file(&e.path);
                false
            } else {
                true
            }
        });
        let mut total: u64 = entries.iter().map(|e| e.bytes).sum();
        let mut count = entries.len();
        for e in &entries {
            if count <= self.limits.max_reports && total <= self.limits.max_total_bytes {
                break;
            }
            let _ = fs::remove_file(&e.path);
            count -= 1;
            total -= e.bytes;
        }
    }

    /// Deletes every pending report (and any half-written or in-flight file).
    /// Returns how many reports were pending.
    pub fn clear(&self) -> usize {
        let n = self.files(EXT).len();
        for ext in [EXT, SENDING, "tmp"] {
            for p in self.files(ext) {
                let _ = fs::remove_file(p);
            }
        }
        n
    }

    /// Takes a report for upload so a second process does not send it too.
    /// The rename is the claim (only one caller can win it) and the new name
    /// records when and by whom: `<name>.<claim time>.<owner>.sending`.
    pub(crate) fn claim(&self, path: &Path) -> Option<PathBuf> {
        self.claim_at(path, now())
    }

    pub(crate) fn claim_at(&self, path: &Path, at: i64) -> Option<PathBuf> {
        let stem = path.file_stem()?.to_str()?;
        let claimed = path.with_file_name(format!("{stem}.{at}.{}.{SENDING}", self.owner));
        fs::rename(path, claimed.as_path()).ok()?;
        Some(claimed)
    }

    pub(crate) fn release(&self, claimed: &Path) {
        if let Some((stem, _)) = parse_claim(claimed) {
            let _ = fs::rename(claimed, claimed.with_file_name(format!("{stem}.{EXT}")));
        } else if let Some(name) = claimed.file_stem().and_then(|s| s.to_str()) {
            // A claim in the old format (`<name>.sending`).
            let _ = fs::rename(claimed, claimed.with_file_name(format!("{name}.{EXT}")));
        }
    }

    pub(crate) fn finish(&self, claimed: &Path) {
        let _ = fs::remove_file(claimed);
    }

    /// Puts back reports whose claim is over an hour old, judged by the claim
    /// time in the name, never by the age of the report. Restoring is a
    /// rename too, so it cannot overlap a fresh claim of the same file.
    pub(crate) fn recover_stale(&self) {
        self.recover_stale_at(now());
    }

    pub(crate) fn recover_stale_at(&self, at: i64) {
        for p in self.files(SENDING) {
            let claimed_at = match parse_claim(&p) {
                Some((_, claimed_at)) => claimed_at,
                // Left by a version that did not record the claim: the best
                // evidence is when the file was last renamed or written.
                None => fs::metadata(&p)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs() as i64),
            };
            if at - claimed_at > CLAIM_TTL_SECS {
                self.release(&p);
            }
        }
    }

    pub fn load_state(&self) -> State {
        fs::read(self.dir.join(STATE))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save_state(&self, state: &State) {
        if fs::create_dir_all(&self.dir).is_ok() {
            if let Ok(json) = serde_json::to_vec(state) {
                let _ = fs::write(self.dir.join(STATE), json);
            }
        }
    }
}

/// `(report name, claim time)` of `<name>.<claim time>.<owner>.sending`.
fn parse_claim(path: &Path) -> Option<(String, i64)> {
    let name = path.file_name()?.to_str()?;
    let rest = name.strip_suffix(&format!(".{SENDING}"))?;
    let (rest, owner) = rest.rsplit_once('.')?;
    let (stem, at) = rest.rsplit_once('.')?;
    if owner.is_empty() || stem.is_empty() {
        return None;
    }
    Some((stem.to_owned(), at.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(msg: &str, ts: i64) -> Report {
        let mut r = Report::new("app", "panic", "1.0.2", msg, []);
        r.timestamp = ts;
        r
    }

    #[test]
    fn write_list_clear() {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::new(dir.path().join("sub"));
        assert!(spool.list().is_empty());
        spool.write(&report("one", now())).unwrap();
        spool.write(&report("two", now() + 1)).unwrap();
        let list = spool.list();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].report.message, "one");
        assert_eq!(spool.clear(), 2);
        assert!(spool.list().is_empty());
    }

    #[test]
    fn count_cap_drops_the_oldest() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits {
            max_reports: 3,
            ..Limits::default()
        };
        let spool = Spool::with_limits(dir.path(), limits);
        for i in 0..6 {
            spool.write(&report(&format!("r{i}"), now() + i)).unwrap();
        }
        let msgs: Vec<_> = spool.list().into_iter().map(|e| e.report.message).collect();
        assert_eq!(msgs, ["r3", "r4", "r5"]);
    }

    #[test]
    fn total_size_cap() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits {
            max_total_bytes: 2_000,
            ..Limits::default()
        };
        let spool = Spool::with_limits(dir.path(), limits);
        for i in 0..20 {
            spool
                .write(&report(&"word ".repeat(80), now() + i))
                .unwrap();
        }
        let total: u64 = spool.list().iter().map(|e| e.bytes).sum();
        assert!(total <= 2_000, "{total}");
        assert!(!spool.list().is_empty());
    }

    #[test]
    fn one_report_is_bounded_by_dropping_frames() {
        let dir = tempfile::tempdir().unwrap();
        let limits = Limits {
            max_report_bytes: 700,
            ..Limits::default()
        };
        let spool = Spool::with_limits(dir.path(), limits);
        let frames: Vec<String> = (0..40)
            .map(|i| format!("module::function_number_{i}"))
            .collect();
        let r = Report::new("app", "panic", "1", "m", frames);
        let path = spool.write(&r).unwrap();
        assert!(fs::metadata(path).unwrap().len() <= 700);
        // A report whose fixed part alone is too big is refused, not truncated mid-JSON.
        let tiny = Spool::with_limits(
            dir.path(),
            Limits {
                max_report_bytes: 50,
                ..Limits::default()
            },
        );
        assert!(tiny.write(&r).is_err());
    }

    #[test]
    fn old_reports_expire() {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::new(dir.path());
        spool.write(&report("old", now() - 15 * 24 * 3600)).unwrap();
        spool.write(&report("new", now())).unwrap();
        let msgs: Vec<_> = spool.list().into_iter().map(|e| e.report.message).collect();
        assert_eq!(msgs, ["new"]);
    }

    #[test]
    fn garbage_files_are_removed_and_state_survives_clear() {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::new(dir.path());
        fs::write(dir.path().join("000-x.crash"), b"not json").unwrap();
        assert!(spool.list().is_empty());
        assert!(!dir.path().join("000-x.crash").exists());
        let state = State {
            retry_after: 5,
            ..State::default()
        };
        spool.save_state(&state);
        spool.clear();
        assert_eq!(spool.load_state(), state);
    }

    #[test]
    fn stale_content_is_scrubbed_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::new(dir.path());
        let mut r = report("fine", now());
        r.message = "to db.example.com".into();
        fs::create_dir_all(dir.path()).unwrap();
        fs::write(
            dir.path()
                .join(format!("{:012}-{}.crash", r.timestamp, r.id)),
            serde_json::to_vec(&r).unwrap(),
        )
        .unwrap();
        assert_eq!(spool.list()[0].report.message, "to <host>");
    }

    fn old_report(spool: &Spool) -> PathBuf {
        // A report that has waited for hours in the spool.
        spool.write(&report("old", now() - 5 * 3600)).unwrap()
    }

    #[test]
    fn a_fresh_claim_on_an_old_report_is_not_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let a = Spool::new(dir.path());
        let b = Spool::new(dir.path());
        let path = old_report(&a);
        // The file itself is old on disk too.
        let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(5 * 3600);
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        let claimed = a.claim(&path).unwrap();
        b.recover_stale();
        assert!(claimed.exists(), "sender B took A's live claim");
        assert!(b.list().is_empty());
        assert!(b.claim(&path).is_none());
        a.finish(&claimed);
        assert!(a.list().is_empty());
    }

    #[test]
    fn a_stale_claim_is_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let a = Spool::new(dir.path());
        let b = Spool::new(dir.path());
        let path = old_report(&a);
        let claimed = a.claim_at(&path, now() - 2 * 3600).unwrap();
        b.recover_stale();
        assert!(!claimed.exists());
        let list = b.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, path);
        assert!(b.claim(&list[0].path).is_some());
    }

    #[test]
    fn only_one_sender_wins_a_claim() {
        let dir = tempfile::tempdir().unwrap();
        let a = Spool::new(dir.path());
        let b = Spool::new(dir.path());
        let path = a.write(&report("x", now())).unwrap();
        let first = a.claim(&path);
        let second = b.claim(&path);
        assert!(first.is_some() && second.is_none());
        a.release(&first.unwrap());
        assert_eq!(a.list().len(), 1);
    }

    #[test]
    fn a_claim_in_the_old_format_is_judged_by_its_file_time() {
        let dir = tempfile::tempdir().unwrap();
        let spool = Spool::new(dir.path());
        let path = spool.write(&report("x", now())).unwrap();
        let legacy = path.with_extension(SENDING);
        fs::rename(&path, &legacy).unwrap();
        spool.recover_stale();
        assert!(legacy.exists(), "just renamed, so not stale");
        spool.recover_stale_at(now() + 2 * 3600);
        assert!(!legacy.exists());
        assert_eq!(spool.list().len(), 1);
    }
}
