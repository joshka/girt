//! Original binary patch fixtures, checked through isolated Git CLI application.
#[path = "support/apply_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use girt::apply::Patch;
use girt::patch::{Limits, Options};
use girt::tree::TreeEditor;
use girt::{InitKind, ObjectFormat, ObjectId, Repository};
use rstest::rstest;

fn tree(repo: &Repository, bytes: Option<&[u8]>) -> ObjectId {
    let loose = repo.loose_objects();
    let mut editor = TreeEditor::new(&loose);
    if let Some(bytes) = bytes {
        editor
            .set(b"binary", loose.write_blob(bytes).unwrap())
            .unwrap();
    }
    editor.write().unwrap()
}

fn render(repo: &Repository, old: ObjectId, new: ObjectId, work: usize) -> Vec<u8> {
    let objects = repo.objects().unwrap();
    let entries = objects.diff_trees(Some(old), Some(new)).unwrap().entries;
    let mut bytes = Vec::new();
    objects
        .write_patch_with(
            &mut bytes,
            &entries,
            Options::default().with_binary_patch(true),
            None,
            Limits::default().with_binary_delta_work(work),
            &AtomicBool::new(false),
        )
        .unwrap();
    bytes
}

#[rstest]
#[case::sha1_literal(ObjectFormat::Sha1, Some(&b"\0old\n"[..]), Some(&b"\0new\n"[..]))]
#[case::sha256_literal(ObjectFormat::Sha256, Some(&b"\0old\n"[..]), Some(&b"\0new\n"[..]))]
#[case::addition(ObjectFormat::Sha1, None, Some(&b"\0new\n"[..]))]
#[case::deletion(ObjectFormat::Sha256, Some(&b"\0old\n"[..]), None)]
fn literal_blocks_apply_in_both_directions(
    #[case] format: ObjectFormat,
    #[case] old: Option<&[u8]>,
    #[case] new: Option<&[u8]>,
) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Bare).unwrap();
    let (old, new) = (tree(&repo, old), tree(&repo, new));
    let bytes = render(&repo, old, new, 0);
    assert!(bytes.windows(9).any(|part| part == b"literal 5"));
    verify(&repo, old, new, &bytes);
}

fn verify(repo: &Repository, old: ObjectId, new: ObjectId, bytes: &[u8]) {
    let root = repo.git_dir();
    oracle::git(root, &["read-tree", &old.to_string()], b"");
    oracle::git(root, &["apply", "--cached", "--binary"], bytes);
    assert_eq!(
        oracle::git(root, &["write-tree"], b""),
        format!("{new}\n").as_bytes()
    );
    oracle::git(root, &["apply", "--cached", "--binary", "--reverse"], bytes);
    assert_eq!(
        oracle::git(root, &["write-tree"], b""),
        format!("{old}\n").as_bytes()
    );

    let objects = repo.objects().unwrap();
    let patch = Patch::parse(bytes).unwrap();
    assert_eq!(patch.apply_tree(&objects, old).unwrap().tree().id(), new);
    assert_eq!(
        patch
            .reversed()
            .unwrap()
            .apply_tree(&objects, new)
            .unwrap()
            .tree()
            .id(),
        old
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn similar_blobs_use_bounded_deltas(#[case] format: ObjectFormat) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Bare).unwrap();
    let before: Vec<u8> = (0..4096u32)
        .flat_map(|n| n.wrapping_mul(2_654_435_761).to_le_bytes())
        .collect();
    let mut after = before.clone();
    after.extend_from_slice(b"\0new footer\n");
    let (old, new) = (tree(&repo, Some(&before)), tree(&repo, Some(&after)));

    let bytes = render(&repo, old, new, 1_000_000);
    assert!(bytes.windows(6).any(|part| part == b"delta "));
    verify(&repo, old, new, &bytes);

    let literal = render(&repo, old, new, 0);
    assert!(!literal.windows(6).any(|part| part == b"delta "));
    assert!(bytes.len() < literal.len());
    verify(&repo, old, new, &literal);

    let exhausted = render(&repo, old, new, 32);
    assert!(!exhausted.windows(6).any(|part| part == b"delta "));
    verify(&repo, old, new, &exhausted);

    let shared = render(&repo, old, new, 20_000);
    let parsed = Patch::parse(&shared).unwrap();
    assert_eq!(
        parsed.files()[0].binary_kind(),
        Some(girt::apply::BinaryKind::Delta)
    );
    assert_eq!(
        parsed.files()[0].reverse_binary_kind(),
        Some(girt::apply::BinaryKind::Literal)
    );
    verify(&repo, old, new, &shared);
}

#[test]
fn literal_framing_matches_git_at_fixed_compression() {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Bare,
    )
    .unwrap();
    let old = tree(&repo, Some(b"\0old\n"));
    let new = tree(&repo, Some(b"\0new\n"));
    let bytes = render(&repo, old, new, 0);
    let expected = oracle::git(
        repo.git_dir(),
        &[
            "-c",
            "core.compression=6",
            "diff",
            "--binary",
            &old.to_string(),
            &new.to_string(),
        ],
        b"",
    );
    assert_eq!(bytes, expected);
}

#[test]
fn gitlink_to_binary_is_split_before_encoding() {
    use girt::{Commit, EntryMode, Signature, TreeValue};

    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        temporary.path().join("r"),
        InitKind::Bare,
    )
    .unwrap();
    let loose = repo.loose_objects();
    let empty = tree(&repo, None);
    let author = Signature {
        name: b"Author".to_vec(),
        email: b"author@example.com".to_vec(),
        seconds: 1_000_000,
        offset_minutes: 0,
    };
    let commit = loose
        .write_commit(
            &Commit::builder(empty, author, b"submodule\n")
                .build()
                .unwrap(),
        )
        .unwrap();
    let mut editor = TreeEditor::new(&loose);
    editor
        .set_entry(
            b"binary",
            TreeValue {
                mode: EntryMode::Gitlink,
                id: commit,
            },
        )
        .unwrap();
    let old = editor.write().unwrap();
    let new = tree(&repo, Some(b"\0new\n"));

    let bytes = render(&repo, old, new, 0);
    let expected = oracle::git(
        repo.git_dir(),
        &[
            "-c",
            "core.compression=6",
            "diff",
            "--binary",
            &old.to_string(),
            &new.to_string(),
        ],
        b"",
    );
    assert_eq!(bytes, expected);
    oracle::git(repo.git_dir(), &["read-tree", &old.to_string()], b"");
    oracle::git(repo.git_dir(), &["apply", "--cached"], &bytes);
    assert_eq!(
        oracle::git(repo.git_dir(), &["write-tree"], b""),
        format!("{new}\n").as_bytes()
    );
    oracle::git(repo.git_dir(), &["apply", "--cached", "--reverse"], &bytes);
    assert_eq!(
        oracle::git(repo.git_dir(), &["write-tree"], b""),
        format!("{old}\n").as_bytes()
    );
}
