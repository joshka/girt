//! Original dual-format fixtures, observed through Git's public executable only.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;

use girt::{
    Commit, CommitFields, CommitHeader, EntryMode, HistoryLimits, InitKind, ObjectFormat, ObjectId,
    ObjectKind, PackLimits, ReadLimits, Repository, Signature, Tag, TagFields, Tree,
    TreeCompareLimits, TreeEntry,
};
use rstest::rstest;

fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn hash(path: &Path, kind: &str, bytes: &[u8]) -> ObjectId {
    let out = git(path, &["hash-object", "-w", "--stdin", "-t", kind], bytes);
    std::str::from_utf8(&out).unwrap().trim().parse().unwrap()
}
fn person(seconds: i64) -> Signature {
    Signature {
        name: b"A".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds,
        offset_minutes: -420,
    }
}
fn commit(tree: ObjectId, parents: Vec<ObjectId>, seconds: i64) -> Commit {
    Commit::new(CommitFields {
        tree,
        parents,
        author: person(seconds),
        committer: person(seconds),
        extra_headers: vec![],
        message: b"original message\n".to_vec(),
    })
    .unwrap()
}
fn entry(mode: EntryMode, name: &[u8], id: ObjectId) -> TreeEntry {
    TreeEntry {
        mode,
        name: name.to_vec(),
        id,
    }
}

#[rstest]
#[case::sha1_bare(ObjectFormat::Sha1, InitKind::Bare)]
#[case::sha256_bare(ObjectFormat::Sha256, InitKind::Bare)]
#[case::sha1_worktree(ObjectFormat::Sha1, InitKind::Worktree)]
#[case::sha256_worktree(ObjectFormat::Sha256, InitKind::Worktree)]
fn public_objects_interoperate_both_directions(
    #[case] format: ObjectFormat,
    #[case] kind: InitKind,
) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("repo");
    let repo = Repository::init(format, &path, kind).unwrap();
    assert_eq!(Repository::discover(&path).unwrap().object_format(), format);
    assert_eq!(
        git(&path, &["rev-parse", "--show-object-format"], b""),
        format!("{format}\n").as_bytes()
    );
    let loose = repo.loose_objects();
    let bytes = b"binary\0\xff\n";
    let blob = loose.write_blob(bytes).unwrap();
    assert_eq!(hash(&path, "blob", bytes), blob);
    assert_eq!(
        git(&path, &["cat-file", "blob", &blob.to_string()], b""),
        bytes
    );
    let imported_blob = hash(&path, "blob", b"Git-written\0bytes");
    assert_eq!(
        loose.read_blob(imported_blob, 100).unwrap(),
        b"Git-written\0bytes"
    );
    let empty = Tree::new(format, vec![]).unwrap();
    let empty_id = loose.write_tree(&empty).unwrap();
    assert_eq!(hash(&path, "tree", b""), empty_id);
    let base = commit(empty_id, vec![], 1700000000);
    let base_id = loose.write_commit(&base).unwrap();
    let tree = Tree::new(
        format,
        vec![
            entry(EntryMode::Blob, b"file", blob),
            entry(EntryMode::Executable, b"exec", blob),
            entry(EntryMode::Symlink, b"link", imported_blob),
            entry(EntryMode::Tree, b"dir", empty_id),
            entry(EntryMode::Gitlink, b"submodule", base_id),
        ],
    )
    .unwrap();
    let tree_id = loose.write_tree(&tree).unwrap();
    assert_eq!(hash(&path, "tree", &tree.encode()), tree_id);
    assert_eq!(
        git(&path, &["cat-file", "tree", &tree_id.to_string()], b""),
        tree.encode()
    );
    let imported_tree = Tree::new(
        format,
        vec![entry(EntryMode::Blob, b"git-file", imported_blob)],
    )
    .unwrap();
    let imported_tree_id = hash(&path, "tree", &imported_tree.encode());
    assert_eq!(
        loose.read_tree(imported_tree_id, 1000).unwrap(),
        imported_tree
    );
    let child = commit(tree_id, vec![base_id], 1700000001);
    let child_id = loose.write_commit(&child).unwrap();
    assert_eq!(hash(&path, "commit", child.as_bytes()), child_id);
    assert_eq!(
        git(&path, &["cat-file", "commit", &child_id.to_string()], b""),
        child.as_bytes()
    );
    let imported_commit = commit(imported_tree_id, vec![child_id], 1700000002);
    let imported_id = hash(&path, "commit", imported_commit.as_bytes());
    assert_eq!(
        loose.read_commit(imported_id, 2000).unwrap(),
        imported_commit
    );
    let tag = Tag::new(TagFields {
        target: imported_id,
        target_kind: ObjectKind::Commit,
        name: b"v1".to_vec(),
        tagger: Some(person(1700000003)),
        extra_headers: vec![],
        message: b"tag\n".to_vec(),
    })
    .unwrap();
    let tag_id = loose.write_tag(&tag).unwrap();
    assert_eq!(hash(&path, "tag", tag.as_bytes()), tag_id);
    assert_eq!(
        git(&path, &["cat-file", "tag", &tag_id.to_string()], b""),
        tag.as_bytes()
    );
    let mut fields = tag.fields().clone();
    fields.name = b"v2".to_vec();
    let imported_tag = Tag::new(fields).unwrap();
    let imported_tag_id = hash(&path, "tag", imported_tag.as_bytes());
    assert_eq!(loose.read_tag(imported_tag_id, 2000).unwrap(), imported_tag);
    git(
        &path,
        &["update-ref", "refs/heads/main", &imported_id.to_string()],
        b"",
    );
    git(
        &path,
        &["update-ref", "refs/tags/v1", &tag_id.to_string()],
        b"",
    );
    git(&path, &["fsck", "--strict", "--no-dangling"], b"");
    let objects = repo.objects(PackLimits::default()).unwrap();
    let object = objects
        .read(tag_id, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(object.id(), tag_id);
    assert_eq!(object.object_format(), format);
    assert_eq!(
        objects
            .walk(&[imported_id], HistoryLimits::default())
            .unwrap(),
        vec![imported_id, child_id, base_id]
    );
    assert!(
        objects
            .is_ancestor(base_id, imported_id, HistoryLimits::default())
            .unwrap()
    );
    let changes = objects
        .compare_trees(
            None,
            Some(imported_tree_id),
            TreeCompareLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(changes.len(), 1);
    let content = girt::content_diff::BlobContent::read(
        &objects,
        &changes[0],
        ReadLimits::default(),
        1000,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(content.new, b"Git-written\0bytes");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn exact_signed_commit_and_opaque_headers(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let tree = loose
        .write_tree(&Tree::new(format, vec![]).unwrap())
        .unwrap();
    let original = commit(tree, vec![], -1);
    let mut fields = original.fields().clone();
    fields.extra_headers = vec![
        CommitHeader {
            name: b"gpgsig".to_vec(),
            value: b"opaque\nfirst".to_vec(),
        },
        CommitHeader {
            name: b"x-unknown".to_vec(),
            value: b"\xff\nsecond".to_vec(),
        },
        CommitHeader {
            name: b"gpgsig-sha256".to_vec(),
            value: b"other\nthird".to_vec(),
        },
    ];
    let signed = Commit::new(fields).unwrap();
    let payload = signed.encode();
    let raw_id = git(
        repo.git_dir(),
        &[
            "hash-object",
            "--literally",
            "-w",
            "--stdin",
            "-t",
            "commit",
        ],
        &payload,
    );
    let id = std::str::from_utf8(&raw_id)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let parsed = loose.read_commit(id, 4000).unwrap();
    assert_eq!(parsed.fields().author.seconds, -1);
    assert_eq!(parsed.as_bytes(), payload);
    assert_eq!(loose.write_commit(&parsed).unwrap(), id);
    assert_eq!(
        git(
            repo.git_dir(),
            &["cat-file", "commit", &id.to_string()],
            b""
        ),
        payload
    );
    let view = girt::CommitPayload::parse(&payload).unwrap();
    let unsigned = view.without_headers(&[3, 5]).unwrap();
    let decoded = Commit::parse(format, &unsigned).unwrap();
    assert_eq!(decoded.fields().extra_headers.len(), 1);
    assert_eq!(decoded.fields().extra_headers[0].name, b"x-unknown");
}

#[test]
fn sha256_scoped_refusals_preserve_metadata() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("repo");
    let repo = Repository::init(ObjectFormat::Sha256, &path, InitKind::Bare).unwrap();
    let config = std::fs::read(path.join("config")).unwrap();
    std::fs::write(path.join("index"), b"existing index sentinel").unwrap();
    assert!(matches!(
        repo.read_index(Default::default()),
        Err(girt::index::StorageError::UnsupportedFormat(
            ObjectFormat::Sha256
        ))
    ));
    assert!(matches!(
        repo.edit_index(Default::default()),
        Err(girt::index::StorageError::UnsupportedFormat(
            ObjectFormat::Sha256
        ))
    ));
    assert!(!path.join("index.lock").exists());
    assert_eq!(
        std::fs::read(path.join("index")).unwrap(),
        b"existing index sentinel"
    );
    assert!(matches!(
        repo.references(),
        Err(girt::refs::ReferenceError::Unsupported(_))
    ));
    assert!(matches!(
        Repository::init(ObjectFormat::Sha1, &path, InitKind::Bare),
        Err(girt::InitError::AlreadyExists(_))
    ));
    assert_eq!(std::fs::read(path.join("config")).unwrap(), config);
    std::fs::write(
        path.join("objects/pack/unpaired.pack"),
        b"existing pack sentinel",
    )
    .unwrap();
    assert!(matches!(
        repo.objects(PackLimits::default()),
        Err(girt::ObjectReadError::Unsupported(_))
    ));
    // Explicit loose access remains usable even in a repository with packed artifacts.
    let loose = repo.loose_objects();
    let id = loose.write_blob(b"loose").unwrap();
    assert_eq!(loose.read_blob(id, 5).unwrap(), b"loose");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn git_created_separate_and_linked_layouts(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let option = format!("--object-format={format}");
    git(
        root.path(),
        &[
            "init",
            "--template=",
            &option,
            "--separate-git-dir=metadata",
            "work",
        ],
        b"",
    );
    let work = root.path().join("work");
    let repo = Repository::open(&work).unwrap();
    assert_eq!(repo.object_format(), format);
    let loose = repo.loose_objects();
    let tree = loose
        .write_tree(&Tree::new(format, vec![]).unwrap())
        .unwrap();
    let id = loose
        .write_commit(&commit(tree, vec![], 1700000000))
        .unwrap();
    git(&work, &["update-ref", "HEAD", &id.to_string()], b"");
    git(
        &work,
        &["worktree", "add", "--detach", "../linked", &id.to_string()],
        b"",
    );
    let linked = root.path().join("linked");
    std::fs::create_dir(linked.join("nested")).unwrap();
    let found = Repository::discover(linked.join("nested")).unwrap();
    assert_eq!(found.object_format(), format);
    assert_eq!(found.common_dir(), repo.common_dir());
    assert_ne!(found.git_dir(), repo.git_dir());
    assert_eq!(
        found.loose_objects().read_commit(id, 2000).unwrap().id(),
        id
    );
}

#[test]
fn sha256_git_pack_is_explicitly_unsupported() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha256,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    let loose = repo.loose_objects();
    let tree = loose
        .write_tree(&Tree::new(ObjectFormat::Sha256, vec![]).unwrap())
        .unwrap();
    let id = loose
        .write_commit(&commit(tree, vec![], 1700000000))
        .unwrap();
    git(
        repo.git_dir(),
        &["update-ref", "refs/heads/main", &id.to_string()],
        b"",
    );
    git(repo.git_dir(), &["repack", "-ad"], b"");
    assert!(matches!(
        repo.objects(PackLimits::default()),
        Err(girt::ObjectReadError::Unsupported(_))
    ));
    git(repo.git_dir(), &["fsck", "--strict"], b"");
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1, ObjectFormat::Sha256)]
#[case::sha256(ObjectFormat::Sha256, ObjectFormat::Sha1)]
fn traversal_checks_format_limits_and_cancellation(
    #[case] format: ObjectFormat,
    #[case] foreign: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let tree = loose
        .write_tree(&Tree::new(format, vec![]).unwrap())
        .unwrap();
    let id = loose.write_commit(&commit(tree, vec![], 1)).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert!(
        objects
            .walk(&[ObjectId::null(foreign)], HistoryLimits::default())
            .is_err()
    );
    assert!(
        objects
            .walk(
                &[id],
                HistoryLimits {
                    max_commits: 0,
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(matches!(
        objects.compare_trees(
            None,
            Some(tree),
            TreeCompareLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(girt::TreeCompareError::Cancelled)
    ));
    assert!(matches!(
        objects.compare_trees(
            None,
            Some(ObjectId::null(foreign)),
            TreeCompareLimits::default(),
            &AtomicBool::new(false)
        ),
        Err(girt::TreeCompareError::ObjectFormat(_))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn imported_noncanonical_hex_and_dates_retain_bytes(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("repo"), InitKind::Bare).unwrap();
    let loose = repo.loose_objects();
    let tree = loose
        .write_tree(&Tree::new(format, vec![]).unwrap())
        .unwrap();
    let payload = format!(
        "tree {}\nauthor A <a> +0001 -0000\ncommitter C <c> 0002 +0000\nx-note first\n continued\nx-note second\n\nbytes\0",
        tree.to_string().to_uppercase()
    );
    let out = git(
        repo.git_dir(),
        &[
            "hash-object",
            "--literally",
            "-w",
            "--stdin",
            "-t",
            "commit",
        ],
        payload.as_bytes(),
    );
    let id = std::str::from_utf8(&out).unwrap().trim().parse().unwrap();
    let parsed = loose.read_commit(id, 2000).unwrap();
    assert_eq!(parsed.as_bytes(), payload.as_bytes());
    assert_eq!(parsed.fields().author.seconds, 1);
    assert_eq!(parsed.fields().extra_headers.len(), 2);
    assert_eq!(loose.write_commit(&parsed).unwrap(), id);
    assert_eq!(
        git(
            repo.git_dir(),
            &["cat-file", "commit", &id.to_string()],
            b""
        ),
        payload.as_bytes()
    );
}

#[test]
fn sha256_cannot_become_sha1_fetch_knowledge() {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha256,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        girt::fetch::KnownHistory::new(&objects, &[], Default::default(), &AtomicBool::new(false)),
        Err(girt::fetch::FetchError::Unsupported(_))
    ));
}
