#![cfg(unix)]

use easyssh_e2e::russh_support::{within, within_for, Scripted};
use easyssh_e2e::E2e;
use easyssh_ssh::{ExitStatus, Session, ShellHandle, ShellOptions};
use std::time::Duration;

/// Reads terminal output until it contains `needle`; returns everything read.
async fn read_until(shell: &mut ShellHandle, needle: &str) -> String {
    within_for(
        &format!("waiting for {needle:?}"),
        Duration::from_secs(30),
        async {
            let mut seen = String::new();
            while !seen.contains(needle) {
                let chunk = shell
                    .read()
                    .await
                    .unwrap_or_else(|| panic!("shell ended before {needle:?}; saw {seen:?}"));
                seen.push_str(&String::from_utf8_lossy(&chunk));
            }
            seen
        },
    )
    .await
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn interactive_shell_on_a_pty() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    let mut shell = within(
        "shell",
        session.shell(ShellOptions {
            term: "xterm".into(),
            cols: 100,
            rows: 30,
            ..ShellOptions::default()
        }),
    )
    .await
    .expect("shell");

    // The marker only appears once the shell has evaluated the arithmetic;
    // the echoed command line contains `$((20+22))`, not `42`.
    shell
        .write(b"echo e2e-$((20+22))-marker\n")
        .await
        .expect("write");
    read_until(&mut shell, "e2e-42-marker").await;

    // The initial size reached the PTY, and a resize changes it.
    shell.write(b"stty size\n").await.expect("write");
    read_until(&mut shell, "30 100").await;
    shell.resize(132, 43).await.expect("resize");
    // Give the server a moment to apply the window change, then ask again;
    // retry until it shows, with an overall limit.
    within_for("resize to show", Duration::from_secs(30), async {
        loop {
            shell.write(b"stty size\n").await.expect("write");
            let seen = read_until_either(&mut shell, "43 132", "30 100").await;
            if seen {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;

    shell.write(b"exit\n").await.expect("write");
    let status = within("shell exit", shell.wait_exit()).await;
    assert_eq!(status, ExitStatus::Code(0));
}

/// True if `wanted` shows up before `stale` does (after the next output).
async fn read_until_either(shell: &mut ShellHandle, wanted: &str, stale: &str) -> bool {
    let mut seen = String::new();
    loop {
        let chunk = shell.read().await.expect("shell ended");
        seen.push_str(&String::from_utf8_lossy(&chunk));
        if seen.contains(wanted) {
            return true;
        }
        if seen.contains(stale) {
            return false;
        }
    }
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn shell_and_exec_run_on_one_connection() {
    let e2e = E2e::new();
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    let mut shell = within("shell", session.shell(ShellOptions::default()))
        .await
        .expect("shell");
    let output = within("exec beside shell", session.exec("echo beside"))
        .await
        .expect("exec");
    assert_eq!(output.stdout_text(), "beside\n");
    shell.write(b"echo still-$((1+1))\n").await.expect("write");
    read_until(&mut shell, "still-2").await;
    shell.close().await.expect("close");
}
