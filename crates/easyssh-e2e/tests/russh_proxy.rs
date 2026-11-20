//! The built-in engine through outbound proxies, to the e2e target. The
//! proxies are small in-process forwarders (SOCKS5 and HTTP CONNECT) that
//! record what the client asked for, so the tests see that the host name
//! reaches the proxy unresolved and that only the first hop uses it.

#![cfg(unix)]

use base64::Engine as _;
use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::{E2e, TARGET_PORT};
use easyssh_ssh::socks5::{accept_request, reply, send_reply};
use easyssh_ssh::{Error, Proxy, ProxyFailure, Session};
use std::sync::{Arc, Mutex};
use tokio::io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type Seen = Arc<Mutex<Vec<String>>>;

/// A SOCKS5 proxy without authentication; returns its port.
async fn socks5_proxy(seen: Seen) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                return;
            };
            let seen = seen.clone();
            tokio::spawn(async move {
                let Ok(request) = accept_request(&mut client).await else {
                    return;
                };
                seen.lock()
                    .unwrap()
                    .push(format!("{}:{}", request.host, request.port));
                match TcpStream::connect((request.host.as_str(), request.port)).await {
                    Ok(mut upstream) => {
                        if send_reply(&mut client, reply::SUCCEEDED).await.is_ok() {
                            let _ = copy_bidirectional(&mut client, &mut upstream).await;
                        }
                    }
                    Err(_) => {
                        let _ = send_reply(&mut client, reply::CONNECTION_REFUSED).await;
                    }
                }
            });
        }
    });
    port
}

/// An HTTP CONNECT proxy that wants `user:password` as basic authentication.
async fn http_proxy(login: &'static str, seen: Seen) -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let expected = format!(
        "Proxy-Authorization: Basic {}",
        base64::engine::general_purpose::STANDARD.encode(login)
    );
    tokio::spawn(async move {
        loop {
            let Ok((mut client, _)) = listener.accept().await else {
                return;
            };
            let (seen, expected) = (seen.clone(), expected.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    if client.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let authority = head
                    .lines()
                    .next()
                    .and_then(|line| line.strip_prefix("CONNECT "))
                    .and_then(|rest| rest.split(' ').next())
                    .unwrap_or_default()
                    .to_owned();
                seen.lock().unwrap().push(authority.clone());
                if !head.lines().any(|line| line == expected) {
                    let _ = client
                        .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                        .await;
                    return;
                }
                match TcpStream::connect(authority.as_str()).await {
                    Ok(mut upstream) => {
                        if client
                            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                            .await
                            .is_ok()
                        {
                            let _ = copy_bidirectional(&mut client, &mut upstream).await;
                        }
                    }
                    Err(_) => {
                        let _ = client.write_all(b"HTTP/1.1 502 Bad Gateway\r\n\r\n").await;
                    }
                }
            });
        }
    });
    port
}

fn seen() -> Seen {
    Arc::new(Mutex::new(Vec::new()))
}

/// The e2e target addressed by name, so the proxy has to resolve it.
fn named_target(e2e: &E2e, proxy: Proxy) -> easyssh_ssh::ConnectParams {
    let mut params = e2e.russh_target().proxy(Some(proxy));
    params.host = "localhost".into();
    params
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn exec_through_a_socks5_proxy_sends_the_name_unresolved() {
    let e2e = E2e::new();
    let log = seen();
    let port = socks5_proxy(log.clone()).await;
    let session = within(
        "connect",
        Session::connect(
            named_target(&e2e, Proxy::socks5("127.0.0.1", port)),
            &Scripted::trusting(),
        ),
    )
    .await
    .expect("connect through the proxy");
    let output = within("exec", session.exec("echo via-socks5; hostname"))
        .await
        .expect("exec");
    assert_eq!(output.stdout_text(), "via-socks5\ntarget\n");
    assert_eq!(
        *log.lock().unwrap(),
        vec![format!("localhost:{TARGET_PORT}")]
    );
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn exec_through_an_http_proxy_with_basic_auth_and_a_wrong_password() {
    let e2e = E2e::new();
    let log = seen();
    let port = http_proxy("proxyuser:proxypass", log.clone()).await;

    let good = Proxy::http("127.0.0.1", port).login("proxyuser", "proxypass");
    let session = within(
        "connect",
        Session::connect(named_target(&e2e, good), &Scripted::trusting()),
    )
    .await
    .expect("connect through the proxy");
    let output = within("exec", session.exec("echo via-http"))
        .await
        .expect("exec");
    assert_eq!(output.stdout_text(), "via-http\n");
    assert_eq!(
        *log.lock().unwrap(),
        vec![format!("localhost:{TARGET_PORT}")]
    );

    // A wrong password is the proxy's refusal, said as such, not an SSH error.
    let bad = Proxy::http("127.0.0.1", port).login("proxyuser", "wrong");
    let error = within(
        "connect",
        Session::connect(named_target(&e2e, bad), &Scripted::trusting()),
    )
    .await
    .expect_err("a wrong proxy password must fail");
    match error {
        Error::Proxy(error) => assert_eq!(error.failure, ProxyFailure::AuthFailed),
        other => panic!("expected a proxy error, got {other}"),
    }
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_proxy_reaches_the_first_hop_only() {
    let e2e = E2e::new();
    let log = seen();
    let port = socks5_proxy(log.clone()).await;
    let mut bastion = e2e
        .russh_bastion()
        .proxy(Some(Proxy::socks5("127.0.0.1", port)));
    bastion.host = "localhost".into();
    let session = within(
        "connect via bastion",
        Session::connect_via(
            &[bastion],
            e2e.russh_target_behind_bastion(),
            &Scripted::trusting(),
        ),
    )
    .await
    .expect("connect through the proxy and the bastion");
    let output = within("exec", session.exec("hostname"))
        .await
        .expect("exec");
    assert_eq!(output.stdout_text(), "target\n");
    // One connection through the proxy: the bastion. The target went through
    // the bastion's SSH connection.
    let log = log.lock().unwrap();
    assert_eq!(log.len(), 1, "{log:?}");
    assert!(log[0].starts_with("localhost:"), "{log:?}");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_target_the_proxy_cannot_reach_is_reported_as_unreachable() {
    let e2e = E2e::new();
    let log = seen();
    let port = socks5_proxy(log.clone()).await;
    // A port nothing listens on, found by binding and letting go.
    let closed = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut params = e2e
        .russh_target()
        .proxy(Some(Proxy::socks5("127.0.0.1", port)));
    params.host = "127.0.0.1".into();
    params.port = closed;
    let error = within("connect", Session::connect(params, &Scripted::trusting()))
        .await
        .expect_err("nothing listens there");
    match error {
        Error::Proxy(error) => {
            assert!(
                matches!(error.failure, ProxyFailure::TargetUnreachable(_)),
                "{error}"
            );
            assert!(error.to_string().contains(&format!("127.0.0.1:{closed}")));
        }
        other => panic!("expected a proxy error, got {other}"),
    }
}
