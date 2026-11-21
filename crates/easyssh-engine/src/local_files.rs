//! The phone's side of a transfer: where an upload waits and where a
//! download lands.
//!
//! The web view cannot hand the engine a path: a file picker gives it the
//! bytes (on Android a `content://` document that is not a path at all). So an
//! upload is staged first, the web view sends the file in chunks and the
//! engine uploads the staged copy, which is removed when the transfer ends.
//! A download lands in the app's own cache folder; handing it to the user
//! (share sheet, Downloads) is the platform's business and happens after the
//! transfer, on a path the app produced.

use std::collections::HashMap;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Remote names come from the server, so they must not point outside the
/// folder they are saved in, and must be valid on every phone file system.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            ch if ch.is_control() => '_',
            ch => ch,
        })
        .collect();
    let cleaned = cleaned.trim().to_owned();
    if matches!(cleaned.as_str(), "" | "." | "..") {
        "download".to_owned()
    } else {
        // Long names are cut at a character boundary (file systems allow 255
        // bytes).
        let mut end = cleaned.len().min(200);
        while !cleaned.is_char_boundary(end) {
            end -= 1;
        }
        cleaned[..end].to_owned()
    }
}

/// `name`, `name (1)`, `name (2)`, ... (the number goes before the extension).
fn numbered_names(name: &str) -> impl Iterator<Item = String> + '_ {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let extension = path.extension().and_then(|extension| extension.to_str());
    std::iter::once(name.to_owned()).chain((1..1000).map(move |number| match extension {
        Some(extension) => format!("{stem} ({number}).{extension}"),
        None => format!("{stem} ({number})"),
    }))
}

/// `name`, or `name (1)`, `name (2)`, ... when it exists in `dir`. Only a
/// suggestion: someone else may take the name before it is used. To take it
/// for certain use [`reserve_unique_path`].
pub fn unique_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = safe_file_name(name);
    let found = numbered_names(&name)
        .map(|file| dir.join(file))
        .find(|candidate| !candidate.exists());
    found.ok_or_else(|| "too many files with the same name".to_owned())
}

/// Like [`unique_path`], but the name is taken for good: an empty file is
/// created under it with `create_new` (exclusive, so two callers never get
/// the same name). The caller replaces that file with the real one, or
/// removes it when it gives up.
pub fn reserve_unique_path(dir: &Path, name: &str) -> Result<PathBuf, String> {
    let name = safe_file_name(name);
    for file in numbered_names(&name) {
        let candidate = dir.join(file);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("too many files with the same name".to_owned())
}

/// Removes the folders in `root` that were last changed more than `age` ago
/// (leftovers of a transfer that never finished). Files directly in `root`
/// are judged the same way.
pub fn sweep(root: &Path, age: Duration) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|elapsed| elapsed > age);
        if !old {
            continue;
        }
        let path = entry.path();
        let _ = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
    }
}

/// Files the web view sends to the engine before uploading them.
#[derive(Debug, Clone)]
pub struct Staging {
    root: PathBuf,
    /// One lock per upload id: the length check and the write of a chunk
    /// happen under it, so overlapping calls for one upload cannot both pass
    /// the check. Uploads with different ids never wait for each other.
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl Staging {
    pub fn new(root: PathBuf) -> Staging {
        Staging {
            root,
            locks: Arc::default(),
        }
    }

    fn lock_for(&self, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(|e| e.into_inner());
        locks.entry(id.to_owned()).or_default().clone()
    }

    fn dir(&self, id: &str) -> Result<PathBuf, String> {
        // Ids are made here; anything else is an attempt to leave the folder.
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("unknown upload".to_owned());
        }
        Ok(self.root.join(id))
    }

    /// Starts an upload of a file called `name` and returns its id.
    pub fn begin(&self, name: &str) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = self.dir(&id)?;
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        let file = dir.join(safe_file_name(name));
        std::fs::File::create(file).map_err(|error| error.to_string())?;
        Ok(id)
    }

    fn file(&self, id: &str) -> Result<PathBuf, String> {
        let dir = self.dir(id)?;
        let mut files = std::fs::read_dir(&dir)
            .map_err(|_| "unknown upload".to_owned())?
            .flatten();
        files
            .next()
            .map(|entry| entry.path())
            .ok_or_else(|| "unknown upload".to_owned())
    }

    /// Appends the next chunk. `offset` must be where the file ends, so a
    /// repeated or reordered chunk is an error rather than a corrupt file. The
    /// check and the write are one step per upload id: of several overlapping
    /// calls with the same offset at most one is accepted. Returns the new
    /// length.
    pub fn append(&self, id: &str, offset: u64, bytes: &[u8]) -> Result<u64, String> {
        self.dir(id)?;
        let lock = self.lock_for(id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.file(id)?;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        let length = file.metadata().map_err(|error| error.to_string())?.len();
        if length != offset {
            return Err(format!(
                "the upload is {length} bytes long, a chunk for {offset} arrived"
            ));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|error| error.to_string())?;
        file.write_all(bytes).map_err(|error| error.to_string())?;
        Ok(length + bytes.len() as u64)
    }

    /// The staged file, once it is complete. `total` is the size the sender
    /// promised; a file of any other length is refused.
    pub fn finish(&self, id: &str, total: u64) -> Result<PathBuf, String> {
        self.dir(id)?;
        let lock = self.lock_for(id);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.file(id)?;
        let length = std::fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .len();
        if length != total {
            return Err(format!(
                "the upload is {length} bytes long, {total} were expected"
            ));
        }
        Ok(path)
    }

    /// The staged file's path.
    pub fn path(&self, id: &str) -> Result<PathBuf, String> {
        self.file(id)
    }

    /// Removes a staged file (the transfer ended, or the person gave up).
    pub fn discard(&self, id: &str) {
        if let Ok(dir) = self.dir(id) {
            let lock = self.lock_for(id);
            {
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                let _ = std::fs::remove_dir_all(dir);
            }
            self.locks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(id);
        }
    }

    /// Removes the uploads that were abandoned more than `age` ago.
    pub fn sweep(&self, age: Duration) {
        sweep(&self.root, age);
    }

    /// The id of a staged file's path (its parent folder's name), when the
    /// path is inside this staging area.
    pub fn id_of(&self, path: &Path) -> Option<String> {
        let id = path.parent()?.file_name()?.to_str()?;
        (path.parent()?.parent()? == self.root && self.dir(id).is_ok()).then(|| id.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_names_cannot_escape_the_downloads_folder() {
        assert_eq!(safe_file_name("..\\..\\evil.exe"), ".._.._evil.exe");
        assert_eq!(safe_file_name(".."), "download");
        assert_eq!(safe_file_name(""), "download");
        assert_eq!(safe_file_name("C:evil"), "C_evil");
        assert_eq!(safe_file_name("a/b"), "a_b");
        assert_eq!(safe_file_name("report 2026.pdf"), "report 2026.pdf");
        assert_eq!(safe_file_name("tab\tname"), "tab_name");
        let long = safe_file_name(&"é".repeat(300));
        assert!(long.len() <= 200 && long.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_name_in_use_gets_a_number_before_the_extension() {
        let dir = tempfile::tempdir().unwrap();
        let first = unique_path(dir.path(), "log.txt").unwrap();
        assert_eq!(first, dir.path().join("log.txt"));
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(dir.path(), "log.txt").unwrap();
        assert_eq!(second, dir.path().join("log (1).txt"));
        std::fs::write(&second, b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "log.txt").unwrap(),
            dir.path().join("log (2).txt")
        );
        std::fs::write(dir.path().join("Makefile"), b"x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "Makefile").unwrap(),
            dir.path().join("Makefile (1)")
        );
    }

    #[test]
    fn a_reserved_name_is_never_handed_out_twice() {
        let dir = tempfile::tempdir().unwrap();
        let first = reserve_unique_path(dir.path(), "log.txt").unwrap();
        let second = reserve_unique_path(dir.path(), "log.txt").unwrap();
        assert_eq!(first, dir.path().join("log.txt"));
        assert_eq!(second, dir.path().join("log (1).txt"));
        assert!(first.exists() && second.exists());
        // Threads racing for one name each get their own.
        let paths: Vec<_> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| reserve_unique_path(dir.path(), "r.bin").unwrap()))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        let distinct: std::collections::HashSet<_> = paths.iter().collect();
        assert_eq!(distinct.len(), 8);
    }

    #[test]
    fn an_upload_is_staged_in_order_and_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let id = staging.begin("../photo.jpg").unwrap();
        assert_eq!(staging.append(&id, 0, b"abc").unwrap(), 3);
        assert_eq!(staging.append(&id, 3, b"def").unwrap(), 6);
        // A repeated or skipped chunk is refused instead of corrupting the file.
        assert!(staging.append(&id, 3, b"def").is_err());
        assert!(staging.append(&id, 9, b"x").is_err());
        let path = staging.path(&id).unwrap();
        assert_eq!(path.file_name().unwrap(), ".._photo.jpg");
        assert_eq!(std::fs::read(&path).unwrap(), b"abcdef");
        assert_eq!(staging.id_of(&path).as_deref(), Some(id.as_str()));
        assert_eq!(staging.id_of(Path::new("/etc/passwd")), None);
        staging.discard(&id);
        assert!(!path.exists());
        assert!(staging.path(&id).is_err());
        staging.discard(&id); // twice is fine
    }

    #[test]
    fn overlapping_chunks_for_one_offset_are_accepted_once() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        for round in 0..20 {
            let id = staging.begin("big.bin").unwrap();
            let gate = Arc::new(std::sync::Barrier::new(8));
            let payload = vec![round as u8; 256 * 1024];
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let (staging, id, gate, payload) =
                        (staging.clone(), id.clone(), gate.clone(), payload.clone());
                    std::thread::spawn(move || {
                        gate.wait();
                        staging.append(&id, 0, &payload).is_ok()
                    })
                })
                .collect();
            let accepted = handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|ok| *ok)
                .count();
            assert_eq!(accepted, 1, "round {round}");
            let path = staging.path(&id).unwrap();
            assert_eq!(std::fs::metadata(path).unwrap().len(), payload.len() as u64);
            assert!(staging.finish(&id, payload.len() as u64).is_ok());
        }
    }

    #[test]
    fn finish_checks_the_promised_size() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let id = staging.begin("a.txt").unwrap();
        staging.append(&id, 0, b"abc").unwrap();
        assert!(staging.finish(&id, 4).is_err());
        assert!(staging.finish(&id, 3).is_ok());
    }

    #[test]
    fn different_uploads_do_not_wait_for_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let a = staging.begin("a").unwrap();
        let b = staging.begin("b").unwrap();
        assert!(!Arc::ptr_eq(&staging.lock_for(&a), &staging.lock_for(&b)));
        assert!(Arc::ptr_eq(&staging.lock_for(&a), &staging.lock_for(&a)));
        // With A's lock held, B still makes progress; A itself waits.
        let held = staging.lock_for(&a);
        let guard = held.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        for (id, name) in [(b.clone(), "b"), (a.clone(), "a")] {
            let (staging, tx) = (staging.clone(), tx.clone());
            std::thread::spawn(move || {
                staging.append(&id, 0, b"x").unwrap();
                tx.send(name).unwrap();
            });
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "b");
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        drop(guard);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "a");
    }

    #[test]
    fn upload_ids_cannot_point_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        for bad in ["..", "../x", "", "zzzz", &"a".repeat(31), &"g".repeat(32)] {
            assert!(staging.path(bad).is_err(), "{bad:?}");
            assert!(staging.append(bad, 0, b"x").is_err(), "{bad:?}");
            staging.discard(bad);
        }
    }

    #[test]
    fn sweeping_removes_only_old_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging::new(dir.path().to_path_buf());
        let fresh = staging.begin("fresh.txt").unwrap();
        staging.sweep(Duration::from_secs(3600));
        assert!(staging.path(&fresh).is_ok(), "a recent upload stays");
        std::thread::sleep(Duration::from_millis(30));
        staging.sweep(Duration::from_millis(10));
        assert!(staging.path(&fresh).is_err(), "an old one goes");
    }
}
