//! rerere compatibility with the Git executable: resolutions recorded by one tool replay in the
//! other.
#![cfg(any(target_os = "linux", target_os = "macos"))]

#[path = "support/rerere_git.rs"]
mod support;

use std::path::Path;
use std::time::{Duration, SystemTime};

use girt::Repository;
use girt::rerere::{self, Rerere};
use support::{git, run};

/// A repository where merging `side` into `main` conflicts in `f` (and `g` when `two`).
fn conflicting(style: &str) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(path, &["init", "-q", "-b", "main", "--template=", "."]);
    git(path, &["config", "merge.conflictStyle", style]);
    let write = |name: &str, text: &str| std::fs::write(path.join(name), text).unwrap();

    write("f", "a\nb\nc\nd\ne\nf\ng\n");
    write("g", "1\n2\n3\n");
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "base"]);
    git(path, &["checkout", "-q", "-b", "side"]);
    write("f", "a\nSIDE\nc\nd\ne\nf\nS2\n");
    write("g", "1\nTHEIRS\n3\n");
    git(path, &["commit", "-q", "-am", "side"]);
    git(path, &["checkout", "-q", "main"]);
    write("f", "a\nMAIN\nc\nd\ne\nf\nM2\n");
    write("g", "1\nOURS\n3\n");
    git(path, &["commit", "-q", "-am", "main"]);
    root
}

/// Merges `side` without Git's rerere, leaving conflict markers.
fn merge_without_rerere(path: &Path) {
    let output = run(path, &["-c", "rerere.enabled=false", "merge", "side"]);
    assert!(!output.status.success(), "merge should conflict");
}

fn read(path: &Path, name: &str) -> String {
    std::fs::read_to_string(path.join(name)).unwrap()
}

fn paths(names: &[&str]) -> Vec<Vec<u8>> {
    names.iter().map(|n| n.as_bytes().to_vec()).collect()
}

#[test]
fn girt_replays_a_resolution_git_recorded() {
    for style in ["merge", "diff3", "zdiff3"] {
        let root = conflicting(style);
        let path = root.path();
        git(path, &["config", "rerere.enabled", "true"]);

        // Git records the conflict and, on commit, the resolution.
        assert!(!run(path, &["merge", "side"]).status.success());
        std::fs::write(path.join("f"), "a\nRESOLVED\nc\nd\ne\nf\nBOTH\n").unwrap();
        std::fs::write(path.join("g"), "1\nOURS+THEIRS\n3\n").unwrap();
        git(path, &["add", "f", "g"]);
        git(path, &["commit", "-q", "--no-edit"]);
        git(path, &["reset", "-q", "--hard", "HEAD~1"]);

        merge_without_rerere(path);
        let repo = Repository::open(path).unwrap();
        assert!(rerere::is_enabled(&repo).unwrap());
        let mut rerere = Rerere::open(&repo).unwrap();
        let report = rerere.run(&paths(&["f", "g"])).unwrap();

        assert_eq!(report.replayed, paths(&["f", "g"]), "{style}");
        assert_eq!(read(path, "f"), "a\nRESOLVED\nc\nd\ne\nf\nBOTH\n");
        assert_eq!(read(path, "g"), "1\nOURS+THEIRS\n3\n");
        assert!(rerere.tracked().is_empty());
    }
}

#[test]
fn git_replays_a_resolution_girt_recorded() {
    let root = conflicting("merge");
    let path = root.path();
    git(path, &["config", "rerere.enabled", "true"]);
    merge_without_rerere(path);

    let repo = Repository::open(path).unwrap();
    let mut rerere = Rerere::open(&repo).unwrap();
    let report = rerere.run(&paths(&["f", "g"])).unwrap();
    assert_eq!(report.recorded_conflicts, paths(&["f", "g"]));
    assert_eq!(git(path, &["rerere", "status"]), "f\ng");
    assert_eq!(rerere.remaining().unwrap(), paths(&["f", "g"]));

    std::fs::write(path.join("f"), "a\nMINE\nc\nd\ne\nf\nTOO\n").unwrap();
    let report = rerere.run(&[]).unwrap();
    assert_eq!(report.recorded_resolutions, paths(&["f"]));
    assert_eq!(git(path, &["rerere", "status"]), "g");

    // Git sees the remaining conflict and replays girt's resolution in a fresh merge.
    git(path, &["checkout", "-q", "-f", "main"]);
    std::fs::remove_file(path.join(".git/MERGE_RR")).ok();
    let output = run(path, &["merge", "side"]);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Resolved 'f' using previous resolution."),
        "{stderr}"
    );
    assert_eq!(read(path, "f"), "a\nMINE\nc\nd\ne\nf\nTOO\n");
}

#[test]
fn context_changes_still_replay_and_variants_are_separate() {
    let root = conflicting("merge");
    let path = root.path();
    git(path, &["config", "rerere.enabled", "true"]);
    merge_without_rerere(path);
    let repo = Repository::open(path).unwrap();
    let mut rerere = Rerere::open(&repo).unwrap();
    rerere.run(&paths(&["g"])).unwrap();
    std::fs::write(path.join("g"), "1\nPICKED\n3\n").unwrap();
    rerere.run(&[]).unwrap();

    // The same conflict with different surrounding lines replays through a three-way merge.
    git(path, &["checkout", "-q", "-f", "main"]);
    std::fs::write(path.join("g"), "0\n1\nOURS\n3\n").unwrap();
    git(path, &["commit", "-q", "-am", "context"]);
    merge_without_rerere(path);
    let mut rerere = Rerere::open(&repo).unwrap();
    let report = rerere.run(&paths(&["g"])).unwrap();
    assert_eq!(report.replayed, paths(&["g"]));
    assert_eq!(read(path, "g"), "0\n1\nPICKED\n3\n");

    // Git reads girt's files: its own `rerere diff` and `status` agree there is nothing left.
    assert_eq!(git(path, &["rerere", "status"]), "");
}

#[test]
fn auto_update_stages_replayed_files() {
    let root = conflicting("merge");
    let path = root.path();
    git(path, &["config", "rerere.enabled", "true"]);
    git(path, &["config", "rerere.autoUpdate", "true"]);
    assert!(!run(path, &["merge", "side"]).status.success());
    std::fs::write(path.join("g"), "1\nKEPT\n3\n").unwrap();
    git(path, &["add", "g"]);
    git(path, &["rerere"]);
    git(path, &["checkout", "-q", "-f", "main"]);
    std::fs::remove_file(path.join(".git/MERGE_RR")).ok();

    merge_without_rerere(path);
    let repo = Repository::open(path).unwrap();
    let mut rerere = Rerere::open(&repo).unwrap();
    let report = rerere.run(&paths(&["f", "g"])).unwrap();
    assert_eq!(report.staged, paths(&["g"]));
    let status = git(path, &["status", "--porcelain"]);
    assert!(status.contains("M  g"), "{status}");
    assert!(status.contains("UU f"), "{status}");
}

#[test]
fn forget_and_gc_follow_git() {
    let root = conflicting("merge");
    let path = root.path();
    git(path, &["config", "rerere.enabled", "true"]);
    assert!(!run(path, &["merge", "side"]).status.success());
    std::fs::write(path.join("g"), "1\nFIRST\n3\n").unwrap();
    git(path, &["rerere"]);

    let repo = Repository::open(path).unwrap();
    let mut rerere = Rerere::open(&repo).unwrap();
    assert_eq!(rerere.forget(&paths(&["g"])).unwrap(), paths(&["g"]));
    // Git now sees `g` as unresolved again, with no recorded resolution.
    assert_eq!(git(path, &["rerere", "status"]), "f\ng");

    // Unresolved entries expire after 15 days, resolved ones after 60, as Git counts them.
    let later = SystemTime::now() + Duration::from_secs(16 * 24 * 60 * 60);
    let removed = rerere::gc(&repo, later).unwrap().removed;
    assert_eq!(removed.len(), 2);
    assert_eq!(
        std::fs::read_dir(path.join(".git/rr-cache"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn gc_expires_variants_separately() {
    let root = conflicting("merge");
    let path = root.path();
    let directory = path.join(".git/rr-cache/27d08dca9e6e45868a32cd28625ad61784a53d0e");
    std::fs::create_dir_all(&directory).unwrap();
    for name in ["preimage", "postimage", "preimage.1", "postimage.1"] {
        std::fs::write(directory.join(name), "x\n").unwrap();
    }
    let old = SystemTime::now() - Duration::from_secs(70 * 24 * 60 * 60);
    std::fs::File::options()
        .write(true)
        .open(directory.join("postimage"))
        .unwrap()
        .set_modified(old)
        .unwrap();

    let repo = Repository::open(path).unwrap();
    let removed = rerere::gc(&repo, SystemTime::now()).unwrap().removed;
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].variant, 0);
    assert!(directory.join("postimage.1").exists());
    assert!(!directory.join("postimage").exists());
}

#[test]
fn a_new_resolution_replays_onto_an_already_tracked_conflict() {
    // `f` and `g` carry the same conflict; resolving `f` lets the next run resolve `g`.
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(path, &["init", "-q", "-b", "main", "--template=", "."]);
    git(path, &["config", "rerere.enabled", "true"]);
    for (branch, text) in [("main", "b"), ("side", "SIDE"), ("main", "MAIN")] {
        if branch == "side" {
            git(path, &["checkout", "-q", "-b", "side"]);
        } else if text == "MAIN" {
            git(path, &["checkout", "-q", "main"]);
        }
        for name in ["f", "g"] {
            std::fs::write(path.join(name), format!("a\n{text}\nc\n")).unwrap();
        }
        git(path, &["add", "."]);
        git(path, &["commit", "-q", "-m", text]);
    }
    merge_without_rerere(path);

    let repo = Repository::open(path).unwrap();
    let mut rerere = Rerere::open(&repo).unwrap();
    rerere.run(&paths(&["f", "g"])).unwrap();
    std::fs::write(path.join("f"), "a\nBOTH\nc\n").unwrap();
    let report = rerere.run(&paths(&["f", "g"])).unwrap();

    assert_eq!(report.recorded_resolutions, paths(&["f"]));
    assert_eq!(report.replayed, paths(&["g"]));
    assert_eq!(read(path, "g"), "a\nBOTH\nc\n");
}

#[test]
fn auto_update_stages_the_cleaned_content() {
    let root = conflicting("merge");
    let path = root.path();
    git(path, &["config", "rerere.enabled", "true"]);
    git(path, &["config", "rerere.autoUpdate", "true"]);
    git(path, &["config", "core.autocrlf", "true"]);
    assert!(!run(path, &["merge", "side"]).status.success());
    std::fs::write(path.join("g"), "1\r\nKEPT\r\n3\r\n").unwrap();
    git(path, &["add", "g"]);
    git(path, &["rerere"]);
    git(path, &["checkout", "-q", "-f", "main"]);

    merge_without_rerere(path);
    let repo = Repository::open(path).unwrap();
    let report = Rerere::open(&repo).unwrap().run(&paths(&["g"])).unwrap();
    assert_eq!(report.staged, paths(&["g"]));
    assert_eq!(git(path, &["show", ":g"]), "1\nKEPT\n3");
}

#[test]
fn conflicted_symlinks_are_never_followed() {
    let root = conflicting("merge");
    let path = root.path();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target");
    std::fs::write(&target, "a\n<<<<<<< x\nA\n=======\nB\n>>>>>>> y\n").unwrap();
    std::os::unix::fs::symlink(&target, path.join("link")).unwrap();

    let repo = Repository::open(path).unwrap();
    let report = Rerere::open(&repo).unwrap().run(&paths(&["link"])).unwrap();
    assert!(report.recorded_conflicts.is_empty());
    assert!(!path.join(".git/MERGE_RR").exists());
}
