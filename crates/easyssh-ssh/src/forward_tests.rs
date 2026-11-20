//! Forwards against an in-process russh server: connections the server will
//! not pass on are counted with their reason, and the forward keeps running.

use crate::interaction::{Decision, Interaction, KbdPrompt};
use crate::known_hosts::KnownHostStatus;
use crate::{ConnectParams, Session};
use async_trait::async_trait;
use russh::server::{Auth, Handler, Server};
use russh::{Channel, ChannelOpenFailure, MethodKind, MethodSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

struct Trusting;

#[async_trait]
impl Interaction for Trusting {
    async fn verify_host_key(&self, _: &str, _: u16, _: &str, _: KnownHostStatus) -> Decision {
        Decision::AcceptOnce
    }
    async fn password(&self, _: &str) -> Option<String> {
        Some("pw".into())
    }
    async fn keyboard_interactive(&self, _: &str, _: &str, _: &[KbdPrompt]) -> Option<Vec<String>> {
        None
    }
    async fn passphrase(&self, _: &Path) -> Option<String> {
        None
    }
}

/// Accepts any password and refuses every `direct-tcpip` channel: port 2
/// as administratively prohibited, any other as a failed connect.
#[derive(Clone)]
struct Refusing;

impl Handler for Refusing {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        _channel: Channel<russh::server::Msg>,
        _host: &str,
        port: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        let reason = if port == 2 {
            ChannelOpenFailure::AdministrativelyProhibited
        } else {
            ChannelOpenFailure::ConnectFailed
        };
        reply.reject(reason).await;
        Ok(())
    }
}

impl Server for Refusing {
    type Handler = Refusing;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Refusing {
        self.clone()
    }
}

async fn connect() -> Session {
    let host_key =
        russh::keys::decode_secret_key(include_str!("../tests/fixtures/plain_ed25519"), None)
            .unwrap();
    let config = Arc::new(russh::server::Config {
        keys: vec![host_key],
        methods: MethodSet::from(&[MethodKind::Password][..]),
        auth_rejection_time: Duration::from_millis(1),
        auth_rejection_time_initial: Some(Duration::from_millis(1)),
        ..Default::default()
    });
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        if let Ok(session) = russh::server::run_stream(config, stream, Refusing).await {
            let _ = session.await;
        }
    });
    let mut params = ConnectParams::new("127.0.0.1", port, "someone");
    params.agent = crate::AgentSource::Disabled;
    params.keyboard_interactive = false;
    params.connect_timeout = Duration::from_secs(10);
    tokio::time::timeout(Duration::from_secs(20), Session::connect(params, &Trusting))
        .await
        .expect("the login finished")
        .expect("logged in")
}

async fn knock(port: u16) {
    let _ = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
}

#[tokio::test]
async fn refused_channels_are_counted_and_the_forward_keeps_listening() {
    let session = connect().await;
    let forward = session
        .forward_local("127.0.0.1", 0, "127.0.0.1", 1)
        .await
        .unwrap();
    let port = forward.local_addr().unwrap().port();
    let mut failures = forward.failures();
    knock(port).await;
    tokio::time::timeout(Duration::from_secs(10), failures.wait_for(|f| f.count == 1))
        .await
        .expect("the failure was reported")
        .unwrap();
    assert_eq!(
        failures.borrow().last.as_deref(),
        Some("127.0.0.1:1: connect failed")
    );
    // Still listening: a second connection fails the same way.
    knock(port).await;
    tokio::time::timeout(Duration::from_secs(10), failures.wait_for(|f| f.count == 2))
        .await
        .expect("the second failure was reported")
        .unwrap();
    forward.stop().await.unwrap();
}

#[tokio::test]
async fn a_prohibited_socks_target_is_reported() {
    let session = connect().await;
    let forward = session.forward_dynamic("127.0.0.1", 0).await.unwrap();
    let port = forward.local_addr().unwrap().port();
    let mut failures = forward.failures();
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    tcp.write_all(&[5, 1, 0]).await.unwrap();
    let mut reply = [0u8; 2];
    tcp.read_exact(&mut reply).await.unwrap();
    // CONNECT 10.0.0.1:2
    tcp.write_all(&[5, 1, 0, 1, 10, 0, 0, 1, 0, 2])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), failures.wait_for(|f| f.count == 1))
        .await
        .expect("the failure was reported")
        .unwrap();
    assert_eq!(
        failures.borrow().last.as_deref(),
        Some("10.0.0.1:2: administratively prohibited")
    );
    forward.stop().await.unwrap();
}
