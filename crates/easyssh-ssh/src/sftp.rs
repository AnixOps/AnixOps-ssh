//! SFTP on a channel of the same connection.

use crate::error::{Error, Result};
use crate::session::Session;
use russh_sftp::client::{Config as SftpConfig, SftpSession};
use std::time::SystemTime;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

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

/// Requests kept in flight per open file, as OpenSSH's `sftp` does (its
/// default is 64 requests of 32 KiB, which fills the 2 MiB channel window).
pub const REQUESTS_IN_FLIGHT: usize = 64;

/// How long one request may wait for its answer. With 64 requests queued
/// behind each other on a slow link the last one waits for all the others.
const REQUEST_TIMEOUT_SECS: u64 = 120;

fn pipelined_config() -> SftpConfig {
    SftpConfig {
        max_concurrent_reads: REQUESTS_IN_FLIGHT,
        max_concurrent_writes: REQUESTS_IN_FLIGHT,
        request_timeout_secs: REQUEST_TIMEOUT_SECS,
        ..SftpConfig::default()
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
    /// Speaks SFTP over any byte stream (a subsystem channel, or a pipe in
    /// tests). Reads and writes are pipelined: many requests are sent
    /// before the first answer is needed, answers may arrive in any order,
    /// and the data is reassembled in file order.
    pub async fn over<S>(stream: S) -> Result<Sftp>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let session = SftpSession::new_with_config(stream, pipelined_config()).await?;
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

    /// Sets the permission bits (`0o644` style) and the modification time
    /// that are given, and nothing else. The access time is set to the
    /// modification time.
    pub async fn set_stat(
        &self,
        path: &str,
        permissions: Option<u32>,
        modified: Option<SystemTime>,
    ) -> Result<()> {
        let seconds = modified
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|age| u32::try_from(age.as_secs()).unwrap_or(u32::MAX));
        let metadata = russh_sftp::client::fs::Metadata {
            permissions: permissions.map(|bits| bits & 0o7777),
            atime: seconds,
            mtime: seconds,
            ..Default::default()
        };
        Ok(self.session.set_metadata(path, metadata).await?)
    }

    /// Sets the permission bits of `path` and, if it is a folder, of
    /// everything in it. Links are skipped: changing one would change what it
    /// points to, which may be outside the folder.
    pub async fn set_permissions_all(&self, path: &str, mode: u32) -> Result<()> {
        if self.lstat(path).await?.kind == EntryKind::Symlink {
            return Ok(());
        }
        self.set_stat(path, Some(mode), None).await?;
        let mut pending = vec![path.to_owned()];
        while let Some(dir) = pending.pop() {
            for entry in self.list(&dir).await? {
                if entry.name.is_empty() || entry.name.contains('/') {
                    return Err(Error::Sftp(format!(
                        "{dir} lists an entry that cannot be changed: {:?}",
                        entry.name
                    )));
                }
                let child = if dir.ends_with('/') {
                    format!("{dir}{}", entry.name)
                } else {
                    format!("{dir}/{}", entry.name)
                };
                match entry.stat.kind {
                    EntryKind::Symlink => {}
                    EntryKind::Dir => {
                        self.set_stat(&child, Some(mode), None).await?;
                        pending.push(child);
                    }
                    _ => self.set_stat(&child, Some(mode), None).await?,
                }
            }
        }
        Ok(())
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

    /// Opens an existing file for reading, starting at `offset` (resuming a
    /// download).
    pub async fn open_read_at(&self, path: &str, offset: u64) -> Result<SftpFile> {
        let mut file = self.open_read(path).await?;
        if offset > 0 {
            file.seek(std::io::SeekFrom::Start(offset))
                .await
                .map_err(|e| Error::Sftp(format!("seeking in {path}: {e}")))?;
        }
        Ok(file)
    }

    /// Opens an existing file for writing at `offset` without truncating it
    /// (resuming an upload). Fails when the file does not exist.
    pub async fn open_write_at(&self, path: &str, offset: u64) -> Result<SftpFile> {
        let mut file = self
            .session
            .open_with_flags(path, russh_sftp::protocol::OpenFlags::WRITE)
            .await?;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|e| Error::Sftp(format!("seeking in {path}: {e}")))?;
        Ok(file)
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
    use bytes::Bytes;
    use russh_sftp::protocol::{Data, Handle, Packet, Status, StatusCode, Version};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

    const FILE_LEN: usize = 1_000_003;

    fn content() -> Vec<u8> {
        (0..FILE_LEN).map(|i| (i * 31 + i / 7) as u8).collect()
    }

    async fn read_request(stream: &mut DuplexStream) -> Option<Packet> {
        let len = stream.read_u32().await.ok()?;
        let mut buf = vec![0; len as usize];
        stream.read_exact(&mut buf).await.ok()?;
        Packet::try_from(&mut Bytes::from(buf)).ok()
    }

    async fn send(stream: &mut DuplexStream, packet: Packet) {
        let bytes = Bytes::try_from(packet).unwrap();
        stream.write_all(&bytes).await.unwrap();
    }

    /// A server that holds on to read requests until `batch` of them are
    /// waiting (or nothing more arrives) and then answers them newest first,
    /// with at most `short` bytes each. It records how many were in flight.
    async fn serve(mut stream: DuplexStream, batch: usize, short: usize, peak: Arc<AtomicUsize>) {
        let data = content();
        let mut waiting: Vec<(u32, u64, u32)> = Vec::new();
        loop {
            let request = if waiting.is_empty() {
                read_request(&mut stream).await
            } else {
                match tokio::time::timeout(Duration::from_millis(30), read_request(&mut stream))
                    .await
                {
                    Ok(request) => request,
                    Err(_) => {
                        flush(&mut stream, &mut waiting, &data, short).await;
                        continue;
                    }
                }
            };
            let Some(request) = request else { return };
            match request {
                Packet::Init(_) => send(&mut stream, Packet::Version(Version::new())).await,
                Packet::Open(open) => {
                    send(
                        &mut stream,
                        Packet::Handle(Handle {
                            id: open.id,
                            handle: "h".into(),
                        }),
                    )
                    .await
                }
                Packet::Read(read) => {
                    waiting.push((read.id, read.offset, read.len));
                    peak.fetch_max(waiting.len(), Ordering::Relaxed);
                    if waiting.len() >= batch {
                        flush(&mut stream, &mut waiting, &data, short).await;
                    }
                }
                Packet::Close(close) => {
                    send(&mut stream, Packet::error(close.id, StatusCode::Ok)).await
                }
                other => {
                    send(
                        &mut stream,
                        Packet::error(other.get_request_id(), StatusCode::OpUnsupported),
                    )
                    .await
                }
            }
        }
    }

    async fn flush(
        stream: &mut DuplexStream,
        waiting: &mut Vec<(u32, u64, u32)>,
        data: &[u8],
        short: usize,
    ) {
        while let Some((id, offset, len)) = waiting.pop() {
            let offset = offset as usize;
            let packet = if offset >= data.len() {
                Packet::Status(Status {
                    id,
                    status_code: StatusCode::Eof,
                    error_message: "eof".into(),
                    language_tag: "en".into(),
                })
            } else {
                let end = (offset + (len as usize).min(short)).min(data.len());
                Packet::Data(Data {
                    id,
                    data: data[offset..end].to_vec(),
                })
            };
            send(stream, packet).await;
        }
    }

    #[tokio::test]
    async fn downloads_reassemble_answers_that_arrive_out_of_order_and_stop_at_eof() {
        for (batch, short) in [(8, 32 * 1024), (16, 32 * 1024), (8, 10_000)] {
            let (client, server) = duplex(1 << 20);
            let peak = Arc::new(AtomicUsize::new(0));
            tokio::spawn(serve(server, batch, short, peak.clone()));
            let sftp = Sftp::over(client).await.unwrap();
            let mut sink = Vec::new();
            let copied =
                tokio::time::timeout(Duration::from_secs(30), sftp.download("/f", &mut sink))
                    .await
                    .expect("download finishes")
                    .unwrap();
            assert_eq!(copied as usize, FILE_LEN, "batch {batch}, short {short}");
            assert!(
                sink == content(),
                "bytes in file order, batch {batch}, short {short}"
            );
            assert!(
                peak.load(Ordering::Relaxed) >= batch.min(2),
                "several reads were in flight at once"
            );
        }
    }

    #[tokio::test]
    async fn a_read_error_in_the_middle_fails_the_download() {
        // The server never answers with data past the first chunk and says
        // permission denied instead.
        let (client, mut server) = duplex(1 << 20);
        tokio::spawn(async move {
            while let Some(request) = read_request(&mut server).await {
                match request {
                    Packet::Init(_) => send(&mut server, Packet::Version(Version::new())).await,
                    Packet::Open(open) => {
                        send(
                            &mut server,
                            Packet::Handle(Handle {
                                id: open.id,
                                handle: "h".into(),
                            }),
                        )
                        .await
                    }
                    Packet::Read(read) if read.offset == 0 => {
                        send(
                            &mut server,
                            Packet::Data(Data {
                                id: read.id,
                                data: vec![7; read.len as usize],
                            }),
                        )
                        .await
                    }
                    Packet::Read(read) => {
                        send(
                            &mut server,
                            Packet::error(read.id, StatusCode::PermissionDenied),
                        )
                        .await
                    }
                    other => {
                        send(
                            &mut server,
                            Packet::error(other.get_request_id(), StatusCode::Ok),
                        )
                        .await
                    }
                }
            }
        });
        let sftp = Sftp::over(client).await.unwrap();
        let mut sink = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(30), sftp.download("/f", &mut sink))
            .await
            .expect("download ends");
        assert!(
            result.is_err(),
            "the error is reported, not read as the end"
        );
    }
}
