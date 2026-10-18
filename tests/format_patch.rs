//! Original fixed-date mail fixtures observed through the isolated Git executable.
#[path = "support/apply_git.rs"]
mod oracle;

use std::sync::atomic::AtomicBool;

use girt::diffstat::{Counts, Diffstat, FileStat};
use girt::format_patch::{Limits, Message, Options};
use girt::{InitKind, ObjectFormat, Repository};
use rstest::rstest;

#[rstest]
#[case::plain(
    "Patch Author <author@example.com>",
    "Change subject\n\nBody paragraph."
)]
#[case::paragraph(
    "Patch Author <author@example.com>",
    "First subject\ncontinued line\n\nBody\nFrom dangerous"
)]
#[case::quoted("A, Writer <author@example.com>", "Quoted name\n\nBody")]
#[case::unicode("Jöhn Writer <author@example.com>", "Résumé subject\n\nBody ünicode.")]
#[case::long_ascii(
    "Patch Author <author@example.com>",
    "Long subject subject subject subject subject subject subject subject subject subject subject subject subject subject subject subject subject subject subject subject\n\nBody"
)]
#[case::long_unicode(
    "Patch Author <author@example.com>",
    "Résumé éééé éééé éééé éééé éééé éééé éééé éééé éééé éééé éééé éééé éééé éééé éééé\n\nBody"
)]
#[case::encoded_ascii_name("=?UTF-8?q?Linus?= <author@example.com>", "Subject\n\nBody")]
#[case::initial_name("Patch A. Author <author@example.com>", "Subject\n\nBody")]
#[case::body_whitespace("Patch Author <author@example.com>", "Subject\n\n  \nbody\n")]
#[case::encoded_ascii_subject("Patch Author <author@example.com>", "=?UTF-8?q?Fake?=\n\nBody")]
fn message_matches_git(
    #[case] author: &str,
    #[case] message: &str,
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values("RFC", "")] prefix: &str,
    #[values(
        "2001-09-17T00:00:00+0000",
        "2001-09-09T01:30:00+0130",
        "2001-09-09T01:30:00-0730"
    )]
    date: &str,
) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
    let root = repo.worktree().unwrap();
    std::fs::write(root.join("file"), b"old\n").unwrap();
    oracle::git(root, &["add", "file"], b"");
    oracle::git(root, &["commit", "-qm", "Base"], b"");
    std::fs::write(root.join("file"), b"new\n").unwrap();
    oracle::git(root, &["add", "file"], b"");
    oracle::git(
        root,
        &[
            "commit",
            "-qm",
            message,
            "--author",
            author,
            "--date",
            date,
            "--cleanup=verbatim",
        ],
        b"",
    );
    let objects = repo.objects().unwrap();
    let head = oracle::git(root, &["rev-parse", "HEAD"], b"");
    let head = std::str::from_utf8(&head).unwrap().trim().parse().unwrap();
    let commit = objects.read_commit(head).unwrap();
    let parent = objects.read_commit(commit.parents()[0]).unwrap();
    let entries = objects
        .diff_trees(Some(parent.tree()), Some(commit.tree()))
        .unwrap()
        .entries;
    let mut patch = Vec::new();
    objects.write_patch(&mut patch, &entries).unwrap();
    let stat = Diffstat::new(
        vec![FileStat {
            path: b"file".to_vec(),
            source_path: None,
            counts: Counts::Text {
                added: 1,
                removed: 1,
            },
        }],
        Default::default(),
    )
    .unwrap();
    let mail = Message::new(&commit, &stat, &patch);

    let actual = mail
        .render_with(
            Options::default().with_signature(Some(b"Test Signature")),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let expected = oracle::git(
        root,
        &[
            "format-patch",
            "--stdout",
            "--signature=Test Signature",
            "-1",
        ],
        b"",
    );
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );

    let actual = mail
        .render_with(
            Options::default()
                .with_signature(None)
                .with_subject_prefix(prefix.as_bytes())
                .with_position(1, 1)
                .with_base(Some(commit.parents()[0])),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let expected = oracle::git(
        root,
        &[
            "format-patch",
            "--stdout",
            "--no-signature",
            "--numbered",
            &format!("--subject-prefix={prefix}"),
            &format!("--base={}", commit.parents()[0]),
            "-1",
        ],
        b"",
    );
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
    let actual = mail
        .render_with(
            Options::default()
                .with_signature(None)
                .with_subject_prefix(prefix.as_bytes()),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let expected = oracle::git(
        root,
        &[
            "format-patch",
            "--stdout",
            "--no-signature",
            &format!("--subject-prefix={prefix}"),
            "-1",
        ],
        b"",
    );
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        String::from_utf8(expected).unwrap()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn empty_commit_has_no_diff_separator(#[case] format: ObjectFormat) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, temporary.path().join("r"), InitKind::Worktree).unwrap();
    let root = repo.worktree().unwrap();
    oracle::git(
        root,
        &["commit", "--allow-empty", "-qm", "Subject\n\nBody"],
        b"",
    );
    let id = oracle::git(root, &["rev-parse", "HEAD"], b"");
    let id = std::str::from_utf8(&id).unwrap().trim().parse().unwrap();
    let commit = repo.objects().unwrap().read_commit(id).unwrap();
    let stat = Diffstat::new(vec![], Default::default()).unwrap();

    let actual = Message::new(&commit, &stat, b"")
        .render_with(
            Options::default().with_signature(Some(b"Test Signature")),
            Limits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let expected = oracle::git(
        root,
        &[
            "format-patch",
            "--stdout",
            "--always",
            "--signature=Test Signature",
            "-1",
        ],
        b"",
    );
    assert_eq!(actual, expected);
}
