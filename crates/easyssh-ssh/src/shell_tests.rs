//! A shell against an in-process russh server: the terminal modes and the
//! environment arrive as requests before the shell starts.

use crate::interaction::{Decision, Interaction, KbdPrompt};
use crate::known_hosts::KnownHostStatus;
use crate::{ConnectParams, Session, ShellOptions};
use async_trait::async_trait;
use russh::server::{Auth, Handler, Msg, Server, Session as ServerSession};
use russh::{Channel, ChannelId, MethodKind, MethodSet, Pty};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

struct Login;

#[async_trait]
impl Interaction for Login {
    async fn verify_host_key(&self, _: &str, _: u16, _: &str, _: KnownHostStatus) -> Decision {
        Decision::AcceptOnce
    }
    async fn password(&self, _: &str) -> Option<String> {
        Some("secret".into())
    }
    async fn keyboard_interactive(&self, _: &str, _: &str, _: &[KbdPrompt]) -> Option<Vec<String>> {
        None
    }
    async fn passphrase(&self, _: &Path) -> Option<String> {
        None
    }
}

/// What the server saw, in order.
#[derive(Clone, Default)]
struct Recorder {
    seen: Arc<Mutex<Vec<String>>>,
    modes: Arc<Mutex<Vec<(Pty, u32)>>>,
}

impl Recorder {
    fn push(&self, event: String) {
        self.seen.lock().unwrap().push(event);
    }
}

impl Handler for Recorder {
    type Error = russh::Error;

    async fn auth_password(&mut self, _: &str, password: &str) -> Result<Auth, Self::Error> {
        Ok(if password == "secret" {
            Auth::Accept
        } else {
            Auth::reject()
        })
    }

    async fn channel_open_session(
        &mut self,
        _: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        modes: &[(Pty, u32)],
        session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        self.push(format!("pty {term} {cols}x{rows}"));
        *self.modes.lock().unwrap() = modes.to_vec();
        session.channel_success(channel)?;
        Ok(())
    }

    async fn env_request(
        &mut self,
        _: ChannelId,
        name: &str,
        value: &str,
        _: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        self.push(format!("env {name}={value}"));
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut ServerSession,
    ) -> Result<(), Self::Error> {
        self.push("shell".into());
        session.channel_success(channel)?;
        session.data(channel, &b"ready"[..])?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

impl Server for Recorder {
    type Handler = Recorder;

    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Recorder {
        self.clone()
    }
}

async fn serve_once(recorder: Recorder) -> u16 {
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
        if let Ok(session) = russh::server::run_stream(config, stream, recorder).await {
            let _ = session.await;
        }
    });
    port
}

#[tokio::test]
async fn the_shell_sends_terminal_modes_and_environment_first() {
    let recorder = Recorder::default();
    let port = serve_once(recorder.clone()).await;
    let mut params = ConnectParams::new("127.0.0.1", port, "someone");
    params.agent = crate::AgentSource::Disabled;
    params.keyboard_interactive = false;
    params.connect_timeout = Duration::from_secs(10);
    let session = tokio::time::timeout(Duration::from_secs(20), Session::connect(params, &Login))
        .await
        .expect("the login finished")
        .expect("logged in");

    let mut shell = tokio::time::timeout(
        Duration::from_secs(20),
        session.shell(ShellOptions {
            cols: 100,
            rows: 30,
            env: crate::shell_env(Vec::new(), Some("zh"), &[]),
            ..ShellOptions::default()
        }),
    )
    .await
    .expect("the shell opened in time")
    .expect("shell");
    assert_eq!(shell.read().await.as_deref(), Some(&b"ready"[..]));
    assert_eq!(shell.wait_exit().await, crate::ExitStatus::Code(0));

    assert_eq!(
        *recorder.seen.lock().unwrap(),
        [
            "pty xterm-256color 100x30",
            "env COLORTERM=truecolor",
            "env LANG=zh_CN.UTF-8",
            "shell",
        ]
    );
    let modes = recorder.modes.lock().unwrap().clone();
    assert!(modes.contains(&(Pty::VERASE, 0x7f)), "{modes:?}");
    assert!(modes.contains(&(Pty::IUTF8, 1)), "{modes:?}");
}
