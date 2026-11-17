#![cfg(unix)]

use easyssh_e2e::russh_support::{
    echo_roundtrip, wait_for_port_open, wait_for_port_shut, within, within_for, Scripted,
};
use easyssh_e2e::{E2e, ECHO_PORT};
use easyssh_ssh::{Error, ForwardKind, Session};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const REMOTE_PORT: u16 = 17101;
const UP: Duration = Duration::from_secs(30);

async fn connect(e2e: &E2e) -> Session {
    within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect")
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn local_forward_reaches_the_echo_service() {
    let e2e = E2e::new();
    let session = connect(&e2e).await;
    let forward = within(
        "forward",
        session.forward_local("127.0.0.1", 0, "127.0.0.1", ECHO_PORT),
    )
    .await
    .expect("forward");
    assert_eq!(forward.kind(), ForwardKind::Local);
    let port = forward.local_addr().expect("address").port();
    wait_for_port_open(port, UP).await;

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    echo_roundtrip(&mut stream, "local-forward").await;
    echo_roundtrip(&mut stream, "second line").await;
    // A second connection through the same forward.
    let mut other = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    echo_roundtrip(&mut other, "parallel").await;
    drop(stream);
    drop(other);

    within("stop", forward.stop()).await.expect("stop");
    wait_for_port_shut(port, Duration::from_secs(10)).await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn dynamic_forward_speaks_socks5() {
    let e2e = E2e::new();
    let session = connect(&e2e).await;
    let forward = within("forward", session.forward_dynamic("127.0.0.1", 0))
        .await
        .expect("forward");
    assert_eq!(forward.kind(), ForwardKind::Dynamic);
    let port = forward.local_addr().expect("address").port();
    wait_for_port_open(port, UP).await;

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    within_for("socks5", Duration::from_secs(20), async {
        // Greeting: version 5, one method, "no authentication".
        stream.write_all(&[5, 1, 0]).await.unwrap();
        let mut reply = [0u8; 2];
        stream.read_exact(&mut reply).await.expect("method reply");
        assert_eq!(reply, [5, 0]);
        // CONNECT 127.0.0.1:ECHO_PORT, as seen from the server.
        let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
        request.extend_from_slice(&ECHO_PORT.to_be_bytes());
        stream.write_all(&request).await.unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).await.expect("connect reply");
        assert_eq!(reply[..2], [5, 0], "SOCKS connect refused: {reply:?}");
    })
    .await;
    echo_roundtrip(&mut stream, "through-socks").await;
    drop(stream);

    // A refused destination is answered with a failure code, not a hang.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    within_for("socks5 refused", Duration::from_secs(20), async {
        stream.write_all(&[5, 1, 0]).await.unwrap();
        let mut reply = [0u8; 2];
        stream.read_exact(&mut reply).await.unwrap();
        let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
        request.extend_from_slice(&1u16.to_be_bytes()); // nothing listens on port 1
        stream.write_all(&request).await.unwrap();
        let mut reply = [0u8; 10];
        stream.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply[0], 5);
        assert_ne!(reply[1], 0, "connecting to a closed port must fail");
    })
    .await;
    drop(stream);

    within("stop", forward.stop()).await.expect("stop");
    wait_for_port_shut(port, Duration::from_secs(10)).await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn remote_forward_reaches_a_local_listener() {
    let e2e = E2e::new();
    let session = connect(&e2e).await;

    // A local service that the server should be able to reach via -R.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let local_port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut line = String::new();
                if reader.read_line(&mut line).await.is_ok() {
                    let _ = reader
                        .get_mut()
                        .write_all(format!("remote-pong:{line}").as_bytes())
                        .await;
                }
            });
        }
    });

    let forward = within(
        "forward",
        session.forward_remote("127.0.0.1", REMOTE_PORT, "127.0.0.1", local_port),
    )
    .await
    .expect("forward");
    assert_eq!(forward.kind(), ForwardKind::Remote);
    assert_eq!(forward.remote_port(), Some(u32::from(REMOTE_PORT)));

    // Ask the server itself to connect to its end of the forward.
    let ask =
        "timeout 10 bash -c 'exec 3<>/dev/tcp/127.0.0.1/17101; echo remote-ping >&3; head -n1 <&3'";
    let answer = within_for("remote forward answers", UP, async {
        loop {
            let output = session.exec(ask).await.expect("exec");
            let text = output.stdout_text();
            if text.contains("remote-pong") {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    })
    .await;
    assert_eq!(answer, "remote-pong:remote-ping\n");

    // After stop the server no longer listens.
    within("stop", forward.stop()).await.expect("stop");
    let output = within("exec", session.exec(ask)).await.expect("exec");
    assert!(
        !output.stdout_text().contains("remote-pong"),
        "forward still answers after stop: {output:?}"
    );
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn forward_to_a_busy_local_port_fails_fast() {
    let e2e = E2e::new();
    let session = connect(&e2e).await;
    let blocker = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = blocker.local_addr().unwrap().port();
    let result = within(
        "forward",
        session.forward_local("127.0.0.1", port, "127.0.0.1", ECHO_PORT),
    )
    .await;
    assert!(
        matches!(result, Err(Error::Forward(_))),
        "{:?}",
        result.err()
    );
    drop(blocker);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn all_three_forwards_run_on_one_connection() {
    let e2e = E2e::new();
    let session = connect(&e2e).await;
    let local = session
        .forward_local("127.0.0.1", 0, "127.0.0.1", ECHO_PORT)
        .await
        .expect("-L");
    let dynamic = session.forward_dynamic("127.0.0.1", 0).await.expect("-D");
    let remote = session
        .forward_remote("127.0.0.1", 0, "127.0.0.1", ECHO_PORT)
        .await
        .expect("-R on a server-chosen port");
    assert!(remote.remote_port().unwrap() > 0);

    let mut stream = TcpStream::connect(("127.0.0.1", local.local_addr().unwrap().port()))
        .await
        .unwrap();
    echo_roundtrip(&mut stream, "still works").await;
    dynamic.stop().await.expect("stop -D");
    remote.stop().await.expect("stop -R");
    local.stop().await.expect("stop -L");
}
