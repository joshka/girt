//! Status with filters, compared with `git status` on repositories using line-ending conversion,
//! `ident`, clean filters and Git LFS.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::Repository;
use girt::filter::Commands;
use girt::status::{Baseline, Comparison, Error, Limits, UntrackedFiles};
use girt::workflow::Environment;
use pack_git::git;

fn environment() -> Environment {
    Environment::default().with("GIT_ATTR_NOSYSTEM", "1")
}

/// Paths `git status` reports as changed in the working tree.
fn git_unstaged(root: &Path) -> Vec<String> {
    let output = git(
        root,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
        b"",
    );
    String::from_utf8(output)
        .unwrap()
        .lines()
        .filter(|line| line.as_bytes().get(1).is_some_and(|byte| *byte != b' '))
        .map(|line| line[3..].to_owned())
        .collect()
}

fn ours(root: &Path, commands: Commands) -> Result<Vec<String>, Error> {
    let repo = Repository::open(root).unwrap();
    let mut filters = repo.filters(&environment(), commands).unwrap();
    let rules = repo.ignore_rules(&environment()).unwrap();
    let report = repo.status_with_filters(
        Baseline::Head,
        UntrackedFiles::Omit,
        &rules,
        &girt::pathspec::Pathspec::all(),
        &mut filters,
        Limits::default(),
        &AtomicBool::new(false),
    )?;
    assert_eq!(report.comparison, Comparison::ConvertedContent);
    Ok(report
        .unstaged
        .iter()
        .map(|change| String::from_utf8(change.path.clone()).unwrap())
        .collect())
}

fn raw(root: &Path) -> Vec<String> {
    let repo = Repository::open(root).unwrap();
    let rules = repo.ignore_rules(&environment()).unwrap();
    repo.status(Baseline::Head, UntrackedFiles::Omit, &rules)
        .unwrap()
        .unstaged
        .iter()
        .map(|change| String::from_utf8(change.path.clone()).unwrap())
        .collect()
}

/// A committed repository with `config`, `.gitattributes` and `files`, checked out by Git.
fn repository(root: &Path, config: &[(&str, &str)], attributes: &str, files: &[(&str, &[u8])]) {
    git(root, &["init", "-q", "."], b"");
    git(root, &["config", "core.attributesFile", "absent"], b"");
    for (key, value) in config {
        git(root, &["config", key, value], b"");
    }
    std::fs::write(root.join(".gitattributes"), attributes).unwrap();
    for (name, bytes) in files {
        std::fs::write(root.join(name), bytes).unwrap();
    }
    git(root, &["add", "."], b"");
    git(root, &["commit", "-q", "-m", "init"], b"");
    // Rewrite every file through Git's smudge path, as a checkout leaves it.
    for (name, _) in files {
        std::fs::remove_file(root.join(name)).unwrap();
    }
    git(root, &["checkout", "--", "."], b"");
}

/// Ages the index so entries are not racily clean, then changes each file's mtime so its stat
/// words no longer match and status must read it.
fn touch_after_index(root: &Path, names: &[&str]) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(10);
    let index = std::fs::File::options()
        .write(true)
        .open(root.join(".git/index"))
        .unwrap();
    index.set_modified(old).unwrap();
    for name in names {
        let file = std::fs::File::options()
            .write(true)
            .open(root.join(name))
            .unwrap();
        file.set_modified(old - std::time::Duration::from_secs(10))
            .unwrap();
    }
}

#[test]
fn checked_out_crlf_files_are_clean() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let files: &[(&str, &[u8])] = &[("a.txt", b"a\nb\n"), ("b.txt", b"c\n")];
    repository(root, &[("core.autocrlf", "true")], "", files);
    assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"a\r\nb\r\n");
    touch_after_index(root, &["a.txt", "b.txt"]);
    assert_eq!(git_unstaged(root), Vec::<String>::new());
    assert_eq!(ours(root, Commands::Refuse).unwrap(), git_unstaged(root));
    // Literal bytes differ from the LF blobs.
    assert_eq!(raw(root), ["a.txt", "b.txt"]);
}

#[test]
fn line_ending_only_changes_follow_git() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let files: &[(&str, &[u8])] = &[
        ("resized.txt", b"a\nb\n"),
        ("same.txt", b"a\nb\n"),
        ("ident.txt", b"$Id$\n"),
        ("edited.txt", b"x\n"),
    ];
    repository(
        root,
        &[("core.autocrlf", "input")],
        "ident.txt ident\n",
        files,
    );
    // Only the line endings change, so the size does too: Git reports it modified.
    std::fs::write(root.join("resized.txt"), b"a\r\nb\r\n").unwrap();
    std::fs::write(root.join("edited.txt"), b"y\n").unwrap();
    touch_after_index(
        root,
        &["resized.txt", "same.txt", "ident.txt", "edited.txt"],
    );
    let expected = git_unstaged(root);
    assert_eq!(expected, ["edited.txt", "resized.txt"]);
    assert_eq!(ours(root, Commands::Refuse).unwrap(), expected);
}

#[test]
fn attributes_added_after_commit_make_files_dirty() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root, &[], "", &[("w.txt", b"a\r\nb\r\n")]);
    std::fs::write(root.join(".gitattributes"), "*.txt text\n").unwrap();
    touch_after_index(root, &["w.txt"]);
    let expected = git_unstaged(root);
    assert_eq!(expected, [".gitattributes", "w.txt"]);
    assert_eq!(ours(root, Commands::Refuse).unwrap(), expected);
}

#[test]
fn clean_filter_decides_same_size_changes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let config = [
        ("filter.up.clean", "tr a-z A-Z"),
        ("filter.up.smudge", "cat"),
    ];
    let files: &[(&str, &[u8])] = &[("a.u", b"hello\n"), ("b.u", b"world\n")];
    repository(root, &config, "*.u filter=up\n", files);
    // Same size and the same cleaned content: clean. Same size but different content: modified.
    std::fs::write(root.join("a.u"), b"hellO\n").unwrap();
    std::fs::write(root.join("b.u"), b"word!\n").unwrap();
    touch_after_index(root, &["a.u", "b.u"]);
    let expected = git_unstaged(root);
    assert_eq!(expected, ["b.u"]);
    assert_eq!(ours(root, Commands::Run).unwrap(), expected);
    let error = ours(root, Commands::Refuse).unwrap_err();
    assert!(matches!(error, Error::Filter { .. }), "{error:?}");
}

#[test]
fn git_lfs_files_are_clean() {
    let installed = std::process::Command::new("git-lfs")
        .arg("version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !installed {
        eprintln!("git-lfs is not installed; skipping");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let config = [
        ("filter.lfs.process", "git-lfs filter-process"),
        ("filter.lfs.required", "true"),
    ];
    let content: Vec<u8> = (0..100_000u32).map(|n| (n * 7 % 251) as u8).collect();
    let files: &[(&str, &[u8])] = &[("a.bin", &content), ("b.bin", b"other\n")];
    repository(root, &config, "*.bin filter=lfs -text\n", files);
    let mut changed = content.clone();
    changed[0] ^= 1;
    std::fs::write(root.join("b.bin"), b"OTHER\n").unwrap();
    touch_after_index(root, &["a.bin", "b.bin"]);
    let expected = git_unstaged(root);
    assert_eq!(expected, ["b.bin"]);
    assert_eq!(ours(root, Commands::Run).unwrap(), expected);
}
