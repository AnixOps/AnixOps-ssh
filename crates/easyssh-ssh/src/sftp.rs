//! SFTP on a channel of the same connection.

use crate::error::{Error, Result};
use crate::session::Session;
use russh_sftp::client::{Config as SftpConfig, SftpSession};
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// A remote file opened for reading or writing (`AsyncRead`, `AsyncWrite`,
/// `AsyncSeek`). Call `shutdown()` (or `close()`) after writing.
pub use russh_sftp::client::fs::File as SftpFile;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStat {
    pub kind: EntryKind,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Permission bits (`0o755` style), if the server sent them.
    pub permissions: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub stat: FileStat,
}

fn convert(metadata: &russh_sftp::client::fs::Metadata) -> FileStat {
    use russh_sftp::protocol::FileType;
    FileStat {
        kind: match metadata.file_type() {
            FileType::File => EntryKind::File,
            FileType::Dir => EntryKind::Dir,
            FileType::Symlink => EntryKind::Symlink,
            FileType::Other => EntryKind::Other,
        },
        size: metadata.len(),
        modified: metadata.modified().ok(),
        permissions: metadata.permissions.map(|bits| bits & 0o7777),
        uid: metadata.uid,
        gid: metadata.gid,
    }
}

/// Requests kept in flight per file, as OpenSSH's `sftp` does (`-R 64`).
/// Reads and writes are pipelined by `SftpFile`: it sends this many
/// positional requests before it waits for the oldest answer, hands the data
/// back in file order, and re-requests the gap after a short read.
pub const MAX_IN_FLIGHT: usize = 64;
/// Size of one write request, OpenSSH's default (`-B 32768`).
pub const WRITE_PACKET_LEN: u32 = 32 * 1024;
/// Largest packet accepted; also caps the size of one read request.
pub const MAX_PACKET_LEN: u32 = 256 * 1024;
/// Seconds to wait for an answer. A full window of requests queues behind
/// each other on a slow link, so the library's 10 s is too short.
pub const REQUEST_TIMEOUT_SECS: u64 = 60;

fn pipeline_config() -> SftpConfig {
    SftpConfig {
        max_packet_len: MAX_PACKET_LEN,
        max_concurrent_reads: MAX_IN_FLIGHT,
        max_concurrent_writes: MAX_IN_FLIGHT,
        max_write_packet_len: WRITE_PACKET_LEN,
        request_timeout_secs: REQUEST_TIMEOUT_SECS,
    }
}

pub struct Sftp {
    session: SftpSession,
}

impl Session {
    /// Opens an SFTP channel on this connection.
    pub async fn sftp(&self) -> Result<Sftp> {
        let channel = self.open_session_channel().await?;
        channel.request_subsystem(true, "sftp").await?;
        Sftp::over(channel.into_stream()).await
    }
}

impl Sftp {
    /// Starts SFTP on any byte stream, with the pipelined transfer settings.
    pub async fn over<S>(stream: S) -> Result<Sftp>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let session = SftpSession::new_with_config(stream, pipeline_config()).await?;
        Ok(Sftp { session })
    }

    /// The user's home directory.
    pub async fn home(&self) -> Result<String> {
        self.realpath(".").await
    }

    pub async fn realpath(&self, path: &str) -> Result<String> {
        Ok(self.session.canonicalize(path).await?)
    }

    /// Entries of a directory, sorted by name, without `.` and `..`.
    pub async fn list(&self, path: &str) -> Result<Vec<DirEntry>> {
        let mut entries: Vec<DirEntry> = self
            .session
            .read_dir(path)
            .await?
            .filter(|entry| {
                let name = entry.file_name();
                name != "." && name != ".."
            })
            .map(|entry| DirEntry {
                name: entry.file_name(),
                stat: convert(&entry.metadata()),
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Follows symbolic links.
    pub async fn stat(&self, path: &str) -> Result<FileStat> {
        Ok(convert(&self.session.metadata(path).await?))
    }

    /// Does not follow symbolic links.
    pub async fn lstat(&self, path: &str) -> Result<FileStat> {
        Ok(convert(&self.session.symlink_metadata(path).await?))
    }

    pub async fn exists(&self, path: &str) -> Result<bool> {
        Ok(self.session.try_exists(path).await?)
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        Ok(self.session.create_dir(path).await?)
    }

    pub async fn rename(&self, from: &str, to: &str) -> Result<()> {
        Ok(self.session.rename(from, to).await?)
    }

    /// Sets the permission bits (`0o644` style) and nothing else.
    pub async fn set_permissions(&self, path: &str, mode: u32) -> Result<()> {
        let metadata = russh_sftp::client::fs::Metadata {
            permissions: Some(mode & 0o7777),
            ..Default::default()
        };
        Ok(self.session.set_metadata(path, metadata).await?)
    }

    pub async fn remove_file(&self, path: &str) -> Result<()> {
        Ok(self.session.remove_file(path).await?)
    }

    pub async fn remove_dir(&self, path: &str) -> Result<()> {
        Ok(self.session.remove_dir(path).await?)
    }

    /// Removes `path` and, if it is a folder, everything in it, deepest
    /// first. Links are removed themselves and never followed, so what a
    /// link inside points to is left alone.
    pub async fn remove_dir_all(&self, path: &str) -> Result<()> {
        if self.lstat(path).await?.kind != EntryKind::Dir {
            return self.remove_file(path).await;
        }
        // (folder, emptied): a folder is removed when it comes up again
        // after everything in it.
        let mut pending = vec![(path.to_owned(), false)];
        while let Some((dir, emptied)) = pending.pop() {
            if emptied {
                self.remove_dir(&dir).await?;
                continue;
            }
            pending.push((dir.clone(), true));
            for entry in self.list(&dir).await? {
                if entry.name.is_empty() || entry.name.contains('/') {
                    return Err(Error::Sftp(format!(
                        "{dir} lists an entry that cannot be removed: {:?}",
                        entry.name
                    )));
                }
                let child = if dir.ends_with('/') {
                    format!("{dir}{}", entry.name)
                } else {
                    format!("{dir}/{}", entry.name)
                };
                // Listings report links as links (lstat), not what they
                // point to.
                if entry.stat.kind == EntryKind::Dir {
                    pending.push((child, false));
                } else {
                    self.remove_file(&child).await?;
                }
            }
        }
        Ok(())
    }

    /// Opens an existing file for reading.
    pub async fn open_read(&self, path: &str) -> Result<SftpFile> {
        Ok(self.session.open(path).await?)
    }

    /// Creates the file (truncating an existing one) for writing.
    pub async fn create(&self, path: &str) -> Result<SftpFile> {
        Ok(self.session.create(path).await?)
    }

    /// Streams a remote file into `sink`; returns the byte count.
    pub async fn download<W: AsyncWrite + Unpin>(&self, path: &str, sink: &mut W) -> Result<u64> {
        let mut file = self.open_read(path).await?;
        let copied = tokio::io::copy(&mut file, sink)
            .await
            .map_err(|e| Error::Sftp(format!("downloading {path}: {e}")))?;
        sink.flush().await?;
        Ok(copied)
    }

    /// Streams `source` into a remote file; returns the byte count.
    pub async fn upload<R: AsyncRead + Unpin>(&self, path: &str, source: &mut R) -> Result<u64> {
        let mut file = self.create(path).await?;
        let copied = tokio::io::copy(source, &mut file)
            .await
            .map_err(|e| Error::Sftp(format!("uploading {path}: {e}")))?;
        file.shutdown()
            .await
            .map_err(|e| Error::Sftp(format!("closing {path}: {e}")))?;
        Ok(copied)
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let mut file = self.open_read(path).await?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .await
            .map_err(|e| Error::Sftp(format!("reading {path}: {e}")))?;
        Ok(bytes)
    }

    pub async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.upload(path, &mut &data[..]).await.map(drop)
    }

    pub async fn close(&self) -> Result<()> {
        Ok(self.session.close().await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh_sftp::protocol::{Data, FileAttributes, Handle, OpenFlags, Status, StatusCode};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// An in-memory server that answers reads in short pieces and records
    /// the offsets it was asked for.
    struct Memory {
        files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        reads: Arc<Mutex<Vec<u64>>>,
        piece: usize,
    }

    impl russh_sftp::server::Handler for Memory {
        type Error = StatusCode;

        fn unimplemented(&self) -> StatusCode {
            StatusCode::OpUnsupported
        }

        async fn open(
            &mut self,
            id: u32,
            filename: String,
            pflags: OpenFlags,
            _attrs: FileAttributes,
        ) -> std::result::Result<Handle, StatusCode> {
            let mut files = self.files.lock().unwrap();
            if pflags.contains(OpenFlags::TRUNCATE) || pflags.contains(OpenFlags::CREATE) {
                files.insert(filename.clone(), Vec::new());
            } else if !files.contains_key(&filename) {
                return Err(StatusCode::NoSuchFile);
            }
            Ok(Handle {
                id,
                handle: filename,
            })
        }

        async fn close(
            &mut self,
            id: u32,
            _handle: String,
        ) -> std::result::Result<Status, StatusCode> {
            Ok(ok(id))
        }

        async fn read(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            len: u32,
        ) -> std::result::Result<Data, StatusCode> {
            self.reads.lock().unwrap().push(offset);
            let files = self.files.lock().unwrap();
            let file = files.get(&handle).ok_or(StatusCode::NoSuchFile)?;
            let start = offset as usize;
            if start >= file.len() {
                return Err(StatusCode::Eof);
            }
            let end = (start + (len as usize).min(self.piece)).min(file.len());
            Ok(Data {
                id,
                data: file[start..end].to_vec(),
            })
        }

        async fn write(
            &mut self,
            id: u32,
            handle: String,
            offset: u64,
            data: Vec<u8>,
        ) -> std::result::Result<Status, StatusCode> {
            let mut files = self.files.lock().unwrap();
            let file = files.get_mut(&handle).ok_or(StatusCode::NoSuchFile)?;
            let end = offset as usize + data.len();
            if file.len() < end {
                file.resize(end, 0);
            }
            file[offset as usize..end].copy_from_slice(&data);
            Ok(ok(id))
        }
    }

    fn ok(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en".into(),
        }
    }

    async fn memory_sftp(
        piece: usize,
    ) -> (
        Sftp,
        Arc<Mutex<HashMap<String, Vec<u8>>>>,
        Arc<Mutex<Vec<u64>>>,
    ) {
        let files = Arc::new(Mutex::new(HashMap::new()));
        let reads = Arc::new(Mutex::new(Vec::new()));
        let (client, server) = tokio::io::duplex(1 << 20);
        russh_sftp::server::run(
            server,
            Memory {
                files: files.clone(),
                reads: reads.clone(),
                piece,
            },
        )
        .await;
        (Sftp::over(client).await.expect("sftp"), files, reads)
    }

    fn sample(len: usize) -> Vec<u8> {
        // Not periodic at the chunk size, so a reordered chunk changes the hash.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    #[test]
    fn window_memory_is_bounded() {
        assert_eq!(MAX_IN_FLIGHT, 64);
        // A full window of the largest requests: servers usually answer
        // reads with 32 to 64 KiB, far below this.
        assert!(MAX_IN_FLIGHT as u64 * MAX_PACKET_LEN as u64 <= 16 * 1024 * 1024);
        let config = pipeline_config();
        assert_eq!(config.max_concurrent_reads, MAX_IN_FLIGHT);
        assert_eq!(config.max_concurrent_writes, MAX_IN_FLIGHT);
        assert_eq!(config.max_write_packet_len, WRITE_PACKET_LEN);
    }

    #[tokio::test]
    async fn upload_of_many_chunks_arrives_in_order() {
        let (sftp, files, _) = memory_sftp(32 * 1024).await;
        // 5 MiB plus a ragged tail: far more requests than the window.
        let data = sample(5 * 1024 * 1024 + 4321);
        let sent = sftp.upload("/up", &mut &data[..]).await.unwrap();
        assert_eq!(sent, data.len() as u64);
        let stored = files.lock().unwrap().get("/up").cloned().unwrap();
        assert_eq!(Sha256::digest(&stored), Sha256::digest(&data));
    }

    #[tokio::test]
    async fn download_handles_short_reads_and_eof() {
        // The server answers with 10000 bytes however much is asked.
        let (sftp, files, reads) = memory_sftp(10_000).await;
        let data = sample(3 * 1024 * 1024 + 77);
        files.lock().unwrap().insert("/down".into(), data.clone());
        let mut sink = Vec::new();
        let got = sftp.download("/down", &mut sink).await.unwrap();
        assert_eq!(got, data.len() as u64);
        assert_eq!(Sha256::digest(&sink), Sha256::digest(&data));
        // Reads were asked for ahead of the data in hand (more than one per
        // answer), yet every byte was requested at least once.
        assert!(reads.lock().unwrap().len() > data.len() / 10_000);
    }

    #[tokio::test]
    async fn empty_file_round_trips() {
        let (sftp, _, _) = memory_sftp(32 * 1024).await;
        sftp.write_file("/empty", b"").await.unwrap();
        assert!(sftp.read_file("/empty").await.unwrap().is_empty());
    }
}
