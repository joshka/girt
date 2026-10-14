//! Configured-remote fetch reads the destination's references once.
//!
//! Histories, references and the expected fetch results come from the Git executable. No upstream
//! implementation or test source is used.

use std::path::Path;
use std::process::Command;

use super::*;

fn git(directory: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(directory)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fetch Author")
        .env("GIT_AUTHOR_EMAIL", "fetch@example.com")
        .env("GIT_COMMITTER_NAME", "Fetch Author")
        .env("GIT_COMMITTER_EMAIL", "fetch@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

const SPECS: [&str; 2] = [
    "+refs/heads/*:refs/remotes/origin/*",
    "+refs/tags/*:refs/tags/*",
];

/// An origin with branches and annotated tags, plus a destination already fetched from it that
/// holds many loose references of its own, as a jj clone does.
fn fetched(format: &str) -> (tempfile::TempDir, std::path::PathBuf, Repository) {
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("origin");
    std::fs::create_dir(&origin).unwrap();
    git(
        &origin,
        &[
            "init",
            "-q",
            &format!("--object-format={format}"),
            "-b",
            "main",
            ".",
        ],
    );
    git(&origin, &["config", "gc.auto", "0"]);
    for n in 0..20 {
        std::fs::write(origin.join("file"), format!("{n}\n")).unwrap();
        git(&origin, &["add", "file"]);
        git(&origin, &["commit", "-q", "-m", &format!("c{n}")]);
        git(&origin, &["tag", "-a", "-m", "t", &format!("v{n}")]);
        git(&origin, &["branch", &format!("b{n}")]);
    }
    let dest = root.path().join("dest.git");
    git(
        root.path(),
        &[
            "init",
            "-q",
            "--bare",
            &format!("--object-format={format}"),
            "dest.git",
        ],
    );
    git(
        &dest,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    );
    let repository = Repository::open(&dest).unwrap();
    fetch(&repository);
    // Loose references the fetch does not touch, like jj's bookkeeping refs.
    let head = git(&origin, &["rev-parse", "HEAD"]);
    for n in 0..200 {
        let dir = dest.join(format!("refs/jj/keep/k{}", n % 10));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("r{n}")), &head).unwrap();
    }
    (root, origin, repository)
}

fn fetch(repository: &Repository) {
    repository
        .fetch(
            "origin",
            &FetchOptions::new(SPECS).prune(true),
            &Environment::from_process(),
            &mut NoCallbacks,
        )
        .unwrap();
}

/// A one-commit fetch lists the destination's references once, and ends with exactly the
/// references `git fetch` writes from the same state.
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn one_commit_fetch_lists_references_once(#[case] format: &str) {
    let (root, origin, repository) = fetched(format);
    let oracle = root.path().join("oracle.git");
    std::fs::create_dir(&oracle).unwrap();
    for entry in walk(repository.git_dir()) {
        let target = oracle.join(entry.strip_prefix(repository.git_dir()).unwrap());
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::copy(&entry, &target).unwrap();
    }
    std::fs::write(origin.join("file"), "new\n").unwrap();
    git(&origin, &["commit", "-qam", "one more"]);

    crate::refs::LOOSE_SCANS.with(|scans| scans.borrow_mut().clear());
    fetch(&repository);
    let destination_scans = crate::refs::LOOSE_SCANS.with(|scans| {
        scans
            .borrow()
            .iter()
            .filter(|root| root.as_path() == repository.git_dir())
            .count()
    });
    // The source's advertisement lists the origin's references separately.
    assert_eq!(destination_scans, 1);

    let mut args = vec!["fetch", "-q", "--prune", "origin"];
    args.extend(SPECS);
    git(&oracle, &args);
    let refs = |dir: &Path| git(dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    assert_eq!(refs(repository.git_dir()), refs(&oracle));
    git(repository.git_dir(), &["fsck", "--strict", "--no-dangling"]);
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

/// Runs `change` once, while the transfer validates the received pack: after the destination's
/// references were listed and before any of them is updated.
struct DuringTransfer<F: FnMut()>(Option<F>);

impl<F: FnMut()> TransferCallbacks for DuringTransfer<F> {
    fn validation_progress(&mut self, _progress: crate::fetch::ValidationProgress) {
        if let Some(mut change) = self.0.take() {
            change();
        }
    }
}

/// How a destination reference is changed by another writer during the transfer.
#[derive(Clone, Copy, Debug)]
enum Race {
    /// A loose reference moves.
    Update,
    /// A reference absent at listing time appears.
    Create,
    /// A packed value is shadowed by a new loose value.
    ShadowPacked,
}

/// A writer that changes a destination after the single listing makes the conditional update
/// fail, and its value survives, whatever the reference's storage.
#[rstest::rstest]
#[case::update(Race::Update)]
#[case::create(Race::Create)]
#[case::shadow_packed(Race::ShadowPacked)]
fn reference_changed_during_transfer_is_not_clobbered(#[case] race: Race) {
    let (_root, origin, repository) = fetched("sha1");
    let dest = repository.git_dir().to_owned();
    let other = git(&origin, &["rev-parse", "v3^{commit}"])
        .trim()
        .to_owned();
    let name = match race {
        Race::Update | Race::ShadowPacked => "refs/remotes/origin/b5",
        Race::Create => "refs/remotes/origin/fresh",
    };
    if let Race::Create = race {
        git(&origin, &["branch", "fresh", "b7"]);
    }
    if let Race::ShadowPacked = race {
        git(&dest, &["pack-refs", "--all"]);
        assert!(!dest.join(name).exists());
    }
    std::fs::write(origin.join("file"), "moved\n").unwrap();
    git(&origin, &["commit", "-qam", "moved"]);
    git(&origin, &["branch", "-f", "b5", "HEAD"]);

    let mut callbacks = DuringTransfer(Some(|| {
        git(&dest, &["update-ref", name, &other]);
    }));
    let result = repository.fetch(
        "origin",
        &FetchOptions::new(SPECS).prune(true),
        &Environment::from_process(),
        &mut callbacks,
    );
    assert!(
        callbacks.0.is_none(),
        "the change must run during the transfer"
    );
    assert!(
        matches!(result, Err(TransferError::FetchFinish(_))),
        "{result:?}"
    );
    assert_eq!(git(&dest, &["rev-parse", name]).trim(), other);
    git(&dest, &["fsck", "--strict", "--no-dangling"]);
}
