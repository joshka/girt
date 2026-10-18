//! Independently generated histories, observed through the installed Git executable.
use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use girt::blame::{Error, Limits};
use girt::{Repo, Repository};
use rstest::rstest;

fn git(path: &Path, args: &[&str]) -> String {
    git_at(path, args, 1700000000)
}
fn git_at(path: &Path, args: &[&str], seconds: i64) -> String {
    let mut command = Command::new("git");
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    let output = command
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent"))
        .env("GIT_AUTHOR_DATE", format!("@{seconds} +0000"))
        .env("GIT_COMMITTER_DATE", format!("@{seconds} +0000"))
        .env("GIT_AUTHOR_NAME", "Author")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "Author")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
fn commit(path: &Path, content: &[u8], name: &str) {
    std::fs::write(path.join(name), content).unwrap();
    git(path, &["add", "-A"]);
    git(
        path,
        &["-c", "commit.gpgsign=false", "commit", "-qm", "change"],
    );
}
fn fixture(format: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(
        directory.path(),
        &[
            "init",
            "-q",
            "--initial-branch=main",
            &format!("--object-format={format}"),
        ],
    );
    commit(directory.path(), b"alpha\nbeta\ngamma\ndelta\n", "old");
    directory
}
fn actual(handle: &Repo, path: &str) -> Vec<(String, usize, String)> {
    handle
        .blame("HEAD", path.as_bytes())
        .unwrap()
        .into_iter()
        .flat_map(|h| {
            (0..h.line_count).map(move |offset| {
                (
                    h.commit.to_string(),
                    h.original_start + offset,
                    String::from_utf8(h.original_path.clone()).unwrap(),
                )
            })
        })
        .collect()
}
fn oracle(path: &Path, file: &str) -> Vec<(String, usize, String)> {
    let output = git(path, &["blame", "--line-porcelain", "HEAD", "--", file]);
    let mut result = Vec::new();
    let mut origin = (String::new(), 0, String::new());
    for line in output.lines() {
        if line.starts_with('\t') {
            result.push(origin.clone());
        } else if let Some(path) = line.strip_prefix("filename ") {
            origin.2 = path.to_owned();
        } else {
            let fields: Vec<_> = line.split(' ').collect();
            if fields.len() >= 3
                && fields[0].len() >= 40
                && fields[0].bytes().all(|b| b.is_ascii_hexdigit())
            {
                origin.0 = fields[0].to_owned();
                origin.1 = fields[1].parse().unwrap();
            }
        }
    }
    result
}
#[rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn edited_rename_and_unterminated_lines_match_git(#[case] format: &str) {
    let dir = fixture(format);
    git(dir.path(), &["mv", "old", "new"]);
    commit(dir.path(), b"alpha\ninserted\nbeta\ngamma\ndelta\n", "new");
    commit(dir.path(), b"alpha\ninserted\nbeta\nDELTA", "new");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "new"), oracle(dir.path(), "new"));
}

/// Original repeated block: Git's default attributes the inserted header/body before the
/// earlier block; --no-indent-heuristic attributes the later identical pair instead.
#[rstest]
fn repetitive_code_uses_default_indent_heuristic(#[values("sha1", "sha256")] format: &str) {
    let dir = fixture(format);
    commit(dir.path(), b"head\n\nhead\n  body\n}\n}\n\n\n}\n", "code");
    commit(
        dir.path(),
        b"head\n\nhead\n  body\nhead\n  body\n}\n}\n\n\n}\n",
        "code",
    );
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "code"), oracle(dir.path(), "code"));
}

fn merge_fixture(both: bool) -> tempfile::TempDir {
    let dir = fixture("sha1");
    git(dir.path(), &["checkout", "-qb", "side"]);
    commit(dir.path(), b"alpha\nbeta\ngamma\nSIDE\n", "old");
    git(dir.path(), &["checkout", "-q", "main"]);
    let content = if both {
        b"MAIN\nbeta\ngamma\ndelta\n".as_slice()
    } else {
        b"alpha\nbeta\ngamma\ndelta\n".as_slice()
    };
    commit(dir.path(), content, "other");
    if both {
        commit(dir.path(), content, "old");
    }
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "merge",
            "-q",
            "--no-ff",
            "side",
            "-m",
            "merge",
        ],
    );
    dir
}
#[rstest]
#[case::ordinary(false)]
#[case::both_parents(true)]
fn merges_attribute_edits_from_both_parents(#[case] both: bool) {
    let dir = merge_fixture(both);
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "old"), oracle(dir.path(), "old"));
}
#[test]
fn limits_cancellation_binary_and_shallow_are_explicit() {
    let dir = fixture("sha1");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    let objects = handle.repository().objects().unwrap();
    let id = handle.rev_parse("HEAD").unwrap();
    let mut limits = Limits::default();
    limits.max_steps = 0;
    assert!(matches!(
        objects.blame_with(id, b"old", limits, &AtomicBool::new(false)),
        Err(Error::Limit("history"))
    ));
    assert!(matches!(
        objects.blame_with(id, b"old", Limits::default(), &AtomicBool::new(true)),
        Err(Error::Cancelled)
    ));
    assert!(matches!(
        objects.blame(id, b"../old"),
        Err(Error::InvalidPath)
    ));
    assert!(matches!(objects.blame(id, b"absent"), Err(Error::Absent)));
    commit(dir.path(), b"binary\0content", "binary");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert!(matches!(
        handle
            .blame("HEAD", b"binary")
            .unwrap_err()
            .downcast_ref::<Error>(),
        Some(Error::Binary)
    ));
    std::fs::write(dir.path().join(".git/shallow"), format!("{id}\n")).unwrap();
    let repo = Repository::open(dir.path()).unwrap();
    assert!(matches!(
        repo.objects().unwrap().blame(id, b"old"),
        Err(Error::Shallow(_))
    ));
}

#[test]
fn merge_matching_second_parent_preserves_its_origins() {
    let dir = fixture("sha1");
    git(dir.path(), &["checkout", "-qb", "side"]);
    commit(dir.path(), b"alpha\nSIDE\ngamma\ndelta\n", "old");
    git(dir.path(), &["checkout", "-q", "main"]);
    commit(dir.path(), b"MAIN\nbeta\ngamma\ndelta\n", "old");
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "merge",
            "--no-commit",
            "-s",
            "ours",
            "side",
        ],
    );
    git(dir.path(), &["checkout", "side", "--", "old"]);
    git(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "take side"],
    );
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "old"), oracle(dir.path(), "old"));
}

#[rstest]
#[case::lines(Limits::default().with_max_lines(1), "lines")]
#[case::bytes(Limits::default().with_max_bytes(1), "bytes")]
fn whole_walk_limits_refuse_partial_results(#[case] limits: Limits, #[case] name: &'static str) {
    let dir = fixture("sha1");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    let id = handle.rev_parse("HEAD").unwrap();
    assert!(
        matches!(handle.repository().objects().unwrap().blame_with(id, b"old", limits, &AtomicBool::new(false)), Err(Error::Limit(found)) if found == name)
    );
}

#[test]
fn empty_file_has_no_attribution_ranges() {
    let dir = fixture("sha1");
    commit(dir.path(), b"", "empty");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert!(handle.blame("HEAD", b"empty").unwrap().is_empty());
}

#[test]
fn byte_paths_and_non_utf8_lines_are_preserved() {
    let dir = fixture("sha1");
    let repo = Repository::open(dir.path()).unwrap();
    let loose = repo.loose_objects();
    let blob = loose.write_blob(b"first\xff\nsecond\r\n").unwrap();
    let mut tree = girt::tree::TreeEditor::new(&loose);
    tree.set(b"dir/\xff", blob).unwrap();
    let tree = tree.write().unwrap();
    let signature = girt::Signature {
        name: b"Author".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1700000000,
        offset_minutes: 0,
    };
    let commit = girt::Commit::builder(tree, signature, b"root")
        .build()
        .unwrap();
    let id = loose.write_commit(&commit).unwrap();
    let result = repo.objects().unwrap().blame(id, b"dir/\xff").unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].original_path, b"dir/\xff");
    assert_eq!(result[0].commit, id);
    assert_eq!(
        (
            result[0].original_start,
            result[0].final_start,
            result[0].line_count
        ),
        (1, 1, 2)
    );
}

#[test]
fn merge_with_independently_identical_edits_matches_git() {
    let dir = fixture("sha1");
    git(dir.path(), &["checkout", "-qb", "side"]);
    commit(dir.path(), b"alpha\nSAME\ngamma\nSIDE\n", "old");
    git(dir.path(), &["checkout", "-q", "main"]);
    commit(dir.path(), b"alpha\nSAME\ngamma\ndelta\n", "old");
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "merge",
            "--no-commit",
            "-s",
            "ours",
            "side",
        ],
    );
    git(dir.path(), &["checkout", "side", "--", "old"]);
    git(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "take side"],
    );
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "old"), oracle(dir.path(), "old"));
}

#[test]
fn merge_follows_rename_and_edits_from_both_parents() {
    let dir = fixture("sha1");
    git(dir.path(), &["checkout", "-qb", "side"]);
    git(dir.path(), &["mv", "old", "new"]);
    commit(dir.path(), b"alpha\nbeta\ngamma\nSIDE\n", "new");
    git(dir.path(), &["checkout", "-q", "main"]);
    commit(dir.path(), b"MAIN\nbeta\ngamma\ndelta\n", "old");
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "merge",
            "-q",
            "--no-ff",
            "side",
            "-m",
            "rename merge",
        ],
    );
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "new"), oracle(dir.path(), "new"));
}

#[test]
fn missing_parent_is_an_error_instead_of_a_root() {
    let dir = fixture("sha1");
    let parent = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_owned();
    commit(dir.path(), b"alpha\nbeta\ngamma\nchanged\n", "old");
    std::fs::remove_file(
        dir.path()
            .join(".git/objects")
            .join(&parent[..2])
            .join(&parent[2..]),
    )
    .unwrap();
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert!(matches!(
        handle
            .blame("HEAD", b"old")
            .unwrap_err()
            .downcast_ref::<Error>(),
        Some(Error::Operation(_))
    ));
}

#[test]
fn unchanged_history_reuses_verified_blob_under_byte_budget() {
    let dir = fixture("sha1");
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-qm",
            "unchanged",
        ],
    );
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    let limits = Limits::default().with_max_bytes(b"alpha\nbeta\ngamma\ndelta\n".len());
    let result = handle
        .blame_with("HEAD", b"old", limits, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        result[0].commit.to_string(),
        git(dir.path(), &["rev-parse", "HEAD~"]).trim()
    );
    assert_eq!(result[0].line_count, 4);
}

#[test]
fn directory_replaced_by_regular_file_starts_new_history() {
    let dir = fixture("sha1");
    std::fs::create_dir(dir.path().join("d")).unwrap();
    commit(dir.path(), b"keep\n", "d/f");
    std::fs::remove_dir_all(dir.path().join("d")).unwrap();
    commit(dir.path(), b"keep\nnew file\n", "d");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "d"), oracle(dir.path(), "d"));
}

#[test]
fn text_after_nul_containing_history_matches_git() {
    let dir = fixture("sha1");
    commit(dir.path(), b"retained\nold\0value\n", "f");
    commit(dir.path(), b"retained\nnew value\n", "f");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "f"), oracle(dir.path(), "f"));
}

#[test]
fn empty_file_at_shallow_boundary_has_no_origins() {
    let dir = fixture("sha1");
    commit(dir.path(), b"", "empty");
    let id = git(dir.path(), &["rev-parse", "HEAD"]);
    std::fs::write(dir.path().join(".git/shallow"), id).unwrap();
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert!(handle.blame("HEAD", b"empty").unwrap().is_empty());
}

#[rstest]
fn symlink_replaced_by_regular_file_starts_new_history(#[values("true", "false")] symlinks: &str) {
    let dir = fixture("sha1");
    git(dir.path(), &["config", "core.symlinks", symlinks]);
    let repo = Repository::open(dir.path()).unwrap();
    let target = repo.loose_objects().write_blob(b"target\n").unwrap();
    git(
        dir.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("120000,{target},link"),
        ],
    );
    git(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "symlink"],
    );
    // Drop the index-only symlink so adding the regular file cannot retain its old mode when
    // core.symlinks=false (the usual Git for Windows setting).
    git(
        dir.path(),
        &["update-index", "--force-remove", "--", "link"],
    );
    commit(dir.path(), b"target\nnew file\n", "link");
    assert!(git(dir.path(), &["ls-tree", "HEAD", "--", "link"]).starts_with("100644 blob "));
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "link"), oracle(dir.path(), "link"));
}

#[test]
fn rename_budget_exhaustion_never_silently_creates_an_origin() {
    let dir = fixture("sha1");
    git(dir.path(), &["mv", "old", "new"]);
    commit(dir.path(), b"alpha\nbeta\ngamma\nchanged\n", "new");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    let limits =
        Limits::default().with_rewrites(girt::rewrites::Limits::default().with_max_pairs(0));
    let error = handle
        .blame_with("HEAD", b"new", limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<Error>(),
            Some(Error::IncompleteRenames)
        ),
        "{error:?}"
    );
}

fn converging_fixture(unequal: bool) -> tempfile::TempDir {
    let dir = fixture("sha1");
    git(dir.path(), &["checkout", "-qb", "side"]);
    std::fs::write(dir.path().join("old"), b"alpha\nbeta\ngamma\nSIDE\n").unwrap();
    git(dir.path(), &["add", "old"]);
    git_at(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "side"],
        1700000001,
    );
    if unequal {
        git_at(
            dir.path(),
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "longer side",
            ],
            1700000002,
        );
    }
    git(dir.path(), &["checkout", "-q", "main"]);
    std::fs::write(dir.path().join("old"), b"MAIN\nbeta\ngamma\ndelta\n").unwrap();
    git(dir.path(), &["add", "old"]);
    git_at(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "main"],
        1700000003,
    );
    git(
        dir.path(),
        &[
            "-c",
            "commit.gpgsign=false",
            "merge",
            "--no-commit",
            "-s",
            "ours",
            "side",
        ],
    );
    std::fs::write(dir.path().join("old"), b"alpha\nbeta\ngamma\ndelta\n").unwrap();
    git(dir.path(), &["add", "old"]);
    git_at(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "merge"],
        1700000004,
    );
    dir
}
#[rstest]
#[case::equal(false, 8)]
#[case::unequal(true, 10)]
fn converging_merge_groups_share_pending_history_work(#[case] unequal: bool, #[case] steps: usize) {
    let dir = converging_fixture(unequal);
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    let result = handle
        .blame_with(
            "HEAD",
            b"old",
            Limits::default().with_max_steps(steps),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(actual(&handle, "old"), oracle(dir.path(), "old"));
}

#[test]
fn gitlink_replaced_by_regular_file_starts_new_history() {
    let dir = fixture("sha1");
    let target = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_owned();
    git(
        dir.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{target},sub"),
        ],
    );
    git(
        dir.path(),
        &["-c", "commit.gpgsign=false", "commit", "-qm", "gitlink"],
    );
    commit(dir.path(), b"new file\n", "sub");
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    assert_eq!(actual(&handle, "sub"), oracle(dir.path(), "sub"));
}

/// Configured ignored revisions alter Git attribution but are not applied by girt (#503).
#[test]
fn configured_ignore_revisions_are_not_applied() {
    let dir = fixture("sha1");
    let first = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_owned();
    commit(dir.path(), b"ALPHA\nbeta\ngamma\ndelta\n", "old");
    let ignored = git(dir.path(), &["rev-parse", "HEAD"]).trim().to_owned();
    std::fs::write(dir.path().join("ignored-revs"), format!("{ignored}\n")).unwrap();
    git(
        dir.path(),
        &["config", "blame.ignoreRevsFile", "ignored-revs"],
    );
    let handle = Repo::new(Repository::open(dir.path()).unwrap());
    let actual = actual(&handle, "old");
    let expected = oracle(dir.path(), "old");
    assert_eq!(actual[0].0, ignored);
    assert_eq!(expected[0].0, first);
    assert_ne!(actual, expected);
}
