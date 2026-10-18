//! Checkout with filters, compared byte for byte with `git checkout` in a twin clone.
#![cfg(any(target_os = "linux", target_os = "macos"))]
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::attributes::DirectorySource;
use girt::checkout::{Error, Limits};
use girt::filter::Commands;
use girt::workflow::Environment;
use girt::{ObjectId, Repository};
use pack_git::git;

fn environment() -> Environment {
    Environment::default().with("GIT_ATTR_NOSYSTEM", "1")
}

const ATTRIBUTES: &str = "*.txt text\n*.bat text eol=crlf\n*.id ident\n*.u filter=up\n\
    *.bin -text\nauto/* text=auto\n";

/// A source repository with two commits, built without conversion.
fn source(root: &Path) -> (ObjectId, ObjectId) {
    git(root, &["init", "-q", "-b", "main", "."], b"");
    let first: &[(&str, &[u8])] = &[
        (".gitattributes", ATTRIBUTES.as_bytes()),
        ("a.txt", b"one\ntwo\n"),
        ("run.bat", b"echo\n"),
        ("v.id", b"$Id$\n"),
        ("x.u", b"HELLO\n"),
        ("data.bin", b"\0a\nb\n"),
        ("auto/lf", b"lf\n"),
        ("auto/crlf", b"crlf\r\n"),
        ("link", b"a.txt"),
    ];
    for (name, bytes) in first {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        if *name == "link" {
            std::os::unix::fs::symlink("a.txt", &path).unwrap();
        } else {
            std::fs::write(path, bytes).unwrap();
        }
    }
    git(root, &["add", "."], b"");
    git(root, &["commit", "-q", "-m", "one"], b"");
    let one = rev(root, "HEAD^{tree}");
    std::fs::write(root.join("a.txt"), b"one\nthree\n").unwrap();
    std::fs::write(root.join("x.u"), b"BYE\n").unwrap();
    std::fs::remove_file(root.join("run.bat")).unwrap();
    git(root, &["add", "-A"], b"");
    git(root, &["commit", "-q", "-m", "two"], b"");
    (one, rev(root, "HEAD^{tree}"))
}

fn rev(root: &Path, revision: &str) -> ObjectId {
    let id = git(root, &["rev-parse", revision], b"");
    String::from_utf8(id).unwrap().trim().parse().unwrap()
}

fn clone(source: &Path, destination: &Path, config: &[(&str, &str)]) {
    let parent = destination.parent().unwrap();
    let args = [
        "clone",
        "-q",
        "-n",
        source.to_str().unwrap(),
        destination.to_str().unwrap(),
    ];
    git(parent, &args, b"");
    git(
        destination,
        &["config", "core.attributesFile", "absent"],
        b"",
    );
    git(
        destination,
        &["config", "filter.up.clean", "tr a-z A-Z"],
        b"",
    );
    git(
        destination,
        &["config", "filter.up.smudge", "tr A-Z a-z"],
        b"",
    );
    for (key, value) in config {
        git(destination, &["config", key, value], b"");
    }
}

/// Every working-tree file except `.git`, with symlinks as their targets.
fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let name = path
                .strip_prefix(root)
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let kind = entry.file_type().unwrap();
            if name == ".git" {
                continue;
            } else if kind.is_dir() {
                pending.push(path);
            } else if kind.is_symlink() {
                let target = std::fs::read_link(&path).unwrap();
                found.insert(name, target.to_str().unwrap().as_bytes().to_vec());
            } else {
                found.insert(name, std::fs::read(&path).unwrap());
            }
        }
    }
    found
}

fn girt_checkout(
    root: &Path,
    baseline: Option<ObjectId>,
    target: ObjectId,
    commands: Commands,
) -> Result<girt::checkout::Report, girt::checkout::Failure> {
    let repo = Repository::open(root).unwrap();
    let mut current = repo.filters(&environment(), commands).unwrap();
    let mut filters = repo
        .filters_with_source(&environment(), DirectorySource::Tree(target), commands)
        .unwrap();
    repo.checkout_tree_with_filters(
        baseline,
        Some(target),
        &mut current,
        &mut filters,
        Limits::default(),
        &AtomicBool::new(false),
    )
}

#[test]
fn checkout_matches_git_checkout() {
    for config in [
        &[][..],
        &[("core.autocrlf", "true")],
        &[("core.autocrlf", "input")],
        &[("core.eol", "crlf")],
    ] {
        let directory = tempfile::tempdir().unwrap();
        let src = directory.path().join("src");
        std::fs::create_dir(&src).unwrap();
        let (one, two) = source(&src);
        let (theirs, ours) = (directory.path().join("git"), directory.path().join("girt"));
        clone(&src, &theirs, config);
        clone(&src, &ours, config);

        git(&theirs, &["checkout", "-q", "main~1"], b"");
        girt_checkout(&ours, None, one, Commands::Run).unwrap();
        assert_eq!(files(&ours), files(&theirs), "{config:?} first");
        // Git sees girt's files as clean.
        git(
            &ours,
            &["update-ref", "HEAD", &rev(&src, "main~1").to_string()],
            b"",
        );
        let status = git(&ours, &["status", "--porcelain"], b"");
        assert_eq!(String::from_utf8_lossy(&status), "", "{config:?}");

        // A second checkout verifies the converted baseline before replacing it.
        git(&theirs, &["checkout", "-q", "main"], b"");
        girt_checkout(&ours, Some(one), two, Commands::Run).unwrap();
        assert_eq!(files(&ours), files(&theirs), "{config:?} second");
    }
}

#[test]
fn baseline_is_verified_with_its_own_attributes() {
    let directory = tempfile::tempdir().unwrap();
    let src = directory.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main", "."], b"");
    std::fs::write(src.join(".gitattributes"), "f text eol=lf\n").unwrap();
    std::fs::write(src.join("f"), b"one\n").unwrap();
    git(&src, &["add", "."], b"");
    git(&src, &["commit", "-q", "-m", "lf"], b"");
    let one = rev(&src, "HEAD^{tree}");
    std::fs::write(src.join(".gitattributes"), "f text eol=crlf\n").unwrap();
    std::fs::write(src.join("f"), b"two\n").unwrap();
    git(&src, &["add", "."], b"");
    git(&src, &["commit", "-q", "-m", "crlf"], b"");
    let two = rev(&src, "HEAD^{tree}");
    let (theirs, ours) = (directory.path().join("git"), directory.path().join("girt"));
    clone(&src, &theirs, &[]);
    clone(&src, &ours, &[]);
    git(&theirs, &["checkout", "-q", "main~1"], b"");
    git(&theirs, &["checkout", "-q", "main"], b"");
    girt_checkout(&ours, None, one, Commands::Run).unwrap();
    girt_checkout(&ours, Some(one), two, Commands::Run).unwrap();
    assert_eq!(files(&ours), files(&theirs));
    assert_eq!(std::fs::read(ours.join("f")).unwrap(), b"two\r\n");
}

#[test]
fn locally_modified_converted_file_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let src = directory.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let (one, two) = source(&src);
    let ours = directory.path().join("girt");
    clone(&src, &ours, &[("core.autocrlf", "true")]);
    girt_checkout(&ours, None, one, Commands::Run).unwrap();
    // Raw LF content equals the blob but not the CRLF file Git would write.
    std::fs::write(ours.join("a.txt"), b"one\ntwo\n").unwrap();
    let failure = girt_checkout(&ours, Some(one), two, Commands::Run).unwrap_err();
    assert!(
        matches!(*failure.cause, Error::Refused { .. }),
        "{:?}",
        failure.cause
    );
    assert!(failure.report.applied.is_empty());
}

#[test]
fn refused_commands_change_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let src = directory.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let (one, _) = source(&src);
    let ours = directory.path().join("girt");
    clone(&src, &ours, &[]);
    let failure = girt_checkout(&ours, None, one, Commands::Refuse).unwrap_err();
    assert!(
        matches!(*failure.cause, Error::Filter { .. }),
        "{:?}",
        failure.cause
    );
    assert!(failure.report.applied.is_empty());
    assert_eq!(files(&ours), BTreeMap::new());
}

#[test]
fn git_lfs_checkout_writes_content() {
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
    git(root, &["init", "-q", "."], b"");
    git(root, &["config", "core.attributesFile", "absent"], b"");
    git(
        root,
        &["config", "filter.lfs.process", "git-lfs filter-process"],
        b"",
    );
    git(root, &["config", "filter.lfs.required", "true"], b"");
    std::fs::write(root.join(".gitattributes"), "*.bin filter=lfs -text\n").unwrap();
    let content: Vec<u8> = (0..200_000u32).map(|n| (n * 13 % 251) as u8).collect();
    std::fs::write(root.join("a.bin"), &content).unwrap();
    git(root, &["add", "."], b"");
    git(root, &["commit", "-q", "-m", "lfs"], b"");
    let tree = rev(root, "HEAD^{tree}");
    // Empty the working tree and index, keeping the LFS object store.
    git(root, &["rm", "-q", "--cached", "-r", "."], b"");
    std::fs::remove_file(root.join("a.bin")).unwrap();
    std::fs::remove_file(root.join(".gitattributes")).unwrap();
    std::fs::remove_file(root.join(".git/index")).unwrap();

    girt_checkout(root, None, tree, Commands::Run).unwrap();
    assert!(std::fs::read(root.join("a.bin")).unwrap() == content);
    let status = git(root, &["status", "--porcelain"], b"");
    assert_eq!(String::from_utf8_lossy(&status), "");
}
