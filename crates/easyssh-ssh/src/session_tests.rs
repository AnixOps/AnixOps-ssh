//! A session against an in-process russh server: a refused session channel
//! is told apart from other channel errors, and a ping answers.

use crate::interaction::{Decision, Interaction, KbdPrompt};
use crate::known_hosts::KnownHostStatus;
use crate::{ConnectParams, Error, Session};
use async_trait::async_trait;
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server};
use russh::{Channel, ChannelOpenFailure, MethodKind, MethodSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;

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

/// Accepts any password and refuses every session channel, as OpenSSH does
/// past `MaxSessions`.
#[derive(Clone)]
struct Full;

impl Handler for Full {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, _: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply
            .reject(ChannelOpenFailure::AdministrativelyProhibited)
            .await;
        Ok(())
    }
}

impl Server for Full {
    type Handler = Full;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Full {
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
        if let Ok(session) = russh::server::run_stream(config, stream, Full).await {
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

#[tokio::test]
async fn a_refused_session_channel_is_its_own_error_and_the_connection_still_answers() {
    let session = connect().await;
    let error = tokio::time::timeout(Duration::from_secs(10), session.exec("true"))
        .await
        .expect("the refusal came back in time")
        .unwrap_err();
    assert!(matches!(error, Error::ChannelRefused(_)), "{error:?}");
    assert!(
        error.to_string().contains("administratively prohibited"),
        "{error}"
    );
    session.ping().await.expect("the connection still answers");
}

#[test]
fn keepalive_defaults_match_openssh_fallback() {
    let params = ConnectParams::new("h", 22, "u");
    assert_eq!(params.keepalive, Some(crate::DEFAULT_KEEPALIVE));
    assert_eq!(params.keepalive_max, crate::DEFAULT_KEEPALIVE_MAX);
    assert_eq!(crate::DEFAULT_KEEPALIVE, Duration::from_secs(60));
    assert_eq!(crate::DEFAULT_KEEPALIVE_MAX, 3);
}
