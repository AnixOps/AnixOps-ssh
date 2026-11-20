#![cfg(unix)]

use easyssh_core::sftp::{join_remote, spawn_transfer};
use easyssh_core::{
    OpenSsh, RemoteFileKind, ScpInvocation, SftpSession, Transfer, TransferDirection,
};
use easyssh_e2e::{finish, sample_bytes, text, unique_name, with_timeout, E2e, CMD_TIMEOUT};

fn connect(e2e: &E2e, alias: &str) -> SftpSession {
    let connection = e2e.alias(alias);
    with_timeout(CMD_TIMEOUT, "SftpSession::connect", move || {
        SftpSession::connect(&OpenSsh, &connection, &[])
    })
    .expect("sftp connect")
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn browse_and_manage_files() {
    let e2e = E2e::new();
    let mut session = connect(&e2e, "e2e-target");
    assert_eq!(session.home(), "/home/keyuser");
    let home = session.home().to_owned();

    let dir = join_remote(&home, &unique_name("e2e-dir"));
    session.mkdir(&dir).expect("mkdir");
    let listing = session.list(&home).expect("list home");
    let name = dir.rsplit('/').next().unwrap();
    let entry = listing
        .iter()
        .find(|file| file.name == name)
        .unwrap_or_else(|| panic!("{name} missing from {listing:?}"));
    assert_eq!(entry.kind, RemoteFileKind::Directory);

    // Upload a file with a space in its name, list it, rename it, remove it.
    let local = e2e.dir.path().join("payload.bin");
    let data = sample_bytes(4096);
    std::fs::write(&local, &data).unwrap();
    let remote = join_remote(&dir, "payload one.bin");
    upload(&e2e, "e2e-target", &local, &remote);

    let files = session.list(&dir).expect("list dir");
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].name, "payload one.bin");
    assert_eq!(files[0].kind, RemoteFileKind::File);
    assert_eq!(files[0].size, data.len() as u64);

    let renamed = join_remote(&dir, "payload two.bin");
    session.rename(&remote, &renamed).expect("rename");
    let files = session.list(&dir).expect("list after rename");
    assert_eq!(files.len(), 1, "{files:?}");
    assert_eq!(files[0].name, "payload two.bin");

    session.remove(&renamed, false).expect("rm");
    assert!(session.list(&dir).expect("list empty").is_empty());
    session.remove(&dir, true).expect("rmdir");
    let listing = session.list(&home).expect("list home again");
    assert!(listing.iter().all(|file| file.name != name));
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn missing_directory_and_files_are_not_listed_as_folders() {
    // `ls` of a missing directory reports it on standard error, which can
    // arrive after the reply; the listing waits for it instead of returning
    // an empty folder. `ls` of a file lists the file itself.
    let e2e = E2e::new();
    let mut session = connect(&e2e, "e2e-target");
    let home = session.home().to_owned();
    let missing = join_remote(&home, &unique_name("e2e-missing"));
    let error = session.list(&missing).expect_err("missing directory");
    assert!(error.to_string().contains("not found"), "{error}");
    let file = join_remote(&home, ".profile");
    if session
        .list(&home)
        .expect("list home")
        .iter()
        .any(|f| f.name == ".profile")
    {
        let error = session.list(&file).expect_err("a file");
        assert!(error.to_string().contains("not a folder"), "{error}");
    }
    // The session still works after both errors.
    session.list(&home).expect("list home afterwards");
}

fn upload(e2e: &E2e, alias: &str, local: &std::path::Path, remote: &str) {
    let connection = e2e.alias(alias);
    let child =
        spawn_transfer(&OpenSsh, &connection, true, local, remote, &[]).expect("spawn upload");
    let output = finish(child, CMD_TIMEOUT);
    assert!(output.status.success(), "upload: {}", text(&output.stderr));
}

fn download(e2e: &E2e, alias: &str, remote: &str, local: &std::path::Path) {
    let connection = e2e.alias(alias);
    let child =
        spawn_transfer(&OpenSsh, &connection, false, local, remote, &[]).expect("spawn download");
    let output = finish(child, CMD_TIMEOUT);
    assert!(
        output.status.success(),
        "download: {}",
        text(&output.stderr)
    );
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn upload_and_download_through_sftp_transfer() {
    let e2e = E2e::new();
    let mut session = connect(&e2e, "e2e-target");
    let data = sample_bytes(300_000);
    let local = e2e.dir.path().join("up.bin");
    std::fs::write(&local, &data).unwrap();
    let remote = join_remote(session.home(), &unique_name("e2e-transfer.bin"));

    upload(&e2e, "e2e-target", &local, &remote);
    let back = e2e.dir.path().join("down.bin");
    download(&e2e, "e2e-target", &remote, &back);
    assert_eq!(std::fs::read(&back).unwrap(), data);

    session.remove(&remote, false).expect("cleanup");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn download_of_a_missing_file_fails_with_a_reason() {
    let e2e = E2e::new();
    let connection = e2e.alias("e2e-target");
    let local = e2e.dir.path().join("never.bin");
    let child = spawn_transfer(
        &OpenSsh,
        &connection,
        false,
        &local,
        "/home/keyuser/e2e-does-not-exist",
        &[],
    )
    .expect("spawn");
    let output = finish(child, CMD_TIMEOUT);
    assert!(!output.status.success());
    assert!(!local.exists());
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn upload_and_download_through_scp_invocation() {
    let e2e = E2e::new();
    let connection = e2e.alias("e2e-target");
    let data = sample_bytes(100_000);
    let local = e2e.dir.path().join("scp-up.bin");
    std::fs::write(&local, &data).unwrap();
    let remote = format!("/home/keyuser/{}", unique_name("e2e-scp.bin"));

    let up = Transfer::new(TransferDirection::Upload, local, remote.clone(), false);
    let invocation = ScpInvocation::build(&OpenSsh, &connection, &up).expect("build scp");
    let output = finish(invocation.spawn().expect("spawn scp"), CMD_TIMEOUT);
    assert!(output.status.success(), "scp up: {}", text(&output.stderr));

    let back = e2e.dir.path().join("scp-down.bin");
    let down = Transfer::new(
        TransferDirection::Download,
        back.clone(),
        remote.clone(),
        false,
    );
    let invocation = ScpInvocation::build(&OpenSsh, &connection, &down).expect("build scp");
    let output = finish(invocation.spawn().expect("spawn scp"), CMD_TIMEOUT);
    assert!(
        output.status.success(),
        "scp down: {}",
        text(&output.stderr)
    );
    assert_eq!(std::fs::read(&back).unwrap(), data);

    let mut session = connect(&e2e, "e2e-target");
    session.remove(&remote, false).expect("cleanup");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn transfers_keep_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let e2e = E2e::new();
    let mut session = connect(&e2e, "e2e-target");
    let local = e2e.dir.path().join("run.sh");
    std::fs::write(&local, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o750)).unwrap();
    let remote = join_remote(session.home(), &unique_name("e2e-mode.sh"));
    upload(&e2e, "e2e-target", &local, &remote);
    let name = remote.rsplit('/').next().unwrap();
    let home = session.home().to_owned();
    let listed = session.list(&home).expect("list");
    let file = listed.iter().find(|f| f.name == name).expect("uploaded");
    assert_eq!(file.permissions, "-rwxr-x---");

    let back = e2e.dir.path().join("back.sh");
    download(&e2e, "e2e-target", &remote, &back);
    let mode = std::fs::metadata(&back).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o750);
    session.remove(&remote, false).expect("cleanup");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn removing_a_folder_removes_everything_in_it_but_not_what_links_point_to() {
    let e2e = E2e::new();
    let connection = e2e.alias("e2e-target");
    let dir = format!("/home/keyuser/{}", unique_name("e2e-tree"));
    let outside = format!("/home/keyuser/{}", unique_name("e2e-outside"));
    let setup: &'static str = Box::leak(
        format!(
            "set -e; mkdir -p '{dir}/a/b' '{outside}'; printf keep > '{outside}/keep.txt'; \
             printf x > '{dir}/.hidden'; printf y > '{dir}/a/b/deep.txt'; \
             mkdir '{dir}/a/empty'; ln -s '{outside}' '{dir}/a/link'; \
             ln -s '{outside}/keep.txt' '{dir}/file-link'"
        )
        .into_boxed_str(),
    );
    let output = e2e.exec(&connection, setup);
    assert!(output.status.success(), "setup: {}", text(&output.stderr));

    let mut session = connect(&e2e, "e2e-target");
    session.remove(&dir, true).expect("remove the folder");
    assert!(session.list(&dir).is_err());
    let kept = session
        .list(&outside)
        .expect("the link target is still there");
    let names: Vec<_> = kept.iter().map(|file| file.name.as_str()).collect();
    assert_eq!(names, ["keep.txt"]);

    session.remove(&outside, true).expect("cleanup");
}

#[test]
#[ignore = "needs the e2e OpenSSH containers; see e2e/README.md"]
fn rename_moves_between_folders_and_chmod_is_recursive() {
    let e2e = E2e::new();
    let mut session = connect(&e2e, "e2e-target");
    let home = session.home().to_owned();
    let dir = join_remote(&home, &unique_name("e2e-move"));
    session.mkdir(&dir).expect("mkdir");
    let inner = join_remote(&dir, "inner");
    session.mkdir(&inner).expect("mkdir inner");

    let local = e2e.dir.path().join("move.bin");
    std::fs::write(&local, b"a").unwrap();
    let remote = join_remote(&dir, "a.bin");
    upload(&e2e, "e2e-target", &local, &remote);

    let moved = join_remote(&inner, "b.bin");
    session.rename(&remote, &moved).expect("move");
    assert!(session
        .list(&dir)
        .unwrap()
        .iter()
        .all(|f| f.name != "a.bin"));
    assert_eq!(session.list(&inner).unwrap()[0].name, "b.bin");

    session.chmod(&moved, 0o640, false).expect("chmod file");
    assert_eq!(session.list(&inner).unwrap()[0].permissions, "-rw-r-----");
    session.chmod(&dir, 0o750, true).expect("chmod tree");
    assert_eq!(session.list(&inner).unwrap()[0].permissions, "-rwxr-x---");
    let top = session.list(&home).unwrap();
    let name = dir.rsplit('/').next().unwrap();
    assert_eq!(
        top.iter().find(|f| f.name == name).unwrap().permissions,
        "drwxr-x---"
    );
    session.remove(&dir, true).expect("cleanup");
}
