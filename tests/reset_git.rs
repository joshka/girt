//! `Repository::reset` and `reset_paths` compared with `git reset` on copies of one repository.
use std::path::Path;
use std::process::{Command, Output};

use girt::pathspec::{Context, Pathspec};
use girt::reset::{ResetError, ResetMode, ResetOptions};
use girt::{ObjectId, Repository, Signature};

fn run(directory: &Path, args: &[&str]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(directory)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", directory.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Reset Author")
        .env("GIT_AUTHOR_EMAIL", "reset@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "Reset Author")
        .env("GIT_COMMITTER_EMAIL", "reset@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .output()
        .expect("Git required for reset compatibility tests")
}

fn git(directory: &Path, args: &[&str]) -> String {
    let output = run(directory, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn committer() -> Signature {
    Signature {
        name: b"Reset Author".to_vec(),
        email: b"reset@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    }
}

/// Three commits on `main` and a `side` branch from the first, then staged, unstaged and
/// untracked changes in the working tree.
fn fixture(root: &Path) {
    git(
        root,
        &["init", "--quiet", "--initial-branch=main", "--template="],
    );
    for (n, content) in ["one", "two", "three"].iter().enumerate() {
        std::fs::write(root.join("a.txt"), format!("{content}\n")).unwrap();
        std::fs::create_dir_all(root.join("dir")).unwrap();
        std::fs::write(root.join("dir").join(format!("f{n}.txt")), *content).unwrap();
        git(root, &["add", "--all"]);
        git(root, &["commit", "--quiet", "-m", content]);
    }
    git(root, &["branch", "side", "main~2"]);
    std::fs::write(root.join("a.txt"), "staged\n").unwrap();
    git(root, &["add", "a.txt"]);
    std::fs::write(root.join("a.txt"), "unstaged\n").unwrap();
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    git(root, &["add", "new.txt"]);
    std::fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
}

/// What a reset can change, as Git reports it.
fn observe(root: &Path) -> Vec<String> {
    let mut seen = vec![
        git(root, &["rev-parse", "HEAD"]),
        String::from_utf8(run(root, &["symbolic-ref", "-q", "HEAD"]).stdout).unwrap(),
        git(root, &["ls-files", "--stage"]),
        // `-v` shows the assume-unchanged flag as a lowercase status letter.
        git(root, &["ls-files", "-v"]),
        git(root, &["status", "--porcelain", "--untracked-files=all"]),
        git(root, &["reflog", "-2", "--format=%H %gs", "HEAD"]),
    ];
    if run(root, &["symbolic-ref", "-q", "HEAD"]).status.success() {
        seen.push(git(root, &["reflog", "-2", "--format=%H %gs", "main"]));
    }
    seen.push(std::fs::read_to_string(root.join(".git/ORIG_HEAD")).unwrap_or_default());
    let mut files: Vec<_> = std::fs::read_dir(root.join(".git"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
        .collect();
    files.sort();
    seen.push(files.join(" "));
    seen.push(working_files(root));
    seen
}

/// Every working file outside `.git`, with whether it is executable and its content.
fn working_files(root: &Path) -> String {
    let mut listed = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.file_name().is_some_and(|name| name == ".git") {
                continue;
            }
            let metadata = std::fs::symlink_metadata(&path).unwrap();
            let relative = path.strip_prefix(root).unwrap().display().to_string();
            if metadata.is_dir() {
                pending.push(path);
            } else {
                #[cfg(unix)]
                let executable = {
                    use std::os::unix::fs::PermissionsExt;
                    metadata.permissions().mode() & 0o100 != 0
                };
                #[cfg(not(unix))]
                let executable = false;
                let content = String::from_utf8_lossy(&std::fs::read(&path).unwrap()).into_owned();
                listed.push(format!("{relative} {executable} {content:?}"));
            }
        }
    }
    listed.sort();
    listed.join("\n")
}

/// Runs `prepare` in a fresh fixture, then compares Git's command with girt's operation.
fn compare(
    prepare: impl Fn(&Path),
    git_args: &[&str],
    girt: impl Fn(&Repository, &Path) -> Result<(), ResetError>,
) {
    let expected = tempfile::tempdir().unwrap();
    let actual = tempfile::tempdir().unwrap();
    for root in [expected.path(), actual.path()] {
        fixture(root);
        prepare(root);
    }
    git(expected.path(), git_args);
    let repository = Repository::open(actual.path()).unwrap();
    girt(&repository, actual.path()).unwrap();
    assert_eq!(
        observe(actual.path()),
        observe(expected.path()),
        "{git_args:?}"
    );
}

fn resolve(root: &Path, revision: &str) -> ObjectId {
    git(root, &["rev-parse", revision]).trim().parse().unwrap()
}

fn tree(root: &Path, revision: &str) -> Option<ObjectId> {
    Some(resolve(root, &format!("{revision}^{{tree}}")))
}

fn paths(specs: &[&str]) -> Pathspec {
    Pathspec::parse(specs.iter().map(|s| s.as_bytes()), &Context::new()).unwrap()
}

fn nothing(_: &Path) {}

#[test]
fn soft_keeps_index_and_files() {
    compare(
        nothing,
        &["reset", "--quiet", "--soft", "HEAD~1"],
        |repo, root| {
            let options = ResetOptions::new(committer(), "HEAD~1");
            repo.reset(resolve(root, "HEAD~1"), ResetMode::Soft, &options)
                .map(drop)
        },
    );
}

#[test]
fn soft_to_same_commit_still_records() {
    compare(
        nothing,
        &["reset", "--quiet", "--soft", "HEAD"],
        |repo, root| {
            let options = ResetOptions::new(committer(), "HEAD");
            repo.reset(resolve(root, "HEAD"), ResetMode::Soft, &options)
                .map(drop)
        },
    );
}

#[test]
fn mixed_replaces_index() {
    compare(
        nothing,
        &["reset", "--quiet", "--mixed", "side"],
        |repo, root| {
            let options = ResetOptions::new(committer(), "side");
            repo.reset(resolve(root, "side"), ResetMode::Mixed, &options)
                .map(drop)
        },
    );
}

#[test]
fn mixed_on_detached_head_moves_only_head() {
    let detach = |root: &Path| {
        git(root, &["checkout", "--quiet", "--detach"]);
    };
    compare(detach, &["reset", "--quiet", "HEAD~2"], |repo, root| {
        let options = ResetOptions::new(committer(), "HEAD~2");
        repo.reset(resolve(root, "HEAD~2"), ResetMode::Mixed, &options)
            .map(drop)
    });
}

#[test]
fn commit_resets_forget_interrupted_operations() {
    let interrupt = |root: &Path| {
        let head = git(root, &["rev-parse", "HEAD"]);
        for name in [
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "MERGE_MSG",
            "SQUASH_MSG",
            "AUTO_MERGE",
        ] {
            std::fs::write(root.join(".git").join(name), &head).unwrap();
        }
    };
    compare(
        interrupt,
        &["reset", "--quiet", "--soft", "HEAD~1"],
        |repo, root| {
            let options = ResetOptions::new(committer(), "HEAD~1");
            repo.reset(resolve(root, "HEAD~1"), ResetMode::Soft, &options)
                .map(drop)
        },
    );
    let merge = |root: &Path| {
        interrupt(root);
        let side = git(root, &["rev-parse", "side"]);
        std::fs::write(root.join(".git/MERGE_HEAD"), side).unwrap();
        std::fs::write(root.join(".git/MERGE_MODE"), "").unwrap();
    };
    compare(merge, &["reset", "--quiet"], |repo, root| {
        let options = ResetOptions::new(committer(), "HEAD");
        repo.reset(resolve(root, "HEAD"), ResetMode::Mixed, &options)
            .map(drop)
    });
}

#[test]
fn soft_reset_refused_during_merge() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let side = git(root.path(), &["rev-parse", "side"]);
    std::fs::write(root.path().join(".git/MERGE_HEAD"), side).unwrap();
    assert!(
        !run(root.path(), &["reset", "--quiet", "--soft", "HEAD~1"])
            .status
            .success()
    );
    let before = observe(root.path());
    let repository = Repository::open(root.path()).unwrap();
    let options = ResetOptions::new(committer(), "HEAD~1");
    let result = repository.reset(resolve(root.path(), "HEAD~1"), ResetMode::Soft, &options);
    assert!(matches!(result, Err(ResetError::MergeInProgress)));
    assert_eq!(observe(root.path()), before);
}

#[test]
fn path_reset_unstages() {
    compare(
        nothing,
        &["reset", "--quiet", "--", "a.txt", "new.txt"],
        |repo, root| {
            repo.reset_paths(
                tree(root, "HEAD"),
                &paths(&["a.txt", "new.txt"]),
                &Default::default(),
            )
        },
    );
    compare(
        nothing,
        &["reset", "--quiet", "side", "--", "dir"],
        |repo, root| repo.reset_paths(tree(root, "side"), &paths(&["dir"]), &Default::default()),
    );
    compare(
        nothing,
        &["restore", "--staged", "--source=side", ":(glob)**/*.txt"],
        |repo, root| {
            repo.reset_paths(
                tree(root, "side"),
                &paths(&[":(glob)**/*.txt"]),
                &Default::default(),
            )
        },
    );
}

#[test]
fn path_reset_resolves_conflicts() {
    let conflict = |root: &Path| {
        git(root, &["stash", "--quiet", "--include-untracked"]);
        git(root, &["checkout", "--quiet", "-b", "other", "main~1"]);
        std::fs::write(root.join("a.txt"), "other\n").unwrap();
        git(root, &["commit", "--quiet", "-am", "other"]);
        git(root, &["checkout", "--quiet", "main"]);
        assert!(!run(root, &["merge", "--quiet", "other"]).status.success());
    };
    compare(
        conflict,
        &["reset", "--quiet", "--", "a.txt"],
        |repo, root| repo.reset_paths(tree(root, "HEAD"), &paths(&["a.txt"]), &Default::default()),
    );
}

#[test]
fn path_reset_without_commits_removes_entries() {
    let expected = tempfile::tempdir().unwrap();
    let actual = tempfile::tempdir().unwrap();
    for root in [expected.path(), actual.path()] {
        git(root, &["init", "--quiet", "--template="]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        git(root, &["add", "--all"]);
    }
    git(expected.path(), &["reset", "--quiet", "--", "a.txt"]);
    let repository = Repository::open(actual.path()).unwrap();
    repository
        .reset_paths(None, &paths(&["a.txt"]), &Default::default())
        .unwrap();
    assert_eq!(
        git(actual.path(), &["ls-files", "--stage"]),
        git(expected.path(), &["ls-files", "--stage"])
    );
}

#[test]
fn mixed_keeps_cached_metadata_for_unchanged_entries() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    // Exits 1 because the fixture has modified files; the refresh still records stat data.
    run(root.path(), &["update-index", "-q", "--refresh"]);
    let repository = Repository::open(root.path()).unwrap();
    let before = repository.read_index(Default::default()).unwrap().unwrap();
    let options = ResetOptions::new(committer(), "HEAD");
    repository
        .reset(resolve(root.path(), "HEAD"), ResetMode::Mixed, &options)
        .unwrap();
    let after = repository.read_index(Default::default()).unwrap().unwrap();
    let stat = |index: &girt::index::Index, path: &[u8]| {
        index
            .entries()
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.stat)
    };
    assert_eq!(stat(&after, b"dir/f0.txt"), stat(&before, b"dir/f0.txt"));
    assert_ne!(stat(&after, b"dir/f0.txt"), Some(Default::default()));
}

#[test]
fn soft_reset_refused_with_conflict_stages() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    let blob = git(root.path(), &["rev-parse", "HEAD:a.txt"]);
    let blob = blob.trim();
    let stages = format!(
        "0 {0} 0\ta.txt\n100644 {blob} 1\ta.txt\n100644 {blob} 2\ta.txt\n",
        "0".repeat(40)
    );
    let output = Command::new("git")
        .current_dir(root.path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.path().join("absent-config"))
        .args(["update-index", "--index-info"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(stages.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    assert!(
        !run(root.path(), &["reset", "--quiet", "--soft", "HEAD~1"])
            .status
            .success()
    );
    let before = observe(root.path());
    let repository = Repository::open(root.path()).unwrap();
    let options = ResetOptions::new(committer(), "HEAD~1");
    let result = repository.reset(resolve(root.path(), "HEAD~1"), ResetMode::Soft, &options);
    assert!(
        matches!(result, Err(ResetError::MergeInProgress)),
        "{result:?}"
    );
    assert_eq!(observe(root.path()), before);
}

#[test]
fn unchanged_entries_keep_assume_unchanged() {
    let flag = |root: &Path| {
        git(
            root,
            &["update-index", "--assume-unchanged", "dir/f0.txt", "a.txt"],
        );
    };
    compare(flag, &["reset", "--quiet", "HEAD~1"], |repo, root| {
        let options = ResetOptions::new(committer(), "HEAD~1");
        repo.reset(resolve(root, "HEAD~1"), ResetMode::Mixed, &options)
            .map(drop)
    });
    compare(flag, &["reset", "--quiet", "--", "dir"], |repo, root| {
        repo.reset_paths(tree(root, "HEAD"), &paths(&["dir"]), &Default::default())
    });
}

#[test]
fn path_reset_replaces_a_file_becoming_a_directory() {
    let directory = |root: &Path| {
        git(root, &["stash", "--quiet", "--include-untracked"]);
        git(root, &["checkout", "--quiet", "-b", "nested", "main"]);
        git(root, &["rm", "--quiet", "--cached", "a.txt"]);
        std::fs::remove_file(root.join("a.txt")).unwrap();
        std::fs::create_dir(root.join("a.txt")).unwrap();
        std::fs::write(root.join("a.txt/inner"), "inner\n").unwrap();
        git(root, &["add", "a.txt/inner"]);
        git(root, &["commit", "--quiet", "-m", "nested"]);
        git(root, &["checkout", "--quiet", "-f", "main"]);
    };
    compare(
        directory,
        &["reset", "--quiet", "nested", "--", "a.txt/inner"],
        |repo, root| {
            repo.reset_paths(
                tree(root, "nested"),
                &paths(&["a.txt/inner"]),
                &Default::default(),
            )
        },
    );
}

#[test]
fn commit_reset_refuses_reftable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(
        path,
        &["init", "--quiet", "--ref-format=reftable", "--template="],
    );
    std::fs::write(path.join("a.txt"), "a\n").unwrap();
    git(path, &["add", "a.txt"]);
    git(path, &["commit", "--quiet", "-m", "one"]);
    let repository = Repository::open(path).unwrap();
    let options = ResetOptions::new(committer(), "HEAD");
    let result = repository.reset(resolve(path, "HEAD"), ResetMode::Soft, &options);
    assert!(
        matches!(result, Err(ResetError::Unsupported(_))),
        "{result:?}"
    );
}

/// Hard resets run on Linux and macOS only, with checkout's platform rules.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn hard(root: &Path, repo: &Repository, revision: &str) -> Result<(), ResetError> {
    let options = ResetOptions::new(committer(), revision);
    repo.reset(resolve(root, revision), ResetMode::Hard, &options)
        .map(drop)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn hard_discards_local_changes() {
    // The fixture's staged, unstaged and added files, plus a deleted file and an untracked one.
    let local = |root: &Path| {
        std::fs::remove_file(root.join("dir/f1.txt")).unwrap();
        std::fs::write(root.join("dir/f9.txt"), "untracked\n").unwrap();
    };
    compare(
        local,
        &["reset", "--quiet", "--hard", "HEAD~1"],
        |repo, root| hard(root, repo, "HEAD~1"),
    );
    compare(nothing, &["reset", "--quiet", "--hard"], |repo, root| {
        hard(root, repo, "HEAD")
    });
    // An untracked file where the target has a file is replaced.
    let obstruct = |root: &Path| {
        git(root, &["rm", "--quiet", "--cached", "dir/f2.txt"]);
        std::fs::write(root.join("dir/f2.txt"), "untracked in the way\n").unwrap();
    };
    compare(
        obstruct,
        &["reset", "--quiet", "--hard", "HEAD"],
        |repo, root| hard(root, repo, "HEAD"),
    );
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn hard_resolves_conflicts_and_modes() {
    let conflict = |root: &Path| {
        git(root, &["stash", "--quiet", "--include-untracked"]);
        git(root, &["checkout", "--quiet", "-b", "other", "main~1"]);
        std::fs::write(root.join("a.txt"), "other\n").unwrap();
        git(root, &["update-index", "--chmod=+x", "dir/f0.txt"]);
        git(root, &["commit", "--quiet", "-am", "other"]);
        git(root, &["checkout", "--quiet", "main"]);
        assert!(!run(root, &["merge", "--quiet", "other"]).status.success());
    };
    compare(
        conflict,
        &["reset", "--quiet", "--hard", "other"],
        |repo, root| hard(root, repo, "other"),
    );
}

/// Git deletes an untracked directory in the way; girt refuses before changing anything.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn hard_refuses_untracked_directory_in_the_way() {
    let root = tempfile::tempdir().unwrap();
    fixture(root.path());
    git(root.path(), &["rm", "--quiet", "--cached", "dir/f2.txt"]);
    std::fs::remove_file(root.path().join("dir/f2.txt")).unwrap();
    std::fs::create_dir(root.path().join("dir/f2.txt")).unwrap();
    std::fs::write(root.path().join("dir/f2.txt/keep"), "keep\n").unwrap();
    let repository = Repository::open(root.path()).unwrap();
    let head = git(root.path(), &["rev-parse", "HEAD"]);
    let before = working_files(root.path());
    let result = hard(root.path(), &repository, "HEAD");
    assert!(matches!(result, Err(ResetError::Checkout(_))), "{result:?}");
    assert_eq!(working_files(root.path()), before);
    assert_eq!(git(root.path(), &["rev-parse", "HEAD"]), head);
}

/// An untracked file where the target needs a directory is replaced, as Git replaces it.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn hard_replaces_untracked_file_blocking_a_directory() {
    let block = |root: &Path| {
        git(root, &["stash", "--quiet", "--include-untracked"]);
        git(root, &["rm", "--quiet", "-r", "dir"]);
        git(root, &["commit", "--quiet", "-m", "no dir"]);
        std::fs::write(root.join("dir"), "blocker\n").unwrap();
    };
    compare(
        block,
        &["reset", "--quiet", "--hard", "HEAD~1"],
        |repo, root| hard(root, repo, "HEAD~1"),
    );
}

/// A clean file whose attributes change but whose blob does not is left alone.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn hard_keeps_clean_files_when_only_attributes_change() {
    let attributes = |root: &Path| {
        git(root, &["stash", "--quiet", "--include-untracked"]);
        std::fs::write(root.join(".gitattributes"), "a.txt text eol=lf\n").unwrap();
        git(root, &["add", ".gitattributes"]);
        git(root, &["commit", "--quiet", "-m", "lf"]);
        std::fs::write(root.join(".gitattributes"), "a.txt text eol=crlf\n").unwrap();
        git(root, &["commit", "--quiet", "-am", "crlf"]);
        git(
            root,
            &["checkout", "--quiet", "HEAD~1", "--", ".gitattributes"],
        );
        git(root, &["commit", "--quiet", "-am", "lf again"]);
    };
    compare(
        attributes,
        &["reset", "--quiet", "--hard", "HEAD~1"],
        |repo, root| {
            let commit = resolve(root, "HEAD~1");
            let tree = resolve(root, "HEAD~1^{tree}");
            let mut filters = repo
                .filters_for_checkout(
                    &girt::workflow::Environment::from_process(),
                    tree,
                    girt::filter::Commands::Refuse,
                )
                .unwrap();
            let options = ResetOptions::new(committer(), "HEAD~1");
            repo.reset_hard_with_filters(commit, &options, &mut filters)
                .map(drop)
        },
    );
}

/// On a branch with no commits, a hard reset removes tracked files and keeps untracked ones.
#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn hard_reset_without_commits() {
    let expected = tempfile::tempdir().unwrap();
    let actual = tempfile::tempdir().unwrap();
    for root in [expected.path(), actual.path()] {
        git(root, &["init", "--quiet", "--template="]);
        std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
        git(root, &["add", "staged.txt"]);
        std::fs::write(root.join("untracked.txt"), "untracked\n").unwrap();
    }
    git(expected.path(), &["reset", "--quiet", "--hard"]);
    let repository = Repository::open(actual.path()).unwrap();
    repository
        .reset_unborn(ResetMode::Hard, &Default::default())
        .unwrap();
    assert_eq!(working_files(actual.path()), working_files(expected.path()));
    assert_eq!(
        git(actual.path(), &["ls-files", "--stage"]),
        git(expected.path(), &["ls-files", "--stage"])
    );
}
