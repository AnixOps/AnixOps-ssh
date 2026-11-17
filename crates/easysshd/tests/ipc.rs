use easysshd::frame::{read_frame, write_frame, MAX_FRAME};
use easysshd::session::{decode_nonce, encode_nonce, random_bytes, Role, Secret, Session};
use easysshd::transport::{self, Conn};
use easysshd::{builtin_handler, Client, Error, Paths, Server};
use serde_json::{json, Value};
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

struct Running {
    dir: TempDir,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl Running {
    fn path(&self) -> std::path::PathBuf {
        self.dir.path().join("run")
    }
    async fn stop(mut self) {
        let _ = self.stop.take().unwrap().send(());
        self.task.await.unwrap();
    }
}

fn start() -> Running {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("run");
    let server = Server::start(&rt, builtin_handler()).unwrap();
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        server
            .serve(async {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    Running {
        dir,
        stop: Some(tx),
        task,
    }
}

fn secret_of(rt: &std::path::Path) -> Secret {
    std::fs::read(rt.join("easysshd.secret"))
        .unwrap()
        .try_into()
        .unwrap()
}

/// Raw handshake with an explicit secret, leaving the caller to craft frames.
async fn raw_handshake(rt: &std::path::Path, secret: &Secret) -> (Conn, Session) {
    let paths = Paths::new(rt).unwrap();
    let mut io = transport::connect(&paths).await.unwrap();
    let cn = random_bytes();
    let hello = json!({"hello": 1, "client_nonce": encode_nonce(&cn)});
    write_frame(&mut io, &serde_json::to_vec(&hello).unwrap())
        .await
        .unwrap();
    let reply = read_frame(&mut io, 1024).await.unwrap();
    let reply: Value = serde_json::from_slice(&reply).unwrap();
    let sn = decode_nonce(reply["server_nonce"].as_str().unwrap()).unwrap();
    (io, Session::new(secret, &cn, &sn, Role::Client))
}

/// Server must close without sending a payload frame we can parse as a reply.
async fn expect_closed(io: &mut Conn) {
    let mut buf = [0u8; 256];
    let r = tokio::time::timeout(Duration::from_secs(5), async {
        // Drain whatever the server already sent (e.g. its "ready" frame).
        loop {
            match io.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => continue,
            }
        }
    })
    .await;
    assert!(r.is_ok(), "connection was not closed");
}

async fn ping_ok(c: &mut Client) {
    c.send(json!({"type": "ping"})).await.unwrap();
    assert_eq!(c.recv().await.unwrap(), json!({"type": "pong"}));
}

#[tokio::test]
async fn handshake_and_ping_pong() {
    let s = start();
    let mut c = Client::connect(s.path()).await.unwrap();
    ping_ok(&mut c).await;
    ping_ok(&mut c).await; // sequence numbers keep advancing
    s.stop().await;
}

#[tokio::test]
async fn files_are_removed_on_shutdown() {
    let s = start();
    let rt = s.path();
    assert!(rt.join("easysshd.secret").exists());
    s.stop().await;
    for f in ["easysshd.secret", "easysshd.lock", "easysshd.sock"] {
        assert!(!rt.join(f).exists(), "{f} left behind");
    }
}

#[tokio::test]
async fn wrong_secret_rejected() {
    let s = start();
    let rt = s.path();

    // Through the client API: the secret file holds the wrong value.
    let real = secret_of(&rt);
    std::fs::write(rt.join("easysshd.secret"), [7u8; 32]).unwrap();
    let err = Client::connect(&rt).await.err().expect("must fail");
    assert!(matches!(err, Error::Auth), "{err:?}");
    std::fs::write(rt.join("easysshd.secret"), real).unwrap();

    // Raw: a client that sends a first frame under the wrong key is dropped.
    let (mut io, mut sess) = raw_handshake(&rt, &[9u8; 32]).await;
    let _ready = read_frame(&mut io, MAX_FRAME).await.unwrap();
    let f = sess.seal(br#"{"type":"ping"}"#);
    write_frame(&mut io, &f).await.unwrap();
    expect_closed(&mut io).await;

    // The server survives and still serves the right secret.
    let mut c = Client::connect(&rt).await.unwrap();
    ping_ok(&mut c).await;
    s.stop().await;
}

#[tokio::test]
async fn tampered_frame_rejected() {
    let s = start();
    let rt = s.path();
    let (mut io, mut sess) = raw_handshake(&rt, &secret_of(&rt)).await;
    let _ready = read_frame(&mut io, MAX_FRAME).await.unwrap();
    let mut f = sess.seal(br#"{"type":"ping"}"#);
    let last = f.len() - 2;
    f[last] ^= 0x01;
    write_frame(&mut io, &f).await.unwrap();
    expect_closed(&mut io).await;
    s.stop().await;
}

#[tokio::test]
async fn replay_rejected() {
    let s = start();
    let rt = s.path();
    let (mut io, mut sess) = raw_handshake(&rt, &secret_of(&rt)).await;
    let _ready = read_frame(&mut io, MAX_FRAME).await.unwrap();
    let f = sess.seal(br#"{"type":"ping"}"#);
    write_frame(&mut io, &f).await.unwrap();
    let pong = read_frame(&mut io, MAX_FRAME).await.unwrap();
    assert!(!pong.is_empty());
    write_frame(&mut io, &f).await.unwrap(); // identical bytes again
    expect_closed(&mut io).await;
    s.stop().await;
}

#[tokio::test]
async fn out_of_order_rejected() {
    let s = start();
    let rt = s.path();
    let (mut io, mut sess) = raw_handshake(&rt, &secret_of(&rt)).await;
    let _ready = read_frame(&mut io, MAX_FRAME).await.unwrap();
    let f0 = sess.seal(br#"{"type":"ping"}"#);
    let f1 = sess.seal(br#"{"type":"ping"}"#);
    // seq 1 before seq 0: a gap at the very first frame.
    write_frame(&mut io, &f1).await.unwrap();
    expect_closed(&mut io).await;
    drop(f0);
    s.stop().await;
}

#[tokio::test]
async fn gap_after_valid_frame_rejected() {
    let s = start();
    let rt = s.path();
    let (mut io, mut sess) = raw_handshake(&rt, &secret_of(&rt)).await;
    let _ready = read_frame(&mut io, MAX_FRAME).await.unwrap();
    write_frame(&mut io, &sess.seal(br#"{"type":"ping"}"#))
        .await
        .unwrap();
    read_frame(&mut io, MAX_FRAME).await.unwrap();
    let _skipped = sess.seal(br#"{"type":"ping"}"#);
    write_frame(&mut io, &sess.seal(br#"{"type":"ping"}"#))
        .await
        .unwrap();
    expect_closed(&mut io).await;
    s.stop().await;
}

#[tokio::test]
async fn oversized_length_rejected_without_body() {
    let s = start();
    let rt = s.path();
    let paths = Paths::new(&rt).unwrap();
    // Pre-handshake limit.
    let mut io = transport::connect(&paths).await.unwrap();
    io.write_all(&u32::MAX.to_be_bytes()).await.unwrap();
    expect_closed(&mut io).await;
    // Post-handshake limit: 4 MiB + 1, and no body is ever sent.
    let (mut io, _sess) = raw_handshake(&rt, &secret_of(&rt)).await;
    let _ready = read_frame(&mut io, MAX_FRAME).await.unwrap();
    io.write_all(&(MAX_FRAME + 1).to_be_bytes()).await.unwrap();
    expect_closed(&mut io).await;
    s.stop().await;
}

#[tokio::test]
async fn second_instance_refused_and_first_unharmed() {
    let s = start();
    let err = Server::start(s.path(), builtin_handler())
        .err()
        .expect("second instance must fail");
    assert!(matches!(err, Error::AlreadyRunning(_)), "{err:?}");
    // The failed start must not have deleted the first instance's files.
    let mut c = Client::connect(s.path()).await.unwrap();
    ping_ok(&mut c).await;
    s.stop().await;
}

#[cfg(unix)]
mod unix_perms {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(p: &std::path::Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[tokio::test]
    async fn permission_bits() {
        let s = start();
        let rt = s.path();
        assert_eq!(mode(&rt), 0o700);
        assert_eq!(mode(&rt.join("easysshd.sock")), 0o600);
        assert_eq!(mode(&rt.join("easysshd.secret")), 0o600);
        assert_eq!(mode(&rt.join("easysshd.lock")), 0o600);
        s.stop().await;
    }

    #[tokio::test]
    async fn loose_directory_refused() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join("run");
        std::fs::create_dir(&rt).unwrap();
        std::fs::set_permissions(&rt, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = Server::start(&rt, builtin_handler())
            .err()
            .expect("must refuse");
        assert!(matches!(err, Error::InsecurePath { .. }), "{err:?}");
        assert!(!rt.join("easysshd.secret").exists());
    }

    #[tokio::test]
    async fn loose_secret_refused_by_client() {
        let s = start();
        let sp = s.path().join("easysshd.secret");
        std::fs::set_permissions(&sp, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = Client::connect(s.path()).await.err().expect("must refuse");
        assert!(matches!(err, Error::InsecurePath { .. }), "{err:?}");
        s.stop().await;
    }

    #[test]
    fn long_socket_path_rejected() {
        let long = std::path::PathBuf::from(format!("/tmp/{}", "a".repeat(120)));
        let err = Paths::new(long).unwrap_err();
        assert!(matches!(err, Error::SocketPathTooLong { .. }), "{err:?}");
    }
}
