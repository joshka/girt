//! The long-running `filter.<name>.process` protocol, compared with Git using a test filter and,
//! when installed, Git LFS.
#![cfg(unix)]
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use std::path::Path;

use girt::filter::{Commands, DriverError, Error, Pipeline, Staged};
use girt::workflow::Environment;
use girt::{ObjectId, Repository};
use pack_git::{git, git_output};

fn filter_script() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/filter-process.pl")
        .display()
        .to_string()
}

fn repository(root: &Path, log: &str, required: bool) {
    git(root, &["init", "-q", "."], b"");
    git(root, &["config", "core.attributesFile", "absent"], b"");
    let command = format!("perl '{}' '{log}'", filter_script());
    git(root, &["config", "filter.case.process", &command], b"");
    // A one-shot command that must not run: the process takes precedence.
    git(root, &["config", "filter.case.clean", "false"], b"");
    if required {
        git(root, &["config", "filter.case.required", "true"], b"");
    }
    std::fs::write(
        root.join(".gitattributes"),
        "* filter=case\n.gitattributes -filter\n",
    )
    .unwrap();
}

fn filters(root: &Path) -> Pipeline {
    let environment = Environment::default().with("GIT_ATTR_NOSYSTEM", "1");
    Repository::open(root)
        .unwrap()
        .filters(&environment, Commands::Run)
        .unwrap()
}

const FILES: &[(&str, &[u8])] = &[
    ("a.txt", b"hello\n"),
    ("error.txt", b"keep\n"),
    ("late-error.txt", b"late\n"),
    ("empty", b""),
];

#[test]
fn clean_requests_match_git_add() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root, "git.log", false);
    let large: Vec<u8> = (0..200_000u32).map(|n| b'a' + (n % 26) as u8).collect();
    let mut files = FILES.to_vec();
    files.push(("large", &large));
    for (name, bytes) in &files {
        std::fs::write(root.join(name), bytes).unwrap();
    }
    let mut args = vec!["add", "--"];
    args.extend(files.iter().map(|(name, _)| *name));
    let output = git_output(root, &args, b"");
    assert!(output.status.success());

    let mut filters = filters_logging(root, "girt.log");
    for (name, bytes) in &files {
        let expected = git(root, &["cat-file", "blob", &format!(":{name}")], b"");
        let ours = filters
            .to_repository(name.as_bytes(), bytes, Staged::Absent)
            .unwrap();
        assert_eq!(*ours.bytes, *expected, "{name}");
        let failed = name.contains("error");
        assert_eq!(ours.skipped.is_some(), failed, "{name}");
    }
    drop(filters);
    assert_eq!(
        sorted(&root.join("girt.log")),
        sorted(&root.join("git.log"))
    );
}

#[test]
fn smudge_requests_match_git_checkout_index() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root, "git.log", false);
    let mut ids = Vec::new();
    for (name, bytes) in FILES {
        let id = git(
            root,
            &["hash-object", "-w", "--no-filters", "--stdin"],
            bytes,
        );
        let id = String::from_utf8(id).unwrap().trim().to_owned();
        let info = format!("100644,{id},{name}");
        git(root, &["update-index", "--add", "--cacheinfo", &info], b"");
        ids.push(id.parse::<ObjectId>().unwrap());
    }
    let mut args = vec!["checkout-index", "-f", "--"];
    args.extend(FILES.iter().map(|(name, _)| *name));
    git(root, &args, b"");

    let mut filters = filters_logging(root, "girt.log");
    for ((name, bytes), id) in FILES.iter().zip(ids) {
        let written = std::fs::read(root.join(name)).unwrap();
        let ours = filters.to_worktree(name.as_bytes(), bytes, id).unwrap();
        assert_eq!(*ours.bytes, *written, "{name}");
    }
    drop(filters);
    // Requests carry `blob=<id>`; girt never offers `delay`, so neither sends `can-delay`.
    assert_eq!(
        sorted(&root.join("girt.log")),
        sorted(&root.join("git.log"))
    );
}

#[test]
fn abort_stops_the_direction_and_required_fails() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    repository(root, "git.log", true);
    std::fs::write(root.join("abort.txt"), b"x\n").unwrap();
    let output = git_output(root, &["add", "abort.txt"], b"");
    assert_eq!(output.status.code(), Some(128));

    let mut filters = filters_logging(root, "girt.log");
    let error = filters
        .to_repository(b"abort.txt", b"x\n", Staged::Absent)
        .unwrap_err();
    assert!(
        matches!(
            error,
            Error::Driver {
                source: DriverError::Aborted { .. },
                ..
            }
        ),
        "{error:?}"
    );
    // After an abort, further clean requests are not sent.
    let error = filters
        .to_repository(b"a.txt", b"x\n", Staged::Absent)
        .unwrap_err();
    assert!(matches!(error, Error::Driver { .. }), "{error:?}");
    let id = ObjectId::null(girt::ObjectFormat::Sha1);
    let smudged = filters.to_worktree(b"a.txt", b"X\n", id).unwrap();
    assert_eq!(*smudged.bytes, *b"x\n");
    drop(filters);
    let log = std::fs::read_to_string(root.join("girt.log")).unwrap();
    assert_eq!(log.lines().count(), 2, "{log}");
}

/// A pipeline whose test filter logs to `log` instead of the file Git used.
fn filters_logging(root: &Path, log: &str) -> Pipeline {
    let command = format!("perl '{}' '{log}'", filter_script());
    git(root, &["config", "filter.case.process", &command], b"");
    filters(root)
}

fn sorted(path: &Path) -> Vec<String> {
    let mut lines: Vec<String> = std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

#[test]
fn git_lfs_round_trip_matches_git() {
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
    let content: Vec<u8> = (0..300_000u32).map(|n| (n * 7 % 251) as u8).collect();
    std::fs::write(root.join("a.bin"), &content).unwrap();
    git(root, &["add", "a.bin"], b"");
    let pointer = git(root, &["cat-file", "blob", ":a.bin"], b"");
    assert!(pointer.starts_with(b"version https://git-lfs.github.com/spec/v1\n"));

    let mut filters = filters(root);
    let stored = filters
        .to_repository(b"a.bin", &content, Staged::Absent)
        .unwrap();
    assert_eq!(*stored.bytes, *pointer);
    let id = git(root, &["rev-parse", ":a.bin"], b"");
    let id: ObjectId = String::from_utf8(id).unwrap().trim().parse().unwrap();
    let written = filters.to_worktree(b"a.bin", &pointer, id).unwrap();
    assert!(*written.bytes == *content, "smudged content differs");
}

#[test]
fn broken_handshake_does_not_hang() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    git(&root, &["init", "-q", "."], b"");
    git(&root, &["config", "core.attributesFile", "absent"], b"");
    // An invalid pkt-line header followed by more output than a pipe buffer holds.
    let command = "printf zzzz; head -c 1000000 /dev/zero; sleep 30";
    git(&root, &["config", "filter.bad.process", command], b"");
    std::fs::write(root.join(".gitattributes"), "* filter=bad\n").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut filters = filters(&root);
        let result = filters.to_repository(b"a", b"a\n", Staged::Absent);
        drop(filters);
        sender
            .send(result.map(|converted| converted.skipped.is_some()))
            .unwrap();
    });
    let result = receiver
        .recv_timeout(std::time::Duration::from_secs(20))
        .expect("filter process cleanup hung");
    // Not required: the failure is reported and the content is kept.
    assert!(result.unwrap());
}
