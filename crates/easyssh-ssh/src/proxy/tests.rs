//! The proxy clients against scripted, in-process proxies. Each script checks
//! the exact bytes the client sends and answers with what a real proxy would.

use super::*;
use tokio::io::{duplex, DuplexStream};
use tokio::net::TcpListener;

async fn expect(stream: &mut DuplexStream, want: &[u8]) {
    let mut got = vec![0u8; want.len()];
    stream.read_exact(&mut got).await.expect("client bytes");
    assert_eq!(
        got,
        want,
        "client sent {:?}, expected {:?}",
        String::from_utf8_lossy(&got),
        String::from_utf8_lossy(want)
    );
}

/// Runs the client handshake against `script`; gives the outcome and the
/// client's end of the stream, so a test can see what follows the handshake.
async fn run<F, Fut>(
    proxy: &Proxy,
    host: &str,
    port: u16,
    script: F,
) -> (Result<(), ProxyError>, DuplexStream)
where
    F: FnOnce(DuplexStream) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let (mut client, server) = duplex(8192);
    let task = tokio::spawn(script(server));
    let result = handshake(&mut client, proxy, host, port).await;
    task.await.expect("the scripted proxy panicked");
    (result, client)
}

/// The handshake fails without a single byte reaching the proxy.
async fn fails_before_sending(proxy: &Proxy, host: &str, port: u16) -> ProxyFailure {
    let (mut client, mut server) = duplex(8192);
    let result = handshake(&mut client, proxy, host, port).await;
    drop(client);
    let mut sent = Vec::new();
    server.read_to_end(&mut sent).await.unwrap();
    assert!(sent.is_empty(), "sent {sent:?}");
    failure(result)
}

fn failure(result: Result<(), ProxyError>) -> ProxyFailure {
    result.expect_err("the handshake should fail").failure
}

const OK_REPLY_V4: [u8; 10] = [5, 0, 0, 1, 0, 0, 0, 0, 0, 0];

// ---- SOCKS5 ---------------------------------------------------------------

#[tokio::test]
async fn socks5_without_login_sends_a_domain_name_unresolved() {
    let proxy = Proxy::socks5("127.0.0.1", 7890);
    let (result, mut client) = run(&proxy, "web.internal", 22, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        s.write_all(&[5, 0]).await.unwrap();
        let mut request = vec![5, 1, 0, 3, 12];
        request.extend_from_slice(b"web.internal");
        request.extend_from_slice(&22u16.to_be_bytes());
        expect(&mut s, &request).await;
        s.write_all(&OK_REPLY_V4).await.unwrap();
        // The SSH server's banner follows at once; the client must not eat it.
        s.write_all(b"SSH-2.0-test\r\n").await.unwrap();
    })
    .await;
    result.unwrap();
    let mut banner = [0u8; 14];
    client.read_exact(&mut banner).await.unwrap();
    assert_eq!(&banner, b"SSH-2.0-test\r\n");
}

#[tokio::test]
async fn socks5_sends_ipv4_and_ipv6_literals_as_addresses() {
    let proxy = Proxy::socks5("p", 1080);
    let (result, _) = run(&proxy, "192.0.2.7", 2222, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        s.write_all(&[5, 0]).await.unwrap();
        expect(&mut s, &[5, 1, 0, 1, 192, 0, 2, 7, 0x08, 0xAE]).await;
        s.write_all(&OK_REPLY_V4).await.unwrap();
    })
    .await;
    result.unwrap();

    for spelling in ["2001:db8::1", "[2001:db8::1]"] {
        let (result, _) = run(&proxy, spelling, 22, |mut s| async move {
            expect(&mut s, &[5, 1, 0]).await;
            s.write_all(&[5, 0]).await.unwrap();
            let mut request = vec![5, 1, 0, 4];
            request.extend_from_slice(&"2001:db8::1".parse::<Ipv6Addr>().unwrap().octets());
            request.extend_from_slice(&[0, 22]);
            expect(&mut s, &request).await;
            // An IPv6 bound address in the reply is read in full.
            let mut reply = vec![5, 0, 0, 4];
            reply.extend_from_slice(&[0; 16]);
            reply.extend_from_slice(&[0, 0]);
            s.write_all(&reply).await.unwrap();
            s.write_all(b"X").await.unwrap();
        })
        .await;
        result.unwrap();
    }
}

#[tokio::test]
async fn socks5_reads_a_domain_bound_address_in_the_reply() {
    let proxy = Proxy::socks5("p", 1080);
    let (result, mut client) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        s.write_all(&[5, 0]).await.unwrap();
        expect(&mut s, &[5, 1, 0, 3, 1, b'h', 0, 22]).await;
        s.write_all(&[5, 0, 0, 3, 3, b'a', b'b', b'c', 0x04, 0x00])
            .await
            .unwrap();
        s.write_all(b"Z").await.unwrap();
    })
    .await;
    result.unwrap();
    let mut next = [0u8; 1];
    client.read_exact(&mut next).await.unwrap();
    assert_eq!(&next, b"Z");
}

#[tokio::test]
async fn socks5_logs_in_with_username_and_password() {
    let proxy = Proxy::socks5("p", 1080).login("bob", "pass");
    let (result, _) = run(&proxy, "example.org", 22, |mut s| async move {
        // Both methods are offered when there is a login.
        expect(&mut s, &[5, 2, 0, 2]).await;
        s.write_all(&[5, 2]).await.unwrap();
        expect(&mut s, &[1, 3, b'b', b'o', b'b', 4, b'p', b'a', b's', b's']).await;
        s.write_all(&[1, 0]).await.unwrap();
        let mut request = vec![5, 1, 0, 3, 11];
        request.extend_from_slice(b"example.org");
        request.extend_from_slice(&[0, 22]);
        expect(&mut s, &request).await;
        s.write_all(&OK_REPLY_V4).await.unwrap();
    })
    .await;
    result.unwrap();
}

#[tokio::test]
async fn socks5_with_a_login_still_works_when_the_proxy_wants_none() {
    let proxy = Proxy::socks5("p", 1080).login("bob", "pass");
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 2, 0, 2]).await;
        s.write_all(&[5, 0]).await.unwrap();
        expect(&mut s, &[5, 1, 0, 3, 1, b'h', 0, 22]).await;
        s.write_all(&OK_REPLY_V4).await.unwrap();
    })
    .await;
    result.unwrap();
}

#[tokio::test]
async fn socks5_wrong_password_is_an_authentication_failure() {
    let proxy = Proxy::socks5("p", 1080).login("bob", "nope");
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 2, 0, 2]).await;
        s.write_all(&[5, 2]).await.unwrap();
        expect(&mut s, &[1, 3, b'b', b'o', b'b', 4, b'n', b'o', b'p', b'e']).await;
        s.write_all(&[1, 1]).await.unwrap();
    })
    .await;
    assert_eq!(failure(result), ProxyFailure::AuthFailed);
}

#[tokio::test]
async fn socks5_a_proxy_that_needs_a_login_the_host_does_not_have() {
    let proxy = Proxy::socks5("p", 1080);
    // It answers "no acceptable method" to the offer of no authentication.
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        s.write_all(&[5, 0xFF]).await.unwrap();
    })
    .await;
    assert_eq!(failure(result), ProxyFailure::AuthRequired);

    // Or it picks the login method although it was not offered.
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        s.write_all(&[5, 2]).await.unwrap();
    })
    .await;
    assert_eq!(failure(result), ProxyFailure::AuthRequired);
}

#[tokio::test]
async fn socks5_no_acceptable_method_with_a_login_is_a_refusal() {
    let proxy = Proxy::socks5("p", 1080).login("u", "p");
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 2, 0, 2]).await;
        s.write_all(&[5, 0xFF]).await.unwrap();
    })
    .await;
    assert!(matches!(failure(result), ProxyFailure::Refused(_)));
}

#[tokio::test]
async fn socks5_failure_codes_say_who_said_no() {
    type Case = (u8, fn(&ProxyFailure) -> bool);
    let cases: [Case; 9] = [
        (0x01, |f| matches!(f, ProxyFailure::Refused(_))),
        (0x02, |f| matches!(f, ProxyFailure::Refused(_))),
        (0x03, |f| matches!(f, ProxyFailure::TargetUnreachable(_))),
        (0x04, |f| matches!(f, ProxyFailure::TargetUnreachable(_))),
        (0x05, |f| matches!(f, ProxyFailure::TargetUnreachable(_))),
        (0x06, |f| matches!(f, ProxyFailure::TargetUnreachable(_))),
        (0x07, |f| matches!(f, ProxyFailure::Refused(_))),
        (0x08, |f| matches!(f, ProxyFailure::Refused(_))),
        (0x09, |f| matches!(f, ProxyFailure::BadReply(_))),
    ];
    let proxy = Proxy::socks5("p", 1080);
    for (code, expected) in cases {
        let (result, _) = run(&proxy, "h", 22, move |mut s| async move {
            expect(&mut s, &[5, 1, 0]).await;
            s.write_all(&[5, 0]).await.unwrap();
            expect(&mut s, &[5, 1, 0, 3, 1, b'h', 0, 22]).await;
            s.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0])
                .await
                .unwrap();
        })
        .await;
        let got = failure(result);
        assert!(expected(&got), "code {code:#04x} gave {got:?}");
    }
}

#[tokio::test]
async fn socks5_a_peer_that_is_not_socks_is_reported_as_such() {
    let proxy = Proxy::socks5("p", 1080);
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        s.write_all(b"HT").await.unwrap(); // the start of an HTTP reply
    })
    .await;
    assert!(matches!(failure(result), ProxyFailure::BadReply(_)));

    // The reply version is checked too.
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        s.write_all(&[5, 0]).await.unwrap();
        expect(&mut s, &[5, 1, 0, 3, 1, b'h', 0, 22]).await;
        s.write_all(&[4, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
    })
    .await;
    assert!(matches!(failure(result), ProxyFailure::BadReply(_)));
}

#[tokio::test]
async fn socks5_a_proxy_that_hangs_up_is_a_closed_connection() {
    let proxy = Proxy::socks5("p", 1080);
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        expect(&mut s, &[5, 1, 0]).await;
        // Dropped without a word.
    })
    .await;
    assert_eq!(failure(result), ProxyFailure::Closed);
}

#[tokio::test]
async fn socks5_refuses_what_the_protocol_cannot_carry_before_sending_anything() {
    let proxy = Proxy::socks5("p", 1080);
    let long = "a".repeat(256);
    assert!(matches!(
        fails_before_sending(&proxy, &long, 22).await,
        ProxyFailure::Refused(_)
    ));
    let proxy = Proxy::socks5("p", 1080).login("u".repeat(256), "p");
    assert!(matches!(
        fails_before_sending(&proxy, "h", 22).await,
        ProxyFailure::Refused(_)
    ));
}

// ---- HTTP CONNECT -----------------------------------------------------------

async fn read_head(s: &mut DuplexStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        s.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    String::from_utf8(head).unwrap()
}

#[tokio::test]
async fn http_connect_200_opens_the_tunnel_and_keeps_the_banner() {
    let proxy = Proxy::http("127.0.0.1", 3128);
    let (result, mut client) = run(&proxy, "web.internal", 22, |mut s| async move {
        let head = read_head(&mut s).await;
        assert_eq!(
            head,
            "CONNECT web.internal:22 HTTP/1.1\r\nHost: web.internal:22\r\n\
             Proxy-Connection: keep-alive\r\n\r\n"
        );
        s.write_all(
            b"HTTP/1.1 200 Connection established\r\nProxy-Agent: fake\r\n\r\nSSH-2.0-test\r\n",
        )
        .await
        .unwrap();
    })
    .await;
    result.unwrap();
    let mut banner = [0u8; 14];
    client.read_exact(&mut banner).await.unwrap();
    assert_eq!(&banner, b"SSH-2.0-test\r\n");
}

#[tokio::test]
async fn http_connect_brackets_an_ipv6_target() {
    let proxy = Proxy::http("p", 3128);
    let (result, _) = run(&proxy, "2001:db8::1", 2222, |mut s| async move {
        let head = read_head(&mut s).await;
        assert!(
            head.starts_with("CONNECT [2001:db8::1]:2222 HTTP/1.1\r\n"),
            "{head}"
        );
        assert!(head.contains("\r\nHost: [2001:db8::1]:2222\r\n"));
        s.write_all(b"HTTP/1.0 200 OK\r\n\r\n").await.unwrap();
    })
    .await;
    result.unwrap();
}

#[tokio::test]
async fn http_connect_sends_basic_authentication() {
    // `me:p:w`: only the user name may not contain a colon.
    let proxy = Proxy::http("p", 3128).login("me", "p:w");
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        let head = read_head(&mut s).await;
        let token = base64::engine::general_purpose::STANDARD.encode("me:p:w");
        assert!(
            head.contains(&format!("\r\nProxy-Authorization: Basic {token}\r\n")),
            "{head}"
        );
        s.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();
    })
    .await;
    result.unwrap();
}

#[tokio::test]
async fn http_connect_407_means_login_needed_or_login_refused() {
    let reply = b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                  Proxy-Authenticate: Basic realm=\"x\"\r\nContent-Length: 0\r\n\r\n";
    let proxy = Proxy::http("p", 3128);
    let (result, _) = run(&proxy, "h", 22, move |mut s| async move {
        let head = read_head(&mut s).await;
        assert!(!head.contains("Proxy-Authorization"));
        s.write_all(reply).await.unwrap();
    })
    .await;
    assert_eq!(failure(result), ProxyFailure::AuthRequired);

    let proxy = Proxy::http("p", 3128).login("me", "wrong");
    let (result, _) = run(&proxy, "h", 22, move |mut s| async move {
        let head = read_head(&mut s).await;
        assert!(head.contains("Proxy-Authorization: Basic "));
        s.write_all(reply).await.unwrap();
    })
    .await;
    assert_eq!(failure(result), ProxyFailure::AuthFailed);
}

#[tokio::test]
async fn http_connect_other_statuses_say_who_said_no() {
    let proxy = Proxy::http("p", 3128);
    for (status, reason, refused) in [
        (403, "Forbidden", true),
        (400, "Bad Request", true),
        (405, "Method Not Allowed", true),
        (502, "Bad Gateway", false),
        (503, "Service Unavailable", false),
        (504, "Gateway Timeout", false),
    ] {
        let (result, _) = run(&proxy, "h", 22, move |mut s| async move {
            read_head(&mut s).await;
            s.write_all(format!("HTTP/1.1 {status} {reason}\r\n\r\n").as_bytes())
                .await
                .unwrap();
        })
        .await;
        let got = failure(result);
        match (&got, refused) {
            (ProxyFailure::Refused(text), true)
            | (ProxyFailure::TargetUnreachable(text), false) => {
                assert!(
                    text.contains(&status.to_string()) && text.contains(reason),
                    "{text}"
                );
            }
            _ => panic!("status {status} gave {got:?}"),
        }
    }
}

#[tokio::test]
async fn http_connect_malformed_replies_are_reported_not_trusted() {
    let proxy = Proxy::http("p", 3128);
    for garbage in [
        &b"SSH-2.0-OpenSSH_9.6\r\n\r\n"[..],
        b"HTTP/1.1 abc OK\r\n\r\n",
        b"\x05\x00\x00\x01\r\n\r\n",
        b"\r\n\r\n",
    ] {
        let (result, _) = run(&proxy, "h", 22, move |mut s| async move {
            read_head(&mut s).await;
            s.write_all(garbage).await.unwrap();
        })
        .await;
        assert!(
            matches!(failure(result), ProxyFailure::BadReply(_)),
            "{:?}",
            String::from_utf8_lossy(garbage)
        );
    }
}

#[tokio::test]
async fn http_connect_gives_up_on_an_endless_head_and_on_a_hang_up() {
    let proxy = Proxy::http("p", 3128);
    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        read_head(&mut s).await;
        s.write_all(b"HTTP/1.1 200 OK\r\n").await.unwrap();
        // Header lines without end; the client stops reading at its limit.
        let filler = [b'a'; 1024];
        for _ in 0..17 {
            if s.write_all(&filler).await.is_err() {
                break;
            }
        }
    })
    .await;
    assert!(matches!(failure(result), ProxyFailure::BadReply(_)));

    let (result, _) = run(&proxy, "h", 22, |mut s| async move {
        read_head(&mut s).await;
        s.write_all(b"HTTP/1.1 20").await.unwrap();
    })
    .await;
    assert_eq!(failure(result), ProxyFailure::Closed);
}

#[tokio::test]
async fn http_connect_refuses_a_target_or_user_that_could_inject_headers() {
    let proxy = Proxy::http("p", 3128);
    assert!(matches!(
        fails_before_sending(&proxy, "h\r\nX: y", 22).await,
        ProxyFailure::Refused(_)
    ));
    let proxy = Proxy::http("p", 3128).login("a:b", "p");
    assert!(matches!(
        fails_before_sending(&proxy, "h", 22).await,
        ProxyFailure::Refused(_)
    ));
}

// ---- messages and real sockets ------------------------------------------------

#[test]
fn messages_name_the_proxy_and_the_target_but_never_the_login() {
    let proxy = Proxy::socks5("127.0.0.1", 7890).login("bob", "hunter2");
    assert_eq!(proxy.display(), "socks5://127.0.0.1:7890");
    assert!(!format!("{proxy:?}").contains("hunter2"));
    assert!(!format!("{proxy:?}").contains("bob"));
    assert_eq!(Proxy::http("::1", 3128).display(), "http://[::1]:3128");

    let error = |failure| {
        ProxyError {
            proxy: proxy.display(),
            target: "web:22".into(),
            failure,
        }
        .to_string()
    };
    assert!(
        error(ProxyFailure::Unreachable("connection refused".into()))
            .starts_with("cannot connect to the proxy socks5://127.0.0.1:7890")
    );
    assert!(error(ProxyFailure::AuthFailed).contains("rejected the user name or password"));
    assert!(error(ProxyFailure::AuthRequired).contains("requires a user name and password"));
    let refused = error(ProxyFailure::Refused("not allowed".into()));
    assert!(
        refused.contains("refused to connect to web:22"),
        "{refused}"
    );
    let unreachable = error(ProxyFailure::TargetUnreachable("host unreachable".into()));
    assert!(
        unreachable.contains("could not reach web:22"),
        "{unreachable}"
    );
    for text in [error(ProxyFailure::Timeout), error(ProxyFailure::Closed)] {
        assert!(!text.contains("hunter2") && text.contains("socks5://127.0.0.1:7890"));
    }
}

#[tokio::test]
async fn connect_through_reaches_the_target_over_a_real_socket() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut greeting = [0u8; 3];
        socket.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        socket.write_all(&[5, 0]).await.unwrap();
        let mut request = [0u8; 5 + 4 + 2];
        socket.read_exact(&mut request).await.unwrap();
        assert_eq!(&request[..5], &[5, 1, 0, 3, 4]);
        assert_eq!(&request[5..9], b"host");
        socket.write_all(&OK_REPLY_V4).await.unwrap();
        socket.write_all(b"hello").await.unwrap();
    });
    let proxy = Proxy::socks5("127.0.0.1", port);
    let mut stream = connect_through(&proxy, "host", 22, Duration::from_secs(5))
        .await
        .unwrap();
    let mut hello = [0u8; 5];
    stream.read_exact(&mut hello).await.unwrap();
    assert_eq!(&hello, b"hello");
    server.await.unwrap();
}

#[tokio::test]
async fn connect_through_tells_an_unreachable_proxy_from_a_silent_one() {
    // Nothing listens on a port that was just released.
    let port = {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        listener.local_addr().unwrap().port()
    };
    let proxy = Proxy::socks5("127.0.0.1", port);
    let error = connect_through(&proxy, "host", 22, Duration::from_secs(5))
        .await
        .unwrap_err();
    assert!(
        matches!(error.failure, ProxyFailure::Unreachable(_)),
        "{error}"
    );
    assert!(error.to_string().contains(&format!("127.0.0.1:{port}")));

    // A proxy that accepts and then says nothing runs into the timeout.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let _hold = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(10)).await;
        drop(socket);
    });
    let proxy = Proxy::http("127.0.0.1", port);
    let error = connect_through(&proxy, "host", 22, Duration::from_millis(200))
        .await
        .unwrap_err();
    assert_eq!(error.failure, ProxyFailure::Timeout);
}
