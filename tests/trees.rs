//! Original fixtures; Git interoperability provenance is in docs/compatibility.md.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use girt::{EntryMode, LooseObjects, ObjectFormat, ObjectId, Tree, TreeEntry};
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

// Deliberately scrambled input exercises mktree's ordering independently of girt. Object references
// are fixed synthetic bytes; --missing avoids expanding this test into other object capabilities.
fn entries() -> Vec<TreeEntry> {
    vec![
        TreeEntry {
            mode: EntryMode::Blob,
            name: b"a0".to_vec(),
            id: ObjectId::from_bytes([1; 20]),
        },
        TreeEntry {
            mode: EntryMode::Tree,
            name: b"a".to_vec(),
            id: ObjectId::from_bytes([2; 20]),
        },
        TreeEntry {
            mode: EntryMode::Executable,
            name: b"a.c".to_vec(),
            id: ObjectId::from_bytes([3; 20]),
        },
        TreeEntry {
            mode: EntryMode::Symlink,
            name: b"link\t\n".to_vec(),
            id: ObjectId::from_bytes([4; 20]),
        },
        TreeEntry {
            mode: EntryMode::Gitlink,
            name: b"sub".to_vec(),
            id: ObjectId::from_bytes([5; 20]),
        },
        TreeEntry {
            mode: EntryMode::Blob,
            name: b"\xff".to_vec(),
            id: ObjectId::from_bytes([6; 20]),
        },
    ]
}

fn git_listing() -> Vec<u8> {
    [
        format!("100644 blob {}\ta0\0", "01".repeat(20)).into_bytes(),
        format!("040000 tree {}\ta\0", "02".repeat(20)).into_bytes(),
        format!("100755 blob {}\ta.c\0", "03".repeat(20)).into_bytes(),
        format!("120000 blob {}\tlink\t\n\0", "04".repeat(20)).into_bytes(),
        format!("160000 commit {}\tsub\0", "05".repeat(20)).into_bytes(),
        [
            format!("100644 blob {}\t", "06".repeat(20)).as_bytes(),
            b"\xff\0",
        ]
        .concat(),
    ]
    .concat()
}

#[rstest]
#[case::empty(vec![], vec![])]
#[case::all_modes_and_byte_names(entries(), git_listing())]
fn interoperates_with_git_in_both_directions(
    #[case] entries: Vec<TreeEntry>,
    #[case] listing: Vec<u8>,
) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "--bare", "--object-format=sha1", "--template=", "."],
        b"",
    );

    let tree = Tree::new(entries).unwrap();
    let id = tree.id().to_string();

    let git_id = git(root.path(), &["mktree", "-z", "--missing"], &listing);
    assert_eq!(std::str::from_utf8(&git_id).unwrap().trim(), id);

    let git_payload = git(root.path(), &["cat-file", "tree", &id], b"");
    assert_eq!(tree.encode(), git_payload);

    let objects = LooseObjects::new(root.path().join("objects"), ObjectFormat::Sha1).unwrap();
    let parsed = objects.read_tree(tree.id(), git_payload.len()).unwrap();
    assert_eq!(parsed.entries(), tree.entries());
    assert_eq!(parsed.validate(), Ok(()));
    assert_eq!(parsed.id(), tree.id());

    // Remove the Git-produced object so the second half necessarily imports girt's payload.
    std::fs::remove_file(root.path().join("objects").join(&id[..2]).join(&id[2..])).unwrap();
    assert_eq!(objects.write_tree(&tree).unwrap(), tree.id());

    let read_listing = git(root.path(), &["ls-tree", "-z", &id], b"");
    let reconstructed = git(root.path(), &["mktree", "-z", "--missing"], &read_listing);
    assert_eq!(reconstructed, git_id);
    assert_eq!(
        git(root.path(), &["cat-file", "tree", &id], b""),
        tree.encode()
    );
}

#[rstest]
#[case::unsorted(b"40000 a\0", b"100644 a.c\0")]
#[case::duplicate(b"100644 a\0", b"100644 a\0")]
#[case::invalid_name(b"100644 a/b\0", b"100644 \0")]
fn preserves_noncanonical_identity(#[case] first: &[u8], #[case] second: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "--bare", "--object-format=sha1", "--template=", "."],
        b"",
    );

    let payload = [first, &[0x81; 20], second, &[0x82; 20]].concat();
    let tree = Tree::parse(&payload).unwrap();
    let expected = git(
        root.path(),
        &["hash-object", "-w", "--literally", "-t", "tree", "--stdin"],
        &payload,
    );

    assert_eq!(tree.encode(), payload);
    assert_eq!(
        tree.id().to_string(),
        std::str::from_utf8(&expected).unwrap().trim()
    );
    assert!(tree.validate().is_err());
    let objects = LooseObjects::new(root.path().join("objects"), ObjectFormat::Sha1).unwrap();
    assert_eq!(objects.read_tree(tree.id(), payload.len()).unwrap(), tree);
    let hex = tree.id().to_string();
    std::fs::remove_file(root.path().join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
    assert_eq!(objects.write_tree(&tree).unwrap(), tree.id());
    assert_eq!(git(root.path(), &["cat-file", "tree", &hex], b""), payload);
}
