//! Signed payload splitting compared with what Git hands its signature programs.
//!
//! Every Git call isolates configuration, identity and dates, removes `SSH_AUTH_SOCK`, and points
//! `HOME` and `GNUPGHOME` into the test's temporary directory. Keys are generated per test.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use girt::signing::SignatureFormat;
use girt::{Commit, ObjectFormat, Tag};
use rstest::rstest;

fn command(program: &str, directory: &Path) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env_remove("SSH_AUTH_SOCK")
        .env("HOME", directory)
        .env("GNUPGHOME", directory.join("gnupg"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000");
    command
}

fn run(program: &str, directory: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut child = command(program, directory)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("{program} required: {error}"));
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}

fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let output = run("git", directory, args, input);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git_line(directory: &Path, args: &[&str], input: &[u8]) -> String {
    String::from_utf8(git(directory, args, input))
        .unwrap()
        .trim()
        .to_owned()
}

fn init(root: &Path, format: ObjectFormat) -> std::path::PathBuf {
    let repo = root.join("repo");
    git(
        root,
        &["init", "-q", &format!("--object-format={format}"), "repo"],
        b"",
    );
    repo
}

fn has_program(program: &str, version_arg: &str) -> bool {
    Command::new(program)
        .arg(version_arg)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

/// What Git passed to its signature program for one verification, if it ran it.
#[cfg(unix)]
struct Capture {
    payload: Vec<u8>,
    signature: Vec<u8>,
}

/// Runs `git verify-<kind> <id>` with every signature program replaced by a recorder.
#[cfg(unix)]
fn capture(root: &Path, repo: &Path, kind: &str, id: &str) -> Option<Capture> {
    use std::os::unix::fs::PermissionsExt;

    let out = root.join("capture");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir(&out).unwrap();
    let script = root.join("record.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n\
             verify=\n\
             for a; do\n\
               case \"$prev\" in -s|--verify) cp \"$a\" '{out}/signature';; esac\n\
               case \"$a\" in --verify|verify|check-novalidate) verify=1;; esac\n\
               prev=$a\n\
             done\n\
             if [ -n \"$verify\" ]; then cat > '{out}/payload'; fi\n\
             exit 1\n",
            out = out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let program = script.to_str().unwrap();
    let allowed = root.join("allowed-signers");
    std::fs::write(&allowed, "").unwrap();
    let output = run(
        "git",
        repo,
        &[
            "-c",
            &format!("gpg.program={program}"),
            "-c",
            &format!("gpg.ssh.program={program}"),
            "-c",
            &format!("gpg.x509.program={program}"),
            "-c",
            &format!("gpg.ssh.allowedSignersFile={}", allowed.display()),
            &format!("verify-{kind}"),
            id,
        ],
        b"",
    );
    assert!(!output.status.success());
    let payload = std::fs::read(out.join("payload")).ok()?;
    Some(Capture {
        payload,
        signature: std::fs::read(out.join("signature")).unwrap(),
    })
}

const COMMIT_CASES: [&str; 7] = [
    "gpgsig -----BEGIN PGP SIGNATURE-----\n A\n -----END PGP SIGNATURE-----\n\nmsg\n",
    "gpgsig-sha256 -----BEGIN SSH SIGNATURE-----\n A\n\nmsg\n",
    "gpgsig-sha256 other\ngpgsig -----BEGIN PGP SIGNATURE-----\n A\nx-other y\n\nmsg\n",
    "gpgsig -----BEGIN PGP SIGNATURE-----\n one\ngpgsig -----BEGIN PGP SIGNATURE-----\n two\n\nmsg\n",
    "gpgsig -----BEGIN SIGNED MESSAGE-----\n \n  indented\ngpgsig-sha256 -----BEGIN SIGNED MESSAGE-----\n \n  indented\n\nmsg\n",
    "x-unsigned value\n\nmsg\n",
    "gpgsig -----BEGIN PGP SIGNATURE-----\n one\ngpgsig-sha256 -----BEGIN PGP SIGNATURE-----\n A\ngpgsig -----BEGIN PGP SIGNATURE-----\n A",
];

const TAG_CASES: [&str; 11] = [
    "\nhi\n-----BEGIN SSH SIGNATURE-----\nA\n-----END SSH SIGNATURE-----\n",
    "\nhi\n-----BEGIN PGP SIGNATURE-----\nB\n-----END PGP SIGNATURE-----\nmid\n-----BEGIN SSH SIGNATURE-----\nA\n-----END SSH SIGNATURE-----",
    "\nhi x-----BEGIN SSH SIGNATURE-----\nA\n",
    "\nhi\n-----BEGIN PGP MESSAGE-----\nA\n",
    "\nhi\n-----BEGIN PGP SIGNATURE-----  x\nA\n",
    "\nhi\n-----BEGIN SSH SIGNATUREX-----\nA\n",
    "gpgsig X\n Y\ngpgsig-sha256 Q\n\nmsg\ngpgsig body\n-----BEGIN SIGNED MESSAGE-----\nA\n",
    "-----BEGIN PGP SIGNATURE-----\nA\n",
    "gpgsig A\ngpgsig B\ngpgsig C\n\nmsg\n-----BEGIN PGP SIGNATURE-----\nA\n",
    "gpgsig A\n a\ngpgsig-sha256 B\n b\nx-mid m\ngpgsig-sha256 C\n\nmsg\n-----BEGIN PGP SIGNATURE-----\nA\n",
    "x-pre p\ngpgsig X\n Y\n\nmsg\n-----BEGIN SSH SIGNATURE-----\nA\n",
];

#[cfg(unix)]
#[rstest]
fn commit_payload_matches_git_verify_input(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(0, 1, 2, 3, 4, 5, 6)] case: usize,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = init(root.path(), format);
    let tree = git_line(&repo, &["mktree"], b"");
    let bytes = format!(
        "tree {tree}\nauthor A <a@example.com> 1700000000 +0000\ncommitter A <a@example.com> 1700000000 +0000\n{}",
        COMMIT_CASES[case]
    );
    let id = git_line(
        &repo,
        &["hash-object", "-t", "commit", "-w", "--literally", "--stdin"],
        bytes.as_bytes(),
    );
    let commit = Commit::parse(format, bytes.as_bytes()).unwrap();
    let signed = commit.signed_payload();
    match capture(root.path(), &repo, "commit", &id) {
        Some(captured) => {
            let signed = signed.expect("Git found a signature");
            assert!(signed.format().is_some());
            assert_eq!(signed.payload(), captured.payload);
            assert_eq!(signed.signature(), captured.signature);
        }
        None => assert!(signed.is_none_or(|signed| signed.format().is_none())),
    }
}

#[cfg(unix)]
#[rstest]
fn tag_payload_matches_git_verify_input(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10)] case: usize,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = init(root.path(), format);
    let tree = git_line(&repo, &["mktree"], b"");
    let bytes = format!(
        "object {tree}\ntype tree\ntag t\ntagger A <a@example.com> 1700000000 +0000\n{}",
        TAG_CASES[case]
    );
    let id = git_line(
        &repo,
        &["hash-object", "-t", "tag", "-w", "--stdin"],
        bytes.as_bytes(),
    );
    let tag = Tag::parse(format, bytes.as_bytes()).unwrap();
    let signed = tag.signed_payload();
    match capture(root.path(), &repo, "tag", &id) {
        Some(captured) => {
            let signed = signed.expect("Git found a signature");
            assert_eq!(signed.payload(), captured.payload);
            assert_eq!(signed.signature(), captured.signature);
        }
        None => assert_eq!(signed, None),
    }
}

/// Signs with a throwaway ssh key through Git, then checks the split with `ssh-keygen -Y verify`.
#[rstest]
fn git_ssh_signed_objects_verify_from_split_payloads(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    if !has_program("ssh-keygen", "-?") {
        eprintln!("skipping: ssh-keygen not installed");
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let key = root.path().join("key");
    let output = run(
        "ssh-keygen",
        root.path(),
        &[
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "test",
            "-f",
            key.to_str().unwrap(),
        ],
        b"",
    );
    assert!(output.status.success());
    let public = std::fs::read_to_string(root.path().join("key.pub")).unwrap();
    let allowed = root.path().join("allowed-signers");
    std::fs::write(&allowed, format!("a@example.com {public}")).unwrap();

    let repo = init(root.path(), format);
    let config = [
        ("gpg.format", "ssh".to_owned()),
        ("user.signingKey", key.display().to_string()),
        ("gpg.ssh.allowedSignersFile", allowed.display().to_string()),
    ];
    for (name, value) in &config {
        git(&repo, &["config", name, value], b"");
    }
    std::fs::write(repo.join("f"), "x").unwrap();
    git(&repo, &["add", "f"], b"");
    git(&repo, &["commit", "-qS", "-m", "base"], b"");
    git(&repo, &["checkout", "-qb", "side"], b"");
    git(
        &repo,
        &["commit", "-qS", "--allow-empty", "-m", "side"],
        b"",
    );
    git(&repo, &["tag", "-s", "-m", "side tag", "side-tag"], b"");
    git(&repo, &["checkout", "-q", "-"], b"");
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "main"], b"");
    git(
        &repo,
        &["merge", "-q", "-S", "--no-ff", "-m", "merge", "side-tag"],
        b"",
    );

    let verify = |signed: girt::signing::SignedPayload| {
        assert_eq!(signed.format(), Some(SignatureFormat::Ssh));
        let signature = root.path().join("signature");
        std::fs::write(&signature, signed.signature()).unwrap();
        let output = run(
            "ssh-keygen",
            root.path(),
            &[
                "-Y",
                "verify",
                "-f",
                allowed.to_str().unwrap(),
                "-I",
                "a@example.com",
                "-n",
                "git",
                "-s",
                signature.to_str().unwrap(),
            ],
            signed.payload(),
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    let merge = Commit::parse(format, &git(&repo, &["cat-file", "commit", "HEAD"], b"")).unwrap();
    assert_eq!(
        merge.id().to_string(),
        git_line(&repo, &["rev-parse", "HEAD"], b"")
    );
    verify(merge.signed_payload().unwrap());

    let tag = Tag::parse(format, &git(&repo, &["cat-file", "tag", "side-tag"], b"")).unwrap();
    verify(tag.signed_payload().unwrap());

    let merged: Vec<Tag> = merge.merge_tags().collect::<Result<_, _>>().unwrap();
    assert_eq!(merged, [tag]);
    verify(merged[0].signed_payload().unwrap());
}
