//! `Repository::push` excludes history the receiver advertises, checked against Git receivers.
//!
//! Destinations keep received packs (`receive.unpackLimit=1`), so the objects a push sent are the
//! entries of the pack index files it added, as listed by `git show-index`.
#[cfg(feature = "http")]
#[path = "support/http_git.rs"]
mod http_git;

use std::path::Path;
use std::process::Command;

use girt::push::{ForcePolicy, PushCommand};
use girt::refs::RefName;
use girt::transfer::{Environment, NoCallbacks, PushOptions};
use girt::{ObjectId, Repository};

/// Commits in the destination's history before each push.
const HISTORY: usize = 30;

fn git(directory: &Path, args: &[&str]) -> String {
    git_at(directory, args, "@1700000000 +0000")
}

fn git_at(directory: &Path, args: &[&str], date: &str) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Push Author")
        .env("GIT_AUTHOR_EMAIL", "push@example.com")
        .env("GIT_COMMITTER_NAME", "Push Author")
        .env("GIT_COMMITTER_EMAIL", "push@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .args(args)
        .output()
        .expect("Git required for push fixtures");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Commits a new file on the current branch with a distinct, increasing committer date.
fn commit(work: &Path, n: usize) -> ObjectId {
    let name = format!("file-{n}");
    std::fs::write(work.join(&name), format!("content {n}\n")).unwrap();
    git(work, &["add", &name]);
    let date = format!("@{} +0000", 1_700_000_000 + n * 60);
    git_at(work, &["commit", "-q", "-m", &name], &date);
    git(work, &["rev-parse", "HEAD"]).parse().unwrap()
}

struct Pair {
    _root: tempfile::TempDir,
    work: std::path::PathBuf,
    dest: std::path::PathBuf,
}

/// A source worktree whose `main` has [`HISTORY`] commits and a bare destination, optionally
/// holding `main`, with `origin` configured as `url` (the destination path by default).
fn pair(seed_destination: bool) -> Pair {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let dest = root.path().join("dest.git");
    std::fs::create_dir(&work).unwrap();
    std::fs::create_dir(&dest).unwrap();
    git(&work, &["init", "-q", "--initial-branch=main", "."]);
    git(
        &dest,
        &["init", "-q", "--bare", "--initial-branch=main", "."],
    );
    git(&dest, &["config", "receive.unpackLimit", "1"]);
    git(&dest, &["config", "http.receivepack", "true"]);
    for n in 0..HISTORY {
        commit(&work, n);
    }
    git(&work, &["remote", "add", "origin", dest.to_str().unwrap()]);
    if seed_destination {
        git(&work, &["push", "-q", "origin", "main"]);
    }
    Pair {
        _root: root,
        work,
        dest,
    }
}

/// Index files of the destination's packs.
fn indexes(dest: &Path) -> std::collections::BTreeSet<std::path::PathBuf> {
    std::fs::read_dir(dest.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "idx"))
        .collect()
}

/// Objects in the packs that appeared since `before`.
fn received(dest: &Path, before: &std::collections::BTreeSet<std::path::PathBuf>) -> usize {
    indexes(dest)
        .difference(before)
        .map(|index| {
            let output = Command::new("git")
                .arg("show-index")
                .stdin(std::fs::File::open(index).unwrap())
                .output()
                .unwrap();
            assert!(output.status.success());
            output.stdout.iter().filter(|b| **b == b'\n').count()
        })
        .sum()
}

fn create(name: &str, new: ObjectId) -> PushCommand {
    PushCommand {
        name: RefName::new(name).unwrap(),
        expected: None,
        new,
        force: ForcePolicy::FastForwardOnly,
    }
}

/// Pushes through `Repository::push` and returns the number of objects the destination received.
fn push(pair: &Pair, commands: Vec<PushCommand>) -> usize {
    push_in(pair, commands, &Environment::default())
}

fn push_in(pair: &Pair, commands: Vec<PushCommand>, environment: &Environment) -> usize {
    let before = indexes(&pair.dest);
    let repo = Repository::open(&pair.work).unwrap();
    let report = repo
        .push(
            "origin",
            commands,
            &PushOptions::default(),
            environment,
            &mut NoCallbacks,
        )
        .unwrap();
    assert!(report.all_succeeded(), "{report:?}");
    git(&pair.dest, &["fsck", "--strict", "--no-dangling"]);
    received(&pair.dest, &before)
}

fn tip(directory: &Path, name: &str) -> String {
    git(directory, &["rev-parse", name])
}

/// One commit, its tree and its new blob: everything else is in the advertised `main`.
const NEW_COMMIT_OBJECTS: usize = 3;

#[test]
fn push_new_branch_sends_only_new_objects() {
    let pair = pair(true);
    git(&pair.work, &["switch", "-q", "-c", "topic"]);
    let topic = commit(&pair.work, HISTORY);
    let sent = push(&pair, vec![create("refs/heads/topic", topic)]);
    assert!(sent <= NEW_COMMIT_OBJECTS, "sent {sent} objects");
    assert_eq!(tip(&pair.dest, "refs/heads/topic"), topic.to_string());
}

#[test]
fn push_uses_advertised_refs_as_haves_even_when_expected_is_none() {
    let pair = pair(true);
    // The new branch builds on `side`, which only an advertised ref other than the pushed one
    // (and other than `main`) covers; an annotated tag is advertised too.
    git(&pair.work, &["switch", "-q", "-c", "side"]);
    commit(&pair.work, HISTORY);
    git(&pair.work, &["push", "-q", "origin", "side"]);
    git(&pair.work, &["tag", "-a", "-m", "release", "v1", "main~5"]);
    git(&pair.work, &["push", "-q", "origin", "v1"]);
    git(&pair.work, &["switch", "-q", "-c", "topic"]);
    let topic = commit(&pair.work, HISTORY + 1);
    let sent = push(&pair, vec![create("refs/heads/topic", topic)]);
    assert!(sent <= NEW_COMMIT_OBJECTS, "sent {sent} objects");
}

#[test]
fn push_to_empty_receiver_sends_everything() {
    let pair = pair(false);
    let main = tip(&pair.work, "main").parse().unwrap();
    let sent = push(&pair, vec![create("refs/heads/main", main)]);
    // Every commit, tree and blob: each commit adds one blob and one tree.
    assert_eq!(sent, HISTORY * 3);
    assert_eq!(tip(&pair.dest, "refs/heads/main"), main.to_string());
}

#[cfg(feature = "http")]
mod http {
    use super::*;

    fn over_http(pair: &Pair) -> http_git::Server {
        let server = http_git::Server::new(&pair.dest, "", "", None);
        git(&pair.work, &["remote", "set-url", "origin", &server.url]);
        server
    }

    #[test]
    fn push_new_branch_sends_only_new_objects() {
        let pair = pair(true);
        let server = over_http(&pair);
        git(&pair.work, &["switch", "-q", "-c", "topic"]);
        let topic = commit(&pair.work, HISTORY);
        let sent = push(&pair, vec![create("refs/heads/topic", topic)]);
        assert!(sent <= NEW_COMMIT_OBJECTS, "sent {sent} objects");
        // One discovery and one receive-pack RPC, as Git's `send-pack`.
        assert_eq!(server.requests(), ["GET", "POST"]);
    }

    #[test]
    fn push_to_empty_receiver_sends_everything() {
        let pair = pair(false);
        let _server = over_http(&pair);
        let main = tip(&pair.work, "main").parse().unwrap();
        let sent = push(&pair, vec![create("refs/heads/main", main)]);
        assert_eq!(sent, HISTORY * 3);
    }
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
mod ssh {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// Routes `ssh://` to a stand-in client that runs the requested Git command locally, so the
    /// SSH session carries a real `git-receive-pack` without a server.
    fn over_ssh(pair: &Pair) -> Environment {
        let client = pair.dest.with_file_name("ssh");
        std::fs::write(
            &client,
            "#!/bin/sh\nfor last; do :; done\nexec /bin/sh -c \"$last\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o755)).unwrap();
        let url = format!("ssh://localhost{}", pair.dest.display());
        git(&pair.work, &["remote", "set-url", "origin", &url]);
        Environment::default()
            .with("PATH", std::env::var_os("PATH").unwrap())
            .with("GIT_SSH_COMMAND", client)
    }

    #[test]
    fn push_new_branch_sends_only_new_objects() {
        let pair = pair(true);
        let environment = over_ssh(&pair);
        git(&pair.work, &["switch", "-q", "-c", "topic"]);
        let topic = commit(&pair.work, HISTORY);
        let sent = push_in(&pair, vec![create("refs/heads/topic", topic)], &environment);
        assert!(sent <= NEW_COMMIT_OBJECTS, "sent {sent} objects");
    }

    #[test]
    fn push_to_empty_receiver_sends_everything() {
        let pair = pair(false);
        let environment = over_ssh(&pair);
        let main = tip(&pair.work, "main").parse().unwrap();
        let sent = push_in(&pair, vec![create("refs/heads/main", main)], &environment);
        assert_eq!(sent, HISTORY * 3);
    }
}
