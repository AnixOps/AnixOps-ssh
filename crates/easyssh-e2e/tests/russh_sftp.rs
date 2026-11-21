#![cfg(unix)]

use easyssh_e2e::russh_support::{within, Scripted};
use easyssh_e2e::{sample_bytes, unique_name, E2e};
use easyssh_ssh::{EntryKind, Error, ExitStatus, Session, Sftp};

async fn open(e2e: &E2e) -> (Session, Sftp) {
    let session = within(
        "connect",
        Session::connect(e2e.russh_target(), &Scripted::trusting()),
    )
    .await
    .expect("connect");
    let sftp = within("sftp", session.sftp()).await.expect("sftp");
    (session, sftp)
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn browse_and_manage_files() {
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    within("whole flow", async {
        let home = sftp.home().await.expect("home");
        assert_eq!(home, "/home/keyuser");
        assert_eq!(sftp.realpath("/home/keyuser/.").await.unwrap(), home);

        let dir = format!("{home}/{}", unique_name("e2e-russh"));
        sftp.mkdir(&dir).await.expect("mkdir");
        let stat = sftp.stat(&dir).await.expect("stat dir");
        assert_eq!(stat.kind, EntryKind::Dir);

        let file = format!("{dir}/hello.txt");
        sftp.write_file(&file, b"hello sftp\n")
            .await
            .expect("write");
        let stat = sftp.stat(&file).await.expect("stat file");
        assert_eq!(stat.kind, EntryKind::File);
        assert_eq!(stat.size, 11);
        assert_eq!(sftp.read_file(&file).await.unwrap(), b"hello sftp\n");

        let listing = sftp.list(&dir).await.expect("list");
        let names: Vec<&str> = listing.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["hello.txt"]);

        let renamed = format!("{dir}/renamed.txt");
        sftp.rename(&file, &renamed).await.expect("rename");
        assert!(!sftp.exists(&file).await.unwrap());
        assert!(sftp.exists(&renamed).await.unwrap());
        let names: Vec<String> = sftp
            .list(&dir)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, ["renamed.txt"]);

        sftp.remove_file(&renamed).await.expect("rm");
        sftp.remove_dir(&dir).await.expect("rmdir");
        assert!(!sftp.exists(&dir).await.unwrap());
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn missing_paths_are_reported_as_not_found() {
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    let missing = format!("/home/keyuser/{}", unique_name("missing"));
    let error = within("list", sftp.list(&missing)).await.unwrap_err();
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
    let error = within("stat", sftp.stat(&missing)).await.unwrap_err();
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
    let error = within("open", sftp.open_read(&missing)).await.err();
    assert!(matches!(error, Some(Error::NotFound(_))), "{error:?}");
    let error = within("rm", sftp.remove_file(&missing)).await.unwrap_err();
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn upload_and_download_300_kb_streams() {
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    let data = sample_bytes(300_000);
    let local_up = e2e.dir.path().join("up.bin");
    let local_down = e2e.dir.path().join("down.bin");
    std::fs::write(&local_up, &data).unwrap();
    let remote = format!("/home/keyuser/{}.bin", unique_name("e2e-russh"));

    within("transfer", async {
        let mut source = tokio::fs::File::open(&local_up).await.unwrap();
        let sent = sftp.upload(&remote, &mut source).await.expect("upload");
        assert_eq!(sent, 300_000);
        assert_eq!(sftp.stat(&remote).await.unwrap().size, 300_000);

        let mut sink = tokio::fs::File::create(&local_down).await.unwrap();
        let received = sftp.download(&remote, &mut sink).await.expect("download");
        assert_eq!(received, 300_000);
        drop(sink);

        sftp.remove_file(&remote).await.expect("rm");
    })
    .await;
    assert_eq!(std::fs::read(&local_down).unwrap(), data);
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn download_of_a_missing_file_fails_with_a_reason() {
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    let mut sink = Vec::new();
    let error = within(
        "download",
        sftp.download("/home/keyuser/nope-e2e", &mut sink),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
    assert!(sink.is_empty());
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn sftp_and_exec_share_the_connection() {
    let e2e = E2e::new();
    let (session, sftp) = open(&e2e).await;
    let remote = format!("/home/keyuser/{}.txt", unique_name("e2e-russh"));
    within("flow", async {
        sftp.write_file(&remote, b"seen by exec").await.unwrap();
        let output = session.exec(&format!("cat {remote}")).await.unwrap();
        assert_eq!(output.stdout_text(), "seen by exec");
        sftp.remove_file(&remote).await.unwrap();
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn removing_a_folder_removes_everything_in_it_but_not_what_links_point_to() {
    let e2e = E2e::new();
    let (session, sftp) = open(&e2e).await;
    let dir = format!("/home/keyuser/{}", unique_name("e2e-tree"));
    let outside = format!("/home/keyuser/{}", unique_name("e2e-outside"));
    within("tree", async {
        let made = session
            .exec(&format!(
                "set -e; mkdir -p '{dir}/a/b' '{outside}'; printf keep > '{outside}/keep.txt'; \
                 printf x > '{dir}/.hidden'; printf y > '{dir}/a/b/deep.txt'; \
                 mkdir '{dir}/a/empty'; ln -s '{outside}' '{dir}/a/link'; \
                 ln -s '{outside}/keep.txt' '{dir}/file-link'"
            ))
            .await
            .unwrap();
        assert_eq!(made.exit, ExitStatus::Code(0), "{:?}", made.stderr_text());

        sftp.remove_dir_all(&dir).await.expect("remove the folder");
        assert!(!sftp.exists(&dir).await.unwrap());
        assert_eq!(
            sftp.read_file(&format!("{outside}/keep.txt"))
                .await
                .unwrap(),
            b"keep"
        );

        // A file and a missing path.
        sftp.write_file(&format!("{outside}/one.txt"), b"1")
            .await
            .unwrap();
        sftp.remove_dir_all(&format!("{outside}/one.txt"))
            .await
            .unwrap();
        let error = sftp.remove_dir_all(&dir).await.unwrap_err();
        assert!(matches!(error, Error::NotFound(_)), "{error:?}");

        sftp.remove_dir_all(&outside).await.unwrap();
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn chmod_one_entry_and_a_whole_folder() {
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    within("whole flow", async {
        let home = sftp.home().await.expect("home");
        let dir = format!("{home}/{}", unique_name("e2e-chmod"));
        sftp.mkdir(&dir).await.expect("mkdir");
        let file = format!("{dir}/a.txt");
        sftp.write_file(&file, b"x").await.expect("write");
        let inner = format!("{dir}/inner");
        sftp.mkdir(&inner).await.expect("mkdir inner");
        let deep = format!("{inner}/b.txt");
        sftp.write_file(&deep, b"y").await.expect("write deep");

        sftp.set_stat(&file, Some(0o600), None)
            .await
            .expect("chmod one");
        assert_eq!(sftp.stat(&file).await.unwrap().permissions, Some(0o600));
        // The folder itself is not touched.
        assert_ne!(sftp.stat(&dir).await.unwrap().permissions, Some(0o600));

        sftp.set_permissions_all(&dir, 0o750)
            .await
            .expect("chmod all");
        for path in [&dir, &file, &inner, &deep] {
            assert_eq!(
                sftp.stat(path).await.unwrap().permissions,
                Some(0o750),
                "{path}"
            );
        }
        sftp.remove_dir_all(&dir).await.expect("clean up");
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn rename_moves_a_file_into_another_folder() {
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    within("whole flow", async {
        let home = sftp.home().await.expect("home");
        let dir = format!("{home}/{}", unique_name("e2e-russh-move"));
        sftp.mkdir(&dir).await.expect("mkdir");
        let inner = format!("{dir}/inner");
        sftp.mkdir(&inner).await.expect("mkdir inner");
        let file = format!("{dir}/a.txt");
        sftp.write_file(&file, b"a").await.expect("write");

        // A rename whose target is in another folder moves the file.
        let moved = format!("{inner}/b.txt");
        sftp.rename(&file, &moved).await.expect("move");
        assert!(!sftp.exists(&file).await.unwrap());
        assert_eq!(sftp.read_file(&moved).await.unwrap(), b"a");
        sftp.remove_dir_all(&dir).await.expect("cleanup");
    })
    .await;
}

// ---- resuming, odd names, many small files, SHA-256 -------------------------

fn sha_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn an_interrupted_upload_continues_at_the_offset_and_ends_byte_identical() {
    use tokio::io::AsyncWriteExt;
    let e2e = E2e::new();
    let (session, sftp) = open(&e2e).await;
    let data = sample_bytes(1_500_000);
    let remote = format!("/home/keyuser/{}.part", unique_name("e2e-resume-up"));
    within("resume upload", async {
        // The first try gets 400 000 bytes there and stops.
        let mut file = sftp.create(&remote).await.expect("create");
        file.write_all(&data[..400_000]).await.expect("write");
        file.shutdown().await.expect("close");
        drop(file);
        let there = sftp.stat(&remote).await.expect("stat").size;
        assert_eq!(there, 400_000);
        // The second goes on from there without truncating.
        let mut source = &data[there as usize..];
        let mut file = sftp.open_write_at(&remote, there).await.expect("reopen");
        tokio::io::copy(&mut source, &mut file).await.expect("copy");
        file.shutdown().await.expect("close");
        drop(file);
        assert_eq!(sftp.stat(&remote).await.unwrap().size, 1_500_000);
        let back = sftp.read_file(&remote).await.expect("read");
        assert_eq!(sha_hex(&back), sha_hex(&data), "content SHA-256");
        // The server's own sha256sum agrees.
        let remote_sha = session.remote_sha256(&remote).await.expect("sha256sum");
        assert_eq!(remote_sha.as_deref(), Some(sha_hex(&data).as_str()));
        // Writing at an offset of a file that is not there fails.
        let missing = format!("{remote}.nope");
        assert!(sftp.open_write_at(&missing, 10).await.is_err());
        sftp.remove_file(&remote).await.expect("rm");
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn an_interrupted_download_continues_at_the_offset_and_ends_byte_identical() {
    use tokio::io::AsyncWriteExt;
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    let data = sample_bytes(1_500_000);
    let remote = format!("/home/keyuser/{}.bin", unique_name("e2e-resume-down"));
    let local = e2e.dir.path().join("partial.bin");
    within("resume download", async {
        sftp.write_file(&remote, &data).await.expect("write");
        // The first try keeps 700 000 bytes.
        std::fs::write(&local, &data[..700_000]).unwrap();
        let mut file = sftp.open_read_at(&remote, 700_000).await.expect("open");
        let mut sink = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&local)
            .await
            .unwrap();
        let copied = tokio::io::copy(&mut file, &mut sink).await.expect("copy");
        sink.flush().await.unwrap();
        assert_eq!(copied, 800_000);
        sftp.remove_file(&remote).await.expect("rm");
    })
    .await;
    assert_eq!(sha_hex(&std::fs::read(&local).unwrap()), sha_hex(&data));
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn an_empty_file_goes_there_and_back_and_has_the_empty_hash() {
    let e2e = E2e::new();
    let (session, sftp) = open(&e2e).await;
    let remote = format!("/home/keyuser/{}.empty", unique_name("e2e-empty"));
    within("empty file", async {
        sftp.write_file(&remote, b"").await.expect("write");
        assert_eq!(sftp.stat(&remote).await.unwrap().size, 0);
        assert!(sftp.read_file(&remote).await.unwrap().is_empty());
        // Resuming at 0 of a file that is 0 long is fine.
        let mut file = sftp.open_write_at(&remote, 0).await.expect("reopen");
        tokio::io::AsyncWriteExt::shutdown(&mut file).await.unwrap();
        drop(file);
        assert_eq!(
            session.remote_sha256(&remote).await.unwrap().as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        sftp.remove_file(&remote).await.expect("rm");
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn names_with_chinese_spaces_and_quotes_survive_and_are_hashed_safely() {
    let e2e = E2e::new();
    let (session, sftp) = open(&e2e).await;
    let dir = format!("/home/keyuser/{}", unique_name("e2e-names"));
    within("odd names", async {
        sftp.mkdir(&dir).await.expect("mkdir");
        let names = [
            "季度 报告 终稿.txt",
            "it's a \"test\" $(id) `id`; rm -x.txt",
            "-leading-dash.txt",
            "tab\there.txt",
        ];
        for (index, name) in names.iter().enumerate() {
            let remote = format!("{dir}/{name}");
            let data = sample_bytes(10_000 + index);
            sftp.write_file(&remote, &data).await.expect("write");
            assert_eq!(sftp.read_file(&remote).await.unwrap(), data, "{name}");
            let listed = sftp.list(&dir).await.unwrap();
            assert!(listed.iter().any(|e| e.name == *name), "{name} is listed");
            assert_eq!(
                session.remote_sha256(&remote).await.expect(name).as_deref(),
                Some(sha_hex(&data).as_str()),
                "{name}"
            );
        }
        // Nothing the names contained ran as a command.
        let probe = session.exec(&format!("ls '{dir}' | wc -l")).await.unwrap();
        assert_eq!(probe.stdout_text().trim(), names.len().to_string());
        sftp.remove_dir_all(&dir).await.expect("cleanup");
    })
    .await;
}

#[tokio::test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
async fn a_thousand_small_files_round_trip_with_the_same_content() {
    const COUNT: usize = 1000;
    let e2e = E2e::new();
    let (_session, sftp) = open(&e2e).await;
    let dir = format!("/home/keyuser/{}", unique_name("e2e-small"));
    let file_data = |index: usize| -> Vec<u8> {
        (0..4096usize)
            .map(|i| (i * 7 + index * 13 + i / 5) as u8)
            .collect()
    };
    sftp.mkdir(&dir).await.expect("mkdir");
    let started = std::time::Instant::now();
    within("upload 1000", async {
        for index in 0..COUNT {
            sftp.write_file(&format!("{dir}/f{index:04}.dat"), &file_data(index))
                .await
                .expect("write");
        }
    })
    .await;
    let up = started.elapsed();
    let listed = sftp.list(&dir).await.expect("list");
    assert_eq!(listed.len(), COUNT, "every file arrived");
    assert!(listed.iter().all(|e| e.stat.size == 4096));
    let started = std::time::Instant::now();
    let mut wrong = Vec::new();
    within("download 1000", async {
        for index in 0..COUNT {
            let back = sftp
                .read_file(&format!("{dir}/f{index:04}.dat"))
                .await
                .expect("read");
            if sha_hex(&back) != sha_hex(&file_data(index)) {
                wrong.push(index);
            }
        }
    })
    .await;
    let down = started.elapsed();
    println!(
        "small files: {COUNT} x 4 KiB up {:.0} files/s ({:.1} s), down {:.0} files/s ({:.1} s)",
        COUNT as f64 / up.as_secs_f64(),
        up.as_secs_f64(),
        COUNT as f64 / down.as_secs_f64(),
        down.as_secs_f64()
    );
    assert!(
        wrong.is_empty(),
        "files with a different SHA-256: {wrong:?}"
    );
    sftp.remove_dir_all(&dir).await.expect("cleanup");
}
