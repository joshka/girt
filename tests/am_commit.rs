//! Original Git am commit payload observations under isolated configuration.
use std::fs;
use std::sync::atomic::AtomicBool;

use girt::am::{CommitLimits, State};
use girt::{ObjectId, OperationLimits, Repository, Signature};
use rstest::rstest;

#[path = "support/apply_git.rs"]
mod oracle;
use oracle::{git, git_output};

#[rstest]
fn proposal_matches_git_continue(
    #[values("sha1", "sha256")] format: &str,
    #[values(
        "A Writer <a@example.com>",
        "O'Neil <o'neil@example.com>",
        "É Writer <e@example.com>"
    )]
    author: &str,
    #[values("+0000", "+0530", "-0730")] zone: &str,
) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    git(
        root,
        &["init", "-b", "main", &format!("--object-format={format}")],
        b"",
    );
    fs::write(root.join("file"), b"base\n").unwrap();
    git(root, &["add", "file"], b"");
    git(root, &["commit", "-m", "base"], b"");
    let parent = git(root, &["rev-parse", "HEAD"], b"");
    let parent_text = std::str::from_utf8(&parent).unwrap().trim();
    fs::write(root.join("new"), b"new\n").unwrap();
    git(root, &["add", "new"], b"");
    git(
        root,
        &[
            "commit",
            "--author",
            author,
            "--date",
            &format!("2001-09-17T12:34:56{zone}"),
            "-m",
            "mail subject\n\nbody",
        ],
        b"",
    );
    let mail = git(root, &["format-patch", "--stdout", "-1"], b"");
    git(root, &["checkout", "-b", "destination", parent_text], b"");
    fs::write(root.join("new"), b"untracked\n").unwrap();
    assert!(!git_output(root, &["am"], &mail).status.success());
    fs::remove_file(root.join("new")).unwrap();
    let repo = Repository::open(root).unwrap();
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let state = State::from_operation(&snapshot).unwrap().unwrap();
    git(root, &["apply", "--index"], state.patch.unwrap());
    let tree = git(root, &["write-tree"], b"");
    let tree: ObjectId = std::str::from_utf8(&tree).unwrap().trim().parse().unwrap();
    let parent = parent_text.parse().unwrap();
    let committer =
        Signature::parse(git(root, &["var", "GIT_COMMITTER_IDENT"], b"").trim_ascii()).unwrap();
    let index = fs::read(repo.git_dir().join("index")).unwrap();
    let proposal = state
        .prepare_commit(
            tree,
            Some(parent),
            committer,
            CommitLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(fs::read(repo.git_dir().join("index")).unwrap(), index);
    assert_eq!(
        git(root, &["rev-parse", "HEAD"], b""),
        parent_text
            .as_bytes()
            .iter()
            .copied()
            .chain([b'\n'])
            .collect::<Vec<_>>()
    );
    git(root, &["am", "--continue"], b"");
    assert_eq!(
        proposal.encode(),
        git(root, &["cat-file", "commit", "HEAD"], b"")
    );
    assert_eq!(
        proposal.id().to_string(),
        std::str::from_utf8(&git(root, &["rev-parse", "HEAD"], b""))
            .unwrap()
            .trim()
    );
}

#[rstest]
#[case("script")]
#[case("substitution")]
#[case("unknown")]
#[case("missing")]
#[case("date")]
#[case("limit")]
#[case("cancel")]
fn unsafe_metadata_is_refused(#[case] failure: &str) {
    let temporary = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        girt::ObjectFormat::Sha1,
        temporary.path().join("repo"),
        girt::InitKind::Bare,
    )
    .unwrap();
    let dir = repo.git_dir().join("rebase-apply");
    fs::create_dir(&dir).unwrap();
    for (name, bytes) in [
        ("applying", b"".as_slice()),
        ("next", b"1\n"),
        ("last", b"1\n"),
        ("0001", b"opaque mail"),
        ("final-commit", b"subject\n"),
    ] {
        fs::write(dir.join(name), bytes).unwrap();
    }
    let mut script = b"GIT_AUTHOR_NAME='Writer'\nGIT_AUTHOR_EMAIL='w@example.com'\nGIT_AUTHOR_DATE='Mon, 17 Sep 2001 00:00:00 +0000'\n".to_vec();
    match failure {
        "script" => script.extend_from_slice(b"touch sentinel\n"),
        "substitution" => script = b"GIT_AUTHOR_NAME=$(touch sentinel)\n".to_vec(),
        "unknown" => script.extend_from_slice(b"EXTRA='value'\n"),
        "date" => {
            script = String::from_utf8(script)
                .unwrap()
                .replace("Mon, 17 Sep 2001 00:00:00 +0000", "yesterday")
                .into_bytes()
        }
        _ => {}
    }
    if failure != "missing" {
        fs::write(dir.join("author-script"), script).unwrap();
    }
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let state = State::from_operation(&snapshot).unwrap().unwrap();
    let mut limits = CommitLimits::default();
    if failure == "limit" {
        limits.max_metadata_bytes = 1;
    }
    let tree = girt::Tree::new(repo.object_format(), vec![]).unwrap().id();
    let committer = Signature::parse(b"Writer <w@example.com> 1000684800 +0000").unwrap();
    assert!(
        state
            .prepare_commit(
                tree,
                None,
                committer,
                limits,
                &AtomicBool::new(failure == "cancel")
            )
            .is_err()
    );
    assert!(!temporary.path().join("sentinel").exists());
    assert!(!repo.git_dir().join("sentinel").exists());
}
