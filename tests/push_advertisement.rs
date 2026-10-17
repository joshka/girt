//! `girt::workflow::push` excludes history the receiver advertises, checked against Git receivers.
//!
//! Destinations keep received packs (`receive.unpackLimit=1`) and don't repack after receiving
//! (`receive.autoGc=false`), so the objects a push sent are the entries of the pack index files it
//! added, as listed by `git show-index`.
#[cfg(feature = "http")]
#[path = "support/http_git.rs"]
mod http_git;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
#[path = "support/stand_in.rs"]
mod stand_in;

use std::path::Path;
use std::process::Command;

use girt::push::{ForcePolicy, PushCommand, PushError, PushReport};
use girt::refs::RefName;
use girt::workflow::{Environment, NoCallbacks, PushOptions, TransferError};
use girt::{ObjectId, Repository};
use rstest::rstest;

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
/// holding `main`, with `origin` configured as the destination path.
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
    receiving(&dest);
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

/// Configures a destination to keep received packs as sent and to accept HTTP pushes.
fn receiving(dest: &Path) {
    git(dest, &["config", "receive.unpackLimit", "1"]);
    git(dest, &["config", "receive.autoGc", "false"]);
    git(dest, &["config", "http.receivepack", "true"]);
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
    PushCommand::new(RefName::new(name).unwrap(), None, new)
        .with_force(ForcePolicy::FastForwardOnly)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Transport {
    Local,
    #[cfg(feature = "http")]
    Http,
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    Ssh,
}

/// `origin` reaching the destination over one transport; keep it alive while pushing.
struct Remote {
    environment: Environment,
    #[cfg(feature = "http")]
    server: Option<http_git::Server>,
}

/// Points `origin` at the destination over `transport`. SSH goes through a stand-in client that
/// runs the requested Git command locally, so the session carries a real `git-receive-pack`
/// without a server.
fn connect(_pair: &Pair, transport: Transport) -> Remote {
    let remote = Remote {
        environment: Environment::default(),
        #[cfg(feature = "http")]
        server: None,
    };
    match transport {
        Transport::Local => remote,
        #[cfg(feature = "http")]
        Transport::Http => {
            let server = http_git::Server::new(&_pair.dest, "", "", None);
            git(&_pair.work, &["remote", "set-url", "origin", &server.url]);
            Remote {
                server: Some(server),
                ..remote
            }
        }
        #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
        Transport::Ssh => {
            let client = _pair.dest.with_file_name("ssh");
            stand_in::shell(&client, "for last; do :; done\nexec /bin/sh -c \"$last\"\n");
            let url = format!("ssh://localhost{}", _pair.dest.display());
            git(&_pair.work, &["remote", "set-url", "origin", &url]);
            let environment = Environment::default()
                .with("PATH", std::env::var_os("PATH").unwrap())
                .with("GIT_SSH_COMMAND", client);
            Remote {
                environment,
                #[cfg(feature = "http")]
                server: remote.server,
            }
        }
    }
}

fn try_push(
    pair: &Pair,
    remote: &Remote,
    commands: Vec<PushCommand>,
) -> Result<PushReport, TransferError> {
    // Installed thin packs include receiver bases appended during repair. Keep this selection
    // corpus ordinary so its exact index counts continue to measure objects transmitted.
    let options = PushOptions::default().with_compression(girt::PackCompression::Ordinary);
    girt::workflow::push(
        &Repository::open(&pair.work).unwrap(),
        "origin",
        commands,
        &options,
        &remote.environment,
        &mut NoCallbacks,
    )
}

/// Pushes through `girt::workflow::push` and returns the number of objects the destination
/// received.
fn push(pair: &Pair, remote: &Remote, commands: Vec<PushCommand>) -> usize {
    let before = indexes(&pair.dest);
    let report = try_push(pair, remote, commands).unwrap();
    assert!(report.all_succeeded(), "{report:?}");
    git(&pair.dest, &["fsck", "--strict", "--no-dangling"]);
    received(&pair.dest, &before)
}

fn tip(directory: &Path, name: &str) -> String {
    git(directory, &["rev-parse", name])
}

/// Commits `topic` one commit ahead of the source's current branch.
fn topic(pair: &Pair) -> ObjectId {
    git(&pair.work, &["switch", "-q", "-c", "topic"]);
    commit(&pair.work, HISTORY + 10)
}

/// One commit, its tree and its new blob: everything else is in advertised history.
const NEW_COMMIT_OBJECTS: usize = 3;

#[rstest]
#[case::local(Transport::Local)]
#[cfg_attr(feature = "http", case::http(Transport::Http))]
#[cfg_attr(
    all(feature = "ssh", any(target_os = "macos", target_os = "linux")),
    case::ssh(Transport::Ssh)
)]
fn push_new_branch_sends_only_new_objects(#[case] transport: Transport) {
    let pair = pair(true);
    let remote = connect(&pair, transport);
    let topic = topic(&pair);
    let sent = push(&pair, &remote, vec![create("refs/heads/topic", topic)]);
    assert!(sent <= NEW_COMMIT_OBJECTS, "sent {sent} objects");
    assert_eq!(tip(&pair.dest, "refs/heads/topic"), topic.to_string());
    // One discovery and one receive-pack RPC, as Git's `send-pack`.
    #[cfg(feature = "http")]
    if let Some(server) = &remote.server {
        assert_eq!(server.requests(), ["GET", "POST"]);
    }
}

#[rstest]
#[case::local(Transport::Local)]
#[cfg_attr(feature = "http", case::http(Transport::Http))]
#[cfg_attr(
    all(feature = "ssh", any(target_os = "macos", target_os = "linux")),
    case::ssh(Transport::Ssh)
)]
fn push_uses_advertised_refs_as_haves_even_when_expected_is_none(#[case] transport: Transport) {
    let pair = pair(true);
    // The new branch builds on `side`, which only an advertised ref other than `main` covers.
    git(&pair.work, &["switch", "-q", "-c", "side"]);
    commit(&pair.work, HISTORY);
    git(&pair.work, &["push", "-q", "origin", "side"]);
    let remote = connect(&pair, transport);
    let topic = topic(&pair);
    let sent = push(&pair, &remote, vec![create("refs/heads/topic", topic)]);
    assert!(sent <= NEW_COMMIT_OBJECTS, "sent {sent} objects");
}

#[rstest]
#[case::local(Transport::Local)]
#[cfg_attr(feature = "http", case::http(Transport::Http))]
#[cfg_attr(
    all(feature = "ssh", any(target_os = "macos", target_os = "linux")),
    case::ssh(Transport::Ssh)
)]
fn push_excludes_history_of_an_annotated_tag_alone(#[case] transport: Transport) {
    let pair = pair(false);
    // The destination's only ref is an annotated tag of `main`, which the walk peels.
    git(&pair.work, &["tag", "-a", "-m", "release", "v1", "main"]);
    git(&pair.work, &["push", "-q", "origin", "v1"]);
    let remote = connect(&pair, transport);
    let topic = topic(&pair);
    let sent = push(&pair, &remote, vec![create("refs/heads/topic", topic)]);
    assert!(sent <= NEW_COMMIT_OBJECTS, "sent {sent} objects");
}

#[rstest]
#[case::local(Transport::Local)]
#[cfg_attr(feature = "http", case::http(Transport::Http))]
#[cfg_attr(
    all(feature = "ssh", any(target_os = "macos", target_os = "linux")),
    case::ssh(Transport::Ssh)
)]
fn push_to_empty_receiver_sends_everything(#[case] transport: Transport) {
    let pair = pair(false);
    let remote = connect(&pair, transport);
    let main = tip(&pair.work, "main").parse().unwrap();
    let sent = push(&pair, &remote, vec![create("refs/heads/main", main)]);
    // Every commit, tree and blob: each commit adds one blob and one tree.
    assert_eq!(sent, HISTORY * 3);
    assert_eq!(tip(&pair.dest, "refs/heads/main"), main.to_string());
}

#[rstest]
#[case::local(Transport::Local)]
#[cfg_attr(feature = "http", case::http(Transport::Http))]
#[cfg_attr(
    all(feature = "ssh", any(target_os = "macos", target_os = "linux")),
    case::ssh(Transport::Ssh)
)]
fn push_does_not_rely_on_hidden_refs(#[case] transport: Transport) {
    let pair = pair(false);
    // `main` is visible ten commits back; only a hidden ref covers the rest of the history.
    git(
        &pair.work,
        &["push", "-q", "origin", "main~10:refs/heads/main"],
    );
    git(
        &pair.work,
        &["push", "-q", "origin", "main:refs/hidden/main"],
    );
    git(&pair.dest, &["config", "receive.hideRefs", "refs/hidden"]);
    let remote = connect(&pair, transport);
    let topic = topic(&pair);
    let sent = push(&pair, &remote, vec![create("refs/heads/topic", topic)]);
    // As `git push` would: the ten commits above the visible `main`, plus the new one.
    assert_eq!(sent, 11 * 3);
}

/// Git's receive-pack advertises the refs of alternate object stores as `.have` lines. The
/// native local transport doesn't, so it sends the history the alternate already holds.
#[rstest]
#[case::local(Transport::Local, HISTORY * 3 + NEW_COMMIT_OBJECTS)]
#[cfg_attr(feature = "http", case::http(Transport::Http, NEW_COMMIT_OBJECTS))]
#[cfg_attr(
    all(feature = "ssh", any(target_os = "macos", target_os = "linux")),
    case::ssh(Transport::Ssh, NEW_COMMIT_OBJECTS)
)]
fn push_excludes_alternate_history_advertised_as_have(
    #[case] transport: Transport,
    #[case] expected: usize,
) {
    let pair = pair(true);
    // A fresh destination borrows the seeded one's objects and has no refs of its own.
    let shared = pair.dest.with_file_name("shared.git");
    std::fs::rename(&pair.dest, &shared).unwrap();
    std::fs::create_dir(&pair.dest).unwrap();
    git(&pair.dest, &["init", "-q", "--bare", "."]);
    receiving(&pair.dest);
    std::fs::write(
        pair.dest.join("objects/info/alternates"),
        format!("{}\n", shared.join("objects").display()),
    )
    .unwrap();
    let remote = connect(&pair, transport);
    let topic = topic(&pair);
    let sent = push(&pair, &remote, vec![create("refs/heads/topic", topic)]);
    assert_eq!(sent, expected);
}

#[rstest]
#[case::local(Transport::Local)]
#[cfg_attr(feature = "http", case::http(Transport::Http))]
#[cfg_attr(
    all(feature = "ssh", any(target_os = "macos", target_os = "linux")),
    case::ssh(Transport::Ssh)
)]
fn push_refuses_a_shallow_receiver_without_changes(#[case] transport: Transport) {
    let pair = pair(false);
    // A shallow receiver's tips don't stand for complete history.
    std::fs::remove_dir_all(&pair.dest).unwrap();
    let source = format!("file://{}", pair.work.display());
    git(
        pair.work.parent().unwrap(),
        &["clone", "-q", "--bare", "--depth=5", &source, "dest.git"],
    );
    receiving(&pair.dest);
    let remote = connect(&pair, transport);
    let topic = topic(&pair);
    let before = indexes(&pair.dest);
    let result = try_push(&pair, &remote, vec![create("refs/heads/topic", topic)]);
    assert!(
        matches!(&result, Err(TransferError::Push(error)) if matches!(**error, PushError::NotSent(_))),
        "{result:?}"
    );
    assert_eq!(indexes(&pair.dest), before);
    assert!(git(&pair.dest, &["for-each-ref", "refs/heads/topic"]).is_empty());
}
