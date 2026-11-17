use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame of {len} bytes exceeds the limit of {max} bytes")]
    FrameTooLarge { len: u64, max: u32 },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("authentication failed")]
    Auth,
    #[error("bad message authentication code")]
    BadMac,
    #[error("unexpected sequence number: expected {expected}, got {got}")]
    BadSequence { expected: u64, got: u64 },
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("another easysshd instance is already running in {0}")]
    AlreadyRunning(PathBuf),
    #[error("insecure runtime directory {path}: {reason}")]
    InsecurePath { path: PathBuf, reason: String },
    #[error("socket path {path} is {len} bytes, but this platform allows at most {max}; use a shorter --runtime-dir")]
    SocketPathTooLong {
        path: PathBuf,
        len: usize,
        max: usize,
    },
    #[error("connection closed")]
    Closed,
    #[error("timed out")]
    Timeout,
}
