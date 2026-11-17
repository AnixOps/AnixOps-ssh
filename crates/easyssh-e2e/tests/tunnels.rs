#![cfg(unix)]

use easyssh_core::tunnel::spawn_tunnel;
use easyssh_core::{OpenSsh, Tunnel, TunnelKind};
use easyssh_e2e::{
    echo_roundtrip, free_port, text, wait_for_port, wait_for_port_closed, E2e, Reaper, ECHO_PORT,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

const REMOTE_PORT: u16 = 17001;
const UP: Duration = Duration::from_secs(30);

fn tunnel(kind: TunnelKind, bind_port: u16, target_port: Option<u16>) -> Tunnel {
    Tunnel {
        id: "e2e".into(),
        name: "e2e".into(),
        host_id: "e2e".into(),
        kind,
        bind_address: Some("127.0.0.1".into()),
        bind_port,
        target_host: (kind != TunnelKind::Dynamic).then(|| "127.0.0.1".into()),
        target_port,
    }
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn local_forward_reaches_the_echo_service() {
    let e2e = E2e::new();
    let port = free_port();
    let spec = tunnel(TunnelKind::Local, port, Some(ECHO_PORT));
    let child = spawn_tunnel(&OpenSsh, &e2e.alias("e2e-target"), &spec, &[]).expect("spawn");
    let mut tunnel = Reaper(child);
    wait_for_port(port, &mut tunnel.0, UP);

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    echo_roundtrip(&mut stream, "local-forward");
    echo_roundtrip(&mut stream, "second line");
    drop(stream);

    tunnel.stop();
    wait_for_port_closed(port, Duration::from_secs(10));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn dynamic_forward_speaks_socks5() {
    let e2e = E2e::new();
    let port = free_port();
    let spec = tunnel(TunnelKind::Dynamic, port, None);
    let child = spawn_tunnel(&OpenSsh, &e2e.alias("e2e-target"), &spec, &[]).expect("spawn");
    let mut tunnel = Reaper(child);
    wait_for_port(port, &mut tunnel.0, UP);

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // Greeting: version 5, one method, "no authentication".
    stream.write_all(&[5, 1, 0]).unwrap();
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).expect("method reply");
    assert_eq!(reply, [5, 0]);
    // CONNECT 127.0.0.1:ECHO_PORT, as seen from the server.
    let mut request = vec![5, 1, 0, 1, 127, 0, 0, 1];
    request.extend_from_slice(&ECHO_PORT.to_be_bytes());
    stream.write_all(&request).unwrap();
    let mut reply = [0u8; 10];
    stream.read_exact(&mut reply).expect("connect reply");
    assert_eq!(reply[..2], [5, 0], "SOCKS connect refused: {reply:?}");
    echo_roundtrip(&mut stream, "through-socks");
    drop(stream);

    tunnel.stop();
    wait_for_port_closed(port, Duration::from_secs(10));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn remote_forward_reaches_a_local_listener() {
    let e2e = E2e::new();
    // A local service that the server should be able to reach via -R.
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let local_port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let mut stream = stream;
                let _ = write!(stream, "remote-pong:{}", line);
            }
        }
    });

    let spec = tunnel(TunnelKind::Remote, REMOTE_PORT, Some(local_port));
    let child = spawn_tunnel(&OpenSsh, &e2e.alias("e2e-target"), &spec, &[]).expect("spawn");
    let mut tunnel = Reaper(child);

    // Ask the server itself to connect to its end of the forward.
    let deadline = Instant::now() + UP;
    let answer = loop {
        if let Some(status) = tunnel.0.try_wait().unwrap() {
            let mut stderr = String::new();
            if let Some(mut pipe) = tunnel.0.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            panic!("tunnel exited early ({status}): {}", stderr.trim());
        }
        let output = e2e.exec(
            &e2e.alias("e2e-target"),
            "timeout 10 bash -c 'exec 3<>/dev/tcp/127.0.0.1/17001; echo remote-ping >&3; head -n1 <&3'",
        );
        let answer = text(&output.stdout);
        if output.status.success() && answer.contains("remote-pong") {
            break answer;
        }
        assert!(
            Instant::now() < deadline,
            "forward never answered; last output {answer:?} {}",
            text(&output.stderr)
        );
        std::thread::sleep(Duration::from_millis(500));
    };
    assert_eq!(answer, "remote-pong:remote-ping\n");

    tunnel.stop();
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn forward_to_a_busy_local_port_fails_fast() {
    // ExitOnForwardFailure=yes: a tunnel that cannot bind must exit, not idle.
    let e2e = E2e::new();
    let blocker = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = blocker.local_addr().unwrap().port();
    let spec = tunnel(TunnelKind::Local, port, Some(ECHO_PORT));
    let child = spawn_tunnel(&OpenSsh, &e2e.alias("e2e-target"), &spec, &[]).expect("spawn");
    let output = easyssh_e2e::finish(child, Duration::from_secs(60));
    assert!(!output.status.success());
    assert!(!text(&output.stderr).trim().is_empty());
    drop(blocker);
}
