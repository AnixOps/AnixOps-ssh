#![cfg(unix)]

use easyssh_core::{OpenSsh, SshInvocation};
use easyssh_e2e::E2e;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Mirrors `src-tauri/src/sessions.rs`: the core's invocation runs on a PTY
/// via portable-pty, input goes to the writer and output comes from a reader.
#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn interactive_shell_on_a_pty() {
    let e2e = E2e::new();
    let invocation = SshInvocation::for_connection(&OpenSsh, &e2e.alias("e2e-target"))
        .expect("build ssh invocation");
    let mut command = CommandBuilder::new(&invocation.executable);
    command.args(&invocation.args);
    command.env("TERM", "xterm-256color");

    let pair = native_pty_system()
        .openpty(size(100, 30))
        .expect("open pty");
    let mut child = pair.slave.spawn_command(command).expect("spawn ssh");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");

    let (sender, output) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        while let Ok(read) = reader.read(&mut buffer) {
            if read == 0 || sender.send(buffer[..read].to_vec()).is_err() {
                break;
            }
        }
    });

    // The typed command is echoed as `e2e-marker-$((1+1))`; only the shell's
    // evaluation produces `e2e-marker-2`.
    writer
        .write_all(b"echo e2e-marker-$((1+1))\r")
        .expect("write command");
    writer.flush().expect("flush");

    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    while !seen.contains("e2e-marker-2") {
        let left = deadline.saturating_duration_since(Instant::now());
        match output.recv_timeout(left) {
            Ok(bytes) => seen.push_str(&String::from_utf8_lossy(&bytes)),
            Err(_) => panic!("marker not seen in the terminal output:\n{seen}"),
        }
    }

    pair.master
        .resize(size(120, 40))
        .expect("resize the pty once");

    writer.write_all(b"exit\r").expect("write exit");
    writer.flush().expect("flush");

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll ssh") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("ssh did not end after `exit`");
        }
        // Keep draining so a full buffer cannot block the child.
        while output.try_recv().is_ok() {}
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "ssh exited with {status:?}");
}
