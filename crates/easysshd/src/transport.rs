//! Platform transport: Unix domain socket, or Windows named pipe.

use crate::error::Result;
use crate::runtime::Paths;
use tokio::io::{AsyncRead, AsyncWrite};

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
pub type Conn = Box<dyn Io>;

#[cfg(unix)]
pub use unix_impl::{connect, Listener};
#[cfg(windows)]
pub use windows_impl::{connect, Listener};

#[cfg(unix)]
mod unix_impl {
    use super::*;
    use crate::error::Error;
    use crate::platform;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use tokio::net::{UnixListener, UnixStream};

    pub struct Listener {
        inner: UnixListener,
    }

    impl Listener {
        /// Bind the socket. The caller holds the instance lock, so any socket
        /// file already there is stale and may be replaced; anything that is
        /// not a socket is left alone and reported.
        pub fn bind(paths: &Paths) -> Result<Self> {
            match std::fs::symlink_metadata(&paths.socket) {
                Ok(md) if md.file_type().is_socket() => std::fs::remove_file(&paths.socket)?,
                Ok(_) => {
                    return Err(Error::InsecurePath {
                        path: paths.socket.clone(),
                        reason: "exists and is not a socket".into(),
                    })
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            let inner = UnixListener::bind(&paths.socket)?;
            // The 0700 directory already keeps others out; tighten the socket too.
            std::fs::set_permissions(&paths.socket, std::fs::Permissions::from_mode(0o600))?;
            Ok(Listener { inner })
        }

        pub async fn accept(&mut self) -> std::io::Result<Conn> {
            let (s, _) = self.inner.accept().await?;
            // Defence in depth: refuse peers that are not the same user.
            let cred = s.peer_cred()?;
            if cred.uid() != platform::euid() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "peer uid mismatch",
                ));
            }
            Ok(Box::new(s))
        }
    }

    pub async fn connect(paths: &Paths) -> Result<Conn> {
        Ok(Box::new(UnixStream::connect(&paths.socket).await?))
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::platform::OwnerOnlySd;
    use sha2::{Digest, Sha256};
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    fn pipe_name(paths: &Paths) -> String {
        let d = Sha256::digest(paths.dir.to_string_lossy().to_lowercase().as_bytes());
        let hex: String = d.iter().take(8).map(|b| format!("{b:02x}")).collect();
        format!(r"\\.\pipe\easysshd-{hex}")
    }

    pub struct Listener {
        name: String,
        sd: OwnerOnlySd,
        next: NamedPipeServer,
    }

    impl Listener {
        fn create(name: &str, sd: &OwnerOnlySd, first: bool) -> std::io::Result<NamedPipeServer> {
            let mut sa = sd.attributes();
            let mut o = ServerOptions::new();
            o.first_pipe_instance(first).reject_remote_clients(true);
            // SAFETY: `sa` and the descriptor it points to outlive the call.
            unsafe {
                o.create_with_security_attributes_raw(
                    name,
                    &mut sa as *mut _ as *mut std::ffi::c_void,
                )
            }
        }

        pub fn bind(paths: &Paths) -> Result<Self> {
            let name = pipe_name(paths);
            let sd = OwnerOnlySd::new(false)?;
            // first_pipe_instance fails if another process already owns the name.
            let next = Self::create(&name, &sd, true)?;
            Ok(Listener { name, sd, next })
        }

        pub async fn accept(&mut self) -> std::io::Result<Conn> {
            self.next.connect().await?;
            let fresh = Self::create(&self.name, &self.sd, false)?;
            Ok(Box::new(std::mem::replace(&mut self.next, fresh)))
        }
    }

    pub async fn connect(paths: &Paths) -> Result<Conn> {
        Ok(Box::new(ClientOptions::new().open(pipe_name(paths))?))
    }
}
