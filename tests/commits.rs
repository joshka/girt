//! Original runtime fixtures; no Git implementation or fixtures are copied.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::{
    Commit, CommitFields, CommitHeader, LooseObjects, ObjectFormat, ObjectId, Signature, Tree,
};
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

fn init(directory: &Path) -> (LooseObjects, ObjectId) {
    git(
        directory,
        &["init", "--bare", "--object-format=sha1", "--template=", "."],
        b"",
    );
    let tree_id = git(directory, &["mktree"], b"");
    let id = std::str::from_utf8(&tree_id)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    (
        LooseObjects::new(directory.join("objects"), ObjectFormat::Sha1).unwrap(),
        id,
    )
}

fn git_commit(directory: &Path, tree: ObjectId, parents: &[ObjectId], message: &[u8]) -> ObjectId {
    let tree = tree.to_string();
    let parent_strings: Vec<_> = parents.iter().map(ToString::to_string).collect();
    let mut args = vec!["commit-tree", &tree];
    for parent in &parent_strings {
        args.extend(["-p", parent]);
    }
    let output = git(directory, &args, message);
    std::str::from_utf8(&output)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

fn parent_commits(directory: &Path, tree: ObjectId, messages: &[&[u8]]) -> Vec<ObjectId> {
    messages
        .iter()
        .map(|message| git_commit(directory, tree, &[], message))
        .collect()
}

fn fields(tree: ObjectId, parents: Vec<ObjectId>, message: &[u8]) -> CommitFields {
    CommitFields {
        tree,
        parents,
        author: Signature {
            name: b"A. Writer".to_vec(),
            email: b"author@example.com".to_vec(),
            seconds: 1_700_000_000,
            offset_minutes: 330,
        },
        committer: Signature {
            name: b"C. Recorder".to_vec(),
            email: b"committer@example.com".to_vec(),
            seconds: 1_700_000_123,
            offset_minutes: -420,
        },
        extra_headers: vec![],
        message: message.to_vec(),
    }
}

fn remove_object(directory: &Path, id: ObjectId) {
    let hex = id.to_string();
    std::fs::remove_file(directory.join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
}

#[rstest]
#[case::root(&[])]
#[case::single_parent(&[b"parent\n".as_slice()])]
#[case::merge(&[b"first parent\n".as_slice(), b"second parent\n".as_slice()])]
fn agrees_with_git_commit_tree_in_both_directions(#[case] messages: &[&[u8]]) {
    let root = tempfile::tempdir().unwrap();
    let (objects, tree) = init(root.path());
    let parents = parent_commits(root.path(), tree, messages);
    let expected = Commit::new(fields(tree, parents.clone(), b"record snapshot\n")).unwrap();
    let git_id = git_commit(root.path(), tree, &parents, b"record snapshot\n");
    let git_payload = git(
        root.path(),
        &["cat-file", "commit", &git_id.to_string()],
        b"",
    );
    let parsed = objects.read_commit(git_id, git_payload.len()).unwrap();

    assert_eq!(parsed.fields(), expected.fields());
    assert_eq!(parsed.encode(), git_payload);
    assert_eq!(expected.encode(), git_payload);
    assert_eq!(expected.id(), git_id);
    assert_eq!(parsed.validate(), Ok(()));

    remove_object(root.path(), git_id);
    assert_eq!(objects.write_commit(&expected).unwrap(), git_id);
    assert_eq!(
        git(
            root.path(),
            &["cat-file", "commit", &git_id.to_string()],
            b""
        ),
        expected.encode()
    );
    // fsck sees the actual tree and parents written above, without requiring a ref update.
    git(
        root.path(),
        &["fsck", "--strict", "--no-reflogs", &git_id.to_string()],
        b"",
    );
}

#[test]
fn stores_constructed_binary_message_and_opaque_headers() {
    let root = tempfile::tempdir().unwrap();
    let (objects, tree) = init(root.path());
    let mut fields = fields(tree, vec![], b"\xff\0message\r\nwithout final newline");
    fields.author.name = b"A\xff".to_vec();
    fields.extra_headers = vec![
        CommitHeader {
            name: b"encoding".to_vec(),
            value: b"ISO-8859-1".to_vec(),
        },
        CommitHeader {
            name: b"x-unknown".to_vec(),
            value: b"\xff\n continued\n\nend\n".to_vec(),
        },
        CommitHeader {
            name: b"x-unknown".to_vec(),
            value: b"again".to_vec(),
        },
        CommitHeader {
            name: b"gpgsig".to_vec(),
            value: b"opaque signature\nnot verified".to_vec(),
        },
    ];
    let commit = Commit::new(fields).unwrap();
    let id = objects.write_commit(&commit).unwrap();
    let git_id = git(
        root.path(),
        &["hash-object", "--literally", "-t", "commit", "--stdin"],
        commit.as_bytes(),
    );
    assert_eq!(std::str::from_utf8(&git_id).unwrap().trim(), id.to_string());
    assert_eq!(
        git(root.path(), &["cat-file", "commit", &id.to_string()], b""),
        commit.encode()
    );
    remove_object(root.path(), id);
    git(
        root.path(),
        &[
            "hash-object",
            "-w",
            "--literally",
            "-t",
            "commit",
            "--stdin",
        ],
        commit.as_bytes(),
    );
    assert_eq!(
        objects.read_commit(id, commit.as_bytes().len()).unwrap(),
        commit
    );
}

#[rstest]
#[case::lexical("A <a> +00042 -0000", b"x first\n \n  last\nx second\n")]
#[case::invalid_identity(" <> -1 +0000", b"x opaque\0\rvalue\n")]
fn preserves_noncanonical_git_storage(#[case] author: &str, #[case] extra: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let (objects, tree) = init(root.path());
    let headers = format!(
        "tree {}\nauthor {author}\ncommitter C <c> 1 +0000\n",
        tree.to_string().to_uppercase()
    );
    let payload = [headers.as_bytes(), extra, b"\n\xff\0body"].concat();
    let git_id = git(
        root.path(),
        &[
            "hash-object",
            "-w",
            "--literally",
            "-t",
            "commit",
            "--stdin",
        ],
        &payload,
    );
    let id: ObjectId = std::str::from_utf8(&git_id)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let commit = objects.read_commit(id, payload.len()).unwrap();
    assert_eq!(commit.as_bytes(), payload);
    assert_eq!(commit.id(), id);
    remove_object(root.path(), id);
    assert_eq!(objects.write_commit(&commit).unwrap(), id);
    assert_eq!(
        git(root.path(), &["cat-file", "commit", &id.to_string()], b""),
        payload
    );
}

#[test]
fn fixed_unit_identity_agrees_with_git() {
    let root = tempfile::tempdir().unwrap();
    let payload = b"tree 4b825dc642cb6eb9a060e54bf8d69288fbee4904\nauthor A <a@example.com> 1700000000 +0530\ncommitter A <a@example.com> 1700000000 +0530\n\nInitial\n";
    let git_id = git(
        root.path(),
        &["hash-object", "-t", "commit", "--stdin"],
        payload,
    );
    assert_eq!(
        std::str::from_utf8(&git_id).unwrap().trim(),
        Commit::parse(payload).unwrap().id().to_string()
    );
    assert_eq!(
        Commit::parse(payload).unwrap().fields().tree,
        Tree::new(vec![]).unwrap().id()
    );
}
