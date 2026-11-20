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
