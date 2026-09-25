//! Original runtime fixtures generated through Git commands in isolated repositories.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::{LooseObjects, ObjectFormat, ObjectId, ObjectKind, Signature, Tag, TagFields};
use rstest::rstest;

/// Runs Git in the disposable repository with supplied stdin and returns stdout.
/// Inherited Git overrides and user/system configuration are disabled; initialization disables
/// templates explicitly at the call site.
fn git(directory: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }

    let mut child = command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A. Writer")
        .env("GIT_AUTHOR_EMAIL", "author@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0530")
        .env("GIT_COMMITTER_NAME", "C. Recorder")
        .env("GIT_COMMITTER_EMAIL", "committer@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000123 -0700")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git is required for interoperability tests");

    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    output.stdout
}

fn init(directory: &Path) -> LooseObjects {
    git(
        directory,
        &["init", "--bare", "--object-format=sha1", "--template=", "."],
        b"",
    );
    LooseObjects::new(directory.join("objects"), ObjectFormat::Sha1)
}

fn id(output: &[u8]) -> ObjectId {
    std::str::from_utf8(output).unwrap().trim().parse().unwrap()
}

fn target(directory: &Path, kind: ObjectKind) -> ObjectId {
    match kind {
        ObjectKind::Blob => id(&git(
            directory,
            &["hash-object", "-w", "--stdin"],
            b"release content\n",
        )),
        ObjectKind::Tree => id(&git(directory, &["mktree"], b"")),
        ObjectKind::Commit => {
            let tree = target(directory, ObjectKind::Tree);
            id(&git(
                directory,
                &["commit-tree", &tree.to_string()],
                b"snapshot\n",
            ))
        }
        ObjectKind::Tag => {
            let blob = target(directory, ObjectKind::Blob);
            let payload =
                format!("object {blob}\ntype blob\ntag inner\ntagger A <a> 1 +0000\n\ninner tag\n");
            id(&git(directory, &["mktag"], payload.as_bytes()))
        }
    }
}

fn remove_object(directory: &Path, id: ObjectId) {
    let hex = id.to_string();
    std::fs::remove_file(directory.join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
}

#[rstest]
#[case::blob(ObjectKind::Blob)]
#[case::tree(ObjectKind::Tree)]
#[case::commit(ObjectKind::Commit)]
#[case::tag(ObjectKind::Tag)]
fn agrees_with_git_in_both_directions(#[case] kind: ObjectKind) {
    let root = tempfile::tempdir().unwrap();
    let objects = init(root.path());
    let target = target(root.path(), kind);
    // Git's porcelain creates the independent payload; ref creation is confined to this fixture.
    git(
        root.path(),
        &[
            "-c",
            "tag.gpgSign=false",
            "tag",
            "-a",
            "v1",
            "-m",
            "release",
            &target.to_string(),
        ],
        b"",
    );
    let git_id = id(&git(root.path(), &["rev-parse", "refs/tags/v1"], b""));
    let payload = git(root.path(), &["cat-file", "tag", &git_id.to_string()], b"");
    let expected = Tag::new(TagFields {
        target,
        target_kind: kind,
        name: b"v1".to_vec(),
        tagger: Some(Signature {
            name: b"C. Recorder".to_vec(),
            email: b"committer@example.com".to_vec(),
            seconds: 1_700_000_123,
            offset_minutes: -420,
        }),
        extra_headers: vec![],
        message: b"release\n".to_vec(),
    });
    let expected = expected.unwrap();
    let parsed = objects.read_tag(git_id, payload.len()).unwrap();
    assert_eq!(parsed.to_fields().unwrap(), expected.to_fields().unwrap());
    assert_eq!(parsed.encode(), payload);
    assert_eq!(expected.encode(), payload);
    assert_eq!(expected.id(), git_id);
    remove_object(root.path(), git_id);
    assert_eq!(objects.write_tag(&expected).unwrap(), git_id);
    assert_eq!(
        git(root.path(), &["cat-file", "tag", &git_id.to_string()], b""),
        expected.encode()
    );
    assert_eq!(
        id(&git(root.path(), &["mktag"], expected.as_bytes())),
        git_id
    );
    git(
        root.path(),
        &["fsck", "--strict", "--no-reflogs", &git_id.to_string()],
        b"",
    );
}

#[rstest]
#[case::no_tagger(b"\nmessage\n")]
#[case::no_tagger_no_separator(b"")]
#[case::tagger_no_separator(b"tagger A <a> 1 +0000\n")]
#[case::unknown_lines(
    b"tagger A <a> 1 +0000\nx first\n continuation\nx second\nbare-line\n\nmessage"
)]
#[case::signature(b"tagger A <a> 1 +0000\n\nrelease\n-----BEGIN PGP SIGNATURE-----\nopaque unverified data\n-----END PGP SIGNATURE-----\n")]
fn preserves_git_accepted_framing(#[case] tail: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let objects = init(root.path());
    let target = target(root.path(), ObjectKind::Blob);
    let headers = format!("object {target}\ntype blob\ntag v1\n");
    let payload = [headers.as_bytes(), tail].concat();
    let git_id = id(&git(
        root.path(),
        &[
            "-c",
            "fsck.missingTaggerEntry=ignore",
            "-c",
            "fsck.extraHeaderEntry=ignore",
            "mktag",
        ],
        &payload,
    ));
    let tag = objects.read_tag(git_id, payload.len()).unwrap();
    assert_eq!(tag.encode(), payload);
    assert_eq!(tag.id(), git_id);
    remove_object(root.path(), git_id);
    assert_eq!(objects.write_tag(&tag).unwrap(), git_id);
    assert_eq!(
        git(root.path(), &["cat-file", "tag", &git_id.to_string()], b""),
        payload
    );
}

#[rstest]
#[case::lexical(b"v1", b"tagger A <a> +00042 -0000\n\n")]
#[case::binary(b"v\xff", b"tagger A <a> 1 +0000\nx \xff\0\r\n\n\xff\0message")]
#[case::invalid_fields(b"", b"tagger  <> -1 +0000\n\n")]
fn preserves_literal_objects_without_claiming_fsck_validity(
    #[case] name: &[u8],
    #[case] tail: &[u8],
) {
    let root = tempfile::tempdir().unwrap();
    let objects = init(root.path());
    let target = target(root.path(), ObjectKind::Blob);
    let headers = format!(
        "object {}\ntype blob\ntag ",
        target.to_string().to_uppercase()
    );
    let payload = [headers.as_bytes(), name, b"\n", tail].concat();
    let git_id = id(&git(
        root.path(),
        &["hash-object", "-w", "--literally", "-t", "tag", "--stdin"],
        &payload,
    ));
    let tag = objects.read_tag(git_id, payload.len()).unwrap();
    assert_eq!(tag.encode(), payload);
    assert_eq!(tag.id(), git_id);
    remove_object(root.path(), git_id);
    assert_eq!(objects.write_tag(&tag).unwrap(), git_id);
    assert_eq!(
        git(root.path(), &["cat-file", "tag", &git_id.to_string()], b""),
        payload
    );
}
