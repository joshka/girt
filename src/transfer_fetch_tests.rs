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

/// An origin with `files` blobs at the root, in `a` and in `a/b`, and a destination that Git
/// fetched it into. Git packs the destination's objects unless `loose`.
fn wide(
    format: &str,
    files: usize,
    loose: bool,
) -> (tempfile::TempDir, std::path::PathBuf, Repository) {
    let root = tempfile::tempdir().unwrap();
    let origin = root.path().join("origin");
    std::fs::create_dir_all(origin.join("a/b")).unwrap();
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
    for n in 0..files {
        std::fs::write(origin.join(format!("f{n}")), format!("root {n}\n")).unwrap();
        std::fs::write(origin.join(format!("a/h{n}")), format!("a {n}\n")).unwrap();
        std::fs::write(origin.join(format!("a/b/g{n}")), format!("b {n}\n")).unwrap();
    }
    git(&origin, &["add", "."]);
    git(&origin, &["commit", "-q", "-m", "base"]);
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
    let limit = if loose {
        "fetch.unpackLimit=1000000"
    } else {
        "fetch.unpackLimit=1"
    };
    let mut args = vec!["-c", limit, "fetch", "-q", "origin"];
    args.extend(SPECS);
    git(&dest, &args);
    (root, origin, Repository::open(&dest).unwrap())
}

fn try_fetch(repository: &Repository) -> Result<FetchOutcome, TransferError> {
    repository.fetch(
        "origin",
        &FetchOptions::new(SPECS).prune(true),
        &Environment::from_process(),
        &mut NoCallbacks,
    )
}

fn loose_path(repository: &Repository, id: &str) -> std::path::PathBuf {
    repository
        .git_dir()
        .join("objects")
        .join(&id[..2])
        .join(&id[2..])
}

/// Destination pack entries inflated by a one-commit fetch that edits `a/b/g0`.
fn nested_edit_inflations(format: &str, files: usize) -> usize {
    let (_root, origin, repository) = wide(format, files, false);
    std::fs::write(origin.join("a/b/g0"), "edited\n").unwrap();
    git(&origin, &["commit", "-qam", "edit"]);
    crate::pack::counters::reset();
    fetch(&repository);
    let inflations =
        crate::pack::counters::INFLATIONS.with(|counts| counts.borrow().values().sum::<usize>());
    let refs = |dir: &Path| git(dir, &["for-each-ref", "--format=%(refname) %(objectname)"]);
    assert_eq!(
        refs(repository.git_dir()).trim(),
        format!(
            "refs/remotes/origin/main {}",
            git(&origin, &["rev-parse", "HEAD"]).trim()
        )
    );
    git(repository.git_dir(), &["fsck", "--strict", "--no-dangling"]);
    inflations
}

/// Validation stops at objects named by the parent's trees, which existing references make
/// complete, so a one-commit fetch reads the changed paths rather than the unchanged tree.
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn one_commit_fetch_reads_only_changed_paths(#[case] format: &str) {
    let small = nested_edit_inflations(format, 20);
    let large = nested_edit_inflations(format, 400);
    assert!(small <= 16, "{small} inflations for 60 unchanged blobs");
    assert!(
        large <= small + 4,
        "{large} inflations for 1200 unchanged blobs, {small} for 60"
    );
}

/// An unchanged object missing from a damaged destination is sent again rather than trusted,
/// because the parent's tree only names it.
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn unchanged_object_missing_from_destination_is_received(#[case] format: &str) {
    let (_root, origin, repository) = wide(format, 3, true);
    let unchanged = git(&origin, &["rev-parse", "HEAD:a/h1"]);
    std::fs::remove_file(loose_path(&repository, unchanged.trim())).unwrap();
    std::fs::write(origin.join("a/b/g0"), "edited\n").unwrap();
    git(&origin, &["commit", "-qam", "edit"]);
    fetch(&repository);
    git(repository.git_dir(), &["fsck", "--strict", "--no-dangling"]);
}

/// An unchanged dependency removed after the walk and before installation fails the fetch before
/// any reference moves.
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn dependency_removed_before_installation_is_rejected(#[case] format: &str) {
    let (_root, origin, repository) = wide(format, 3, true);
    let before = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    let unchanged = git(&origin, &["rev-parse", "HEAD:a/h1"]);
    let path = loose_path(&repository, unchanged.trim());
    std::fs::write(origin.join("a/b/g0"), "edited\n").unwrap();
    git(&origin, &["commit", "-qam", "edit"]);
    let mut callbacks = DuringTransfer(Some(|| std::fs::remove_file(&path).unwrap()));
    let result = repository.fetch(
        "origin",
        &FetchOptions::new(SPECS).prune(true),
        &Environment::from_process(),
        &mut callbacks,
    );
    assert!(
        callbacks.0.is_none(),
        "the removal must run during the transfer"
    );
    assert!(result.is_err(), "{result:?}");
    let after = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    assert_eq!(after, before);
}

/// A new tree naming an object the origin does not have is rejected without moving references.
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn missing_received_object_is_rejected(#[case] format: &str) {
    let (_root, origin, repository) = wide(format, 3, false);
    let before = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    let missing = git_input(&origin, &["hash-object", "--stdin"], "never stored\n");
    let listing = git(&origin, &["ls-tree", "HEAD"]);
    let tree = git_input(
        &origin,
        &["mktree", "--missing"],
        &format!("{listing}100644 blob {missing}\tabsent\n"),
    );
    let parent = git(&origin, &["rev-parse", "HEAD"]);
    let commit = git(
        &origin,
        &["commit-tree", &tree, "-p", parent.trim(), "-m", "broken"],
    );
    git(&origin, &["update-ref", "refs/heads/main", commit.trim()]);
    let result = try_fetch(&repository);
    assert!(result.is_err(), "{result:?}");
    let after = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    assert_eq!(after, before);
}

/// A new blob whose stored bytes are another object's is rejected without moving references.
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn corrupt_received_object_is_rejected(#[case] format: &str) {
    let (_root, origin, repository) = wide(format, 3, false);
    let before = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    std::fs::write(origin.join("a/b/g0"), "edited\n").unwrap();
    git(&origin, &["commit", "-qam", "edit"]);
    let edited = git(&origin, &["rev-parse", "HEAD:a/b/g0"]);
    let other = git(&origin, &["rev-parse", "HEAD:a/b/g1"]);
    let object = |id: &str| origin.join(".git/objects").join(&id[..2]).join(&id[2..]);
    let target = object(edited.trim());
    let mut permissions = std::fs::metadata(&target).unwrap().permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "test replaces a loose object"
    )]
    permissions.set_readonly(false);
    std::fs::set_permissions(&target, permissions).unwrap();
    std::fs::copy(object(other.trim()), &target).unwrap();
    let result = try_fetch(&repository);
    assert!(result.is_err(), "{result:?}");
    let after = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    assert_eq!(after, before);
}

/// Runs Git with `input` on standard input, returning its trimmed output.
fn git_input(directory: &Path, args: &[&str], input: &str) -> String {
    let mut child = Command::new("git")
        .current_dir(directory)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(child.stdin.as_mut().unwrap(), input.as_bytes()).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// Where the destination's unchanged object is selected from when a wrong-kind copy replaces it.
#[derive(Clone, Copy, Debug)]
enum Selected {
    /// The destination's own loose object.
    Primary,
    /// The destination's loose object, ahead of a correct copy in an alternate store.
    AheadOfAlternate,
}

/// A complete dependency whose selected representation has another kind than the parent's tree
/// requires fails the fetch before any reference moves, even with a correct copy in an alternate.
#[rstest::rstest]
#[case::primary(Selected::Primary)]
#[case::ahead_of_alternate(Selected::AheadOfAlternate)]
fn wrong_kind_complete_dependency_is_rejected(#[case] selected: Selected) {
    let (root, origin, repository) = wide("sha1", 3, true);
    if let Selected::AheadOfAlternate = selected {
        let alternate = root.path().join("alternate.git");
        git(
            root.path(),
            &[
                "clone",
                "-q",
                "--bare",
                "--no-local",
                &origin.to_string_lossy(),
                "alternate.git",
            ],
        );
        std::fs::write(
            repository.git_dir().join("objects/info/alternates"),
            format!("{}\n", alternate.join("objects").display()),
        )
        .unwrap();
    }
    let before = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    let unchanged = git(&origin, &["rev-parse", "HEAD:a/h1"]);
    let tree = git(&origin, &["rev-parse", "HEAD:a"]);
    let path = loose_path(&repository, unchanged.trim());
    let tree_file = origin
        .join(".git/objects")
        .join(&tree.trim()[..2])
        .join(&tree.trim()[2..]);
    std::fs::write(origin.join("a/b/g0"), "edited\n").unwrap();
    git(&origin, &["commit", "-qam", "edit"]);
    let mut callbacks = DuringTransfer(Some(|| {
        std::fs::remove_file(&path).unwrap();
        std::fs::copy(&tree_file, &path).unwrap();
    }));
    let result = repository.fetch(
        "origin",
        &FetchOptions::new(SPECS).prune(true),
        &Environment::from_process(),
        &mut callbacks,
    );
    assert!(
        callbacks.0.is_none(),
        "the replacement must run during the transfer"
    );
    assert!(
        result.is_err(),
        "fetch accepted a wrong-kind dependency: {result:?}"
    );
    let after = git(
        repository.git_dir(),
        &["rev-parse", "refs/remotes/origin/main"],
    );
    assert_eq!(after, before);
}
