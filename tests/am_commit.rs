//! Original Git am commit payload observations under isolated configuration.
use std::fs;
use std::sync::atomic::AtomicBool;

use girt::am::{CommitLimits, PrepareError, State};
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
        "É Writer <e@example.com>",
        "Bang! Writer <b!@example.com>",
        "Mix!' Writer <mix!'@example.com>"
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
        git(root, &["rev-parse", "HEAD"], b"").trim_ascii(),
        parent_text.as_bytes()
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
#[case("valid")]
#[case("exact_limit")]
#[case("literal")]
#[case("missing_message")]
#[case("calendar")]
#[case("identity")]
#[case("format")]
#[case("script")]
#[case("substitution")]
#[case("unknown")]
#[case("missing")]
#[case("date")]
#[case("limit")]
#[case("cancel")]
fn metadata_admission_reports_the_expected_outcome(#[case] failure: &str) {
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
    let sentinel = temporary.path().join("sentinel");
    let mut script = b"GIT_AUTHOR_NAME='Writer'\nGIT_AUTHOR_EMAIL='w@example.com'\nGIT_AUTHOR_DATE='Mon, 17 Sep 2001 00:00:00 +0000'\n".to_vec();
    match failure {
        "script" => {
            script.extend_from_slice(format!("touch '{}'\n", sentinel.display()).as_bytes())
        }
        "substitution" => {
            script = format!("GIT_AUTHOR_NAME=$(touch '{}')\n", sentinel.display()).into_bytes()
        }
        "unknown" => script.extend_from_slice(b"EXTRA='value'\n"),
        "date" => {
            script = String::from_utf8(script)
                .unwrap()
                .replace("Mon, 17 Sep 2001 00:00:00 +0000", "yesterday")
                .into_bytes()
        }
        "calendar" => {
            script = String::from_utf8(script)
                .unwrap()
                .replace("17 Sep", "31 Feb")
                .into_bytes()
        }
        "identity" => {
            script = String::from_utf8(script)
                .unwrap()
                .replace("NAME='Writer'", "NAME=''")
                .into_bytes()
        }
        "literal" => {
            script = String::from_utf8(script)
                .unwrap()
                .replace(
                    "NAME='Writer'",
                    &format!("NAME='$(touch {})'", sentinel.display()),
                )
                .into_bytes()
        }
        _ => {}
    }
    if failure != "missing" {
        fs::write(dir.join("author-script"), &script).unwrap();
    }
    if failure == "missing_message" {
        fs::remove_file(dir.join("final-commit")).unwrap();
    }
    let snapshot = repo.operation_state(OperationLimits::default()).unwrap();
    let state = State::from_operation(&snapshot).unwrap().unwrap();
    let mut limits = CommitLimits::default();
    let tree = girt::Tree::new(repo.object_format(), vec![]).unwrap().id();
    let committer = Signature::parse(b"Writer <w@example.com> 1000684800 +0000").unwrap();
    if failure == "limit" {
        limits.max_metadata_bytes = 1;
    }
    if failure == "exact_limit" {
        limits.max_metadata_bytes = script.len()
            + state.message.unwrap().len()
            + committer.name.len()
            + committer.email.len();
    }
    let parent = (failure == "format").then(|| ObjectId::null(girt::ObjectFormat::Sha256));
    let result = state.prepare_commit(
        tree,
        parent,
        committer,
        limits,
        &AtomicBool::new(failure == "cancel"),
    );
    match failure {
        "valid" | "exact_limit" | "literal" => {
            let commit = result.unwrap();
            assert!(commit.parents().is_empty());
            assert_eq!(commit.message(), b"subject\n");
        }
        "script" | "substitution" | "unknown" => {
            assert!(matches!(result, Err(PrepareError::Script)))
        }
        "missing" => assert!(matches!(
            result,
            Err(PrepareError::Missing("author-script"))
        )),
        "missing_message" => assert!(matches!(result, Err(PrepareError::Missing("final-commit")))),
        "date" => assert!(matches!(result, Err(PrepareError::DateSyntax))),
        "calendar" => assert!(matches!(result, Err(PrepareError::Date(_)))),
        "identity" | "format" => assert!(matches!(result, Err(PrepareError::Commit(_)))),
        "limit" => assert!(matches!(result, Err(PrepareError::Limit))),
        "cancel" => assert!(matches!(result, Err(PrepareError::Cancelled))),
        _ => unreachable!(),
    }
    assert!(!temporary.path().join("sentinel").exists());
    assert!(!repo.git_dir().join("sentinel").exists());
}
