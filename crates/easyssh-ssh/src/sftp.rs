//! SFTP on a channel of the same connection.

use crate::error::{Error, Result};
use crate::session::Session;
use russh_sftp::client::SftpSession;
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

pub struct Sftp {
    session: SftpSession,
}

impl Session {
    /// Opens an SFTP channel on this connection.
    pub async fn sftp(&self) -> Result<Sftp> {
        let channel = self.open_session_channel().await?;
        channel.request_subsystem(true, "sftp").await?;
        let session = SftpSession::new(channel.into_stream()).await?;
        Ok(Sftp { session })
    }
}

impl Sftp {
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

    /// Sets the permission bits (`0o644` style) and nothing else.
    pub async fn set_permissions(&self, path: &str, mode: u32) -> Result<()> {
        self.set_stat(path, Some(mode), None).await
    }

    /// Sets the permission bits of `path` and, with `recursive`, of
    /// everything in it. Links are skipped: setting the mode of a link would
    /// change what it points to, possibly outside the folder.
    pub async fn set_permissions_all(&self, path: &str, mode: u32, recursive: bool) -> Result<()> {
        if !recursive {
            return self.set_permissions(path, mode).await;
        }
        let mut pending = vec![path.to_owned()];
        while let Some(current) = pending.pop() {
            let stat = self.lstat(&current).await?;
            match stat.kind {
                EntryKind::Symlink if current != path => continue,
                EntryKind::Dir => {
                    for entry in self.list(&current).await? {
                        if entry.name.is_empty() || entry.name.contains('/') {
                            return Err(Error::Sftp(format!(
                                "{current} lists an entry that cannot be changed: {:?}",
                                entry.name
                            )));
                        }
                        pending.push(if current.ends_with('/') {
                            format!("{current}{}", entry.name)
                        } else {
                            format!("{current}/{}", entry.name)
                        });
                    }
                }
                _ => {}
            }
            self.set_permissions(&current, mode).await?;
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
