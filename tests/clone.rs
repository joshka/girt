//! Original Git CLI fixtures; no upstream implementation or test source is used.
#![cfg(unix)]
use std::fs;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::AtomicBool;

use girt::clone::{BranchSelection, CloneFailure, CloneHead, CloneRequest, CloneTransferError};
use girt::fetch::{FetchError, FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory};
use girt::refs::{RefName, Reflog, Target};
use girt::remote::Remote;
use girt::transport::TransportControl;
use girt::{InitKind, ObjectId, Repository};
use rstest::rstest;

fn command(path: &Path, args: &[&str], input: &[u8]) -> Output {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", path.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "Workflow")
        .env("GIT_AUTHOR_EMAIL", "workflow@example.com")
        .env("GIT_COMMITTER_NAME", "Workflow")
        .env("GIT_COMMITTER_EMAIL", "workflow@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
fn git(path: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let output = command(path, args, input);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn oid(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}
fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}
struct Source {
    root: tempfile::TempDir,
    first: ObjectId,
    second: ObjectId,
    blob: ObjectId,
    tree: ObjectId,
    tag: ObjectId,
}
impl Source {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        git(
            root.path(),
            &[
                "init",
                "--bare",
                "--object-format=sha1",
                "--template=",
                "--initial-branch=main",
                ".",
            ],
            b"",
        );
        let blob = oid(&git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            b"original workflow payload\n",
        ));
        let tree = oid(&git(
            root.path(),
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        ));
        let first = oid(&git(
            root.path(),
            &["commit-tree", &tree.to_string()],
            b"first\n",
        ));
        let second = oid(&git(
            root.path(),
            &["commit-tree", &tree.to_string(), "-p", &first.to_string()],
            b"second\n",
        ));
        let tag = oid(&git(root.path(), &["mktag"], format!("object {first}\ntype commit\ntag v1\ntagger Workflow <workflow@example.com> 1700000000 +0000\n\nOriginal tag\n").as_bytes()));
        let source = Self {
            root,
            first,
            second,
            blob,
            tree,
            tag,
        };
        source.set("refs/heads/main", first);
        source.set("refs/heads/topic", second);
        source.set("refs/heads/private", second);
        source.set("refs/tags/v1", tag);
        source
    }
    fn set(&self, name: &str, value: ObjectId) {
        git(
            self.root.path(),
            &["update-ref", name, &value.to_string()],
            b"",
        );
    }
}
fn clone(
    source: &Source,
    path: &Path,
    kind: InitKind,
    selection: BranchSelection,
) -> girt::clone::CloneReport {
    let request = CloneRequest::prepare(
        path,
        kind,
        source.root.path().as_os_str().as_encoded_bytes(),
        selection,
        Reflog::Preserve,
    )
    .unwrap();
    request
        .receive_local(
            source.root.path(),
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap()
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap()
}
fn stored(repo: &Repository, value: &str) -> Option<Target> {
    repo.references().unwrap().read(&name(value)).unwrap()
}

#[rstest]
#[case::bare(InitKind::Bare)]
#[case::ordinary(InitKind::Worktree)]
fn populated_clone_and_subsequent_fetch_use_persisted_configuration(#[case] kind: InitKind) {
    let source = Source::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let report = clone(&source, &path, kind, BranchSelection::Default);
    assert!(report.reserved && report.initialized && report.configured);
    let repo = report.repository.unwrap();
    assert_eq!(
        stored(&repo, "HEAD"),
        Some(Target::Symbolic(name("refs/heads/main")))
    );
    assert_eq!(
        stored(&repo, "refs/heads/main"),
        Some(Target::Direct(source.first))
    );
    assert_eq!(
        stored(&repo, "refs/remotes/origin/topic"),
        Some(Target::Direct(source.second))
    );
    assert_eq!(
        stored(&repo, "refs/tags/v1"),
        Some(Target::Direct(source.tag))
    );
    assert_eq!(
        git(
            repo.git_dir(),
            &["cat-file", "blob", &source.blob.to_string()],
            b""
        ),
        b"original workflow payload\n"
    );
    assert_eq!(
        git(repo.git_dir(), &["rev-parse", "HEAD^{tree}"], b""),
        format!("{}\n", source.tree).as_bytes()
    );
    git(repo.git_dir(), &["fsck", "--strict", "--full"], b"");
    assert!(!repo.git_dir().join("index").exists());
    assert!(!path.join("file").exists());
    let remote = Remote::find(repo.config(), b"origin").unwrap().unwrap();
    assert_eq!(
        git(repo.git_dir(), &["config", "remote.origin.url"], b""),
        [source.root.path().as_os_str().as_encoded_bytes(), b"\n"].concat()
    );
    assert_eq!(
        git(repo.git_dir(), &["config", "branch.main.merge"], b""),
        b"refs/heads/main\n"
    );
    source.set("refs/heads/main", source.second);
    let request = FetchRequest::prepare(
        repo,
        remote.fetch_refspecs().clone(),
        Default::default(),
        Reflog::Preserve,
    )
    .unwrap();
    let fetch = request
        .receive_local(
            source.root.path(),
            &KnownHistory::default(),
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap()
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert!(fetch.installed.is_some());
    let repo = Repository::open(&path).unwrap();
    assert_eq!(
        stored(&repo, "refs/remotes/origin/main"),
        Some(Target::Direct(source.second))
    );
    assert_eq!(
        stored(&repo, "refs/heads/main"),
        Some(Target::Direct(source.first))
    );
    // Git consumes the same persisted origin configuration with no arguments or injected refspec.
    git(&path, &["fetch", "origin"], b"");
    git(&path, &["fsck", "--strict", "--full"], b"");
}

#[test]
fn ordinary_clone_matches_git_no_checkout_index_boundary() {
    let source = Source::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let report = clone(&source, &path, InitKind::Worktree, BranchSelection::Default);
    git(
        root.path(),
        &[
            "clone",
            "--no-checkout",
            "--no-local",
            source.root.path().to_str().unwrap(),
            "git-copy",
        ],
        b"",
    );
    let expected = git(
        &root.path().join("git-copy"),
        &["status", "--porcelain"],
        b"",
    );
    assert_eq!(git(&path, &["status", "--porcelain"], b""), expected);
    assert_eq!(expected, b"D  file\n");
    assert!(!report.repository.unwrap().git_dir().join("index").exists());
    assert_eq!(fs::read_dir(path).unwrap().count(), 1);
}

#[test]
fn explicit_branch_selects_local_head_without_excluding_other_branches() {
    let source = Source::new();
    let root = tempfile::tempdir().unwrap();
    let report = clone(
        &source,
        &root.path().join("copy"),
        InitKind::Bare,
        BranchSelection::Branch(name("refs/heads/topic")),
    );
    let repo = report.repository.unwrap();
    assert_eq!(
        stored(&repo, "HEAD"),
        Some(Target::Symbolic(name("refs/heads/topic")))
    );
    assert_eq!(
        stored(&repo, "refs/heads/topic"),
        Some(Target::Direct(source.second))
    );
    assert_eq!(
        stored(&repo, "refs/remotes/origin/main"),
        Some(Target::Direct(source.first))
    );
    assert_eq!(stored(&repo, "refs/heads/main"), None);
    assert_eq!(
        git(repo.git_dir(), &["config", "branch.topic.merge"], b""),
        b"refs/heads/topic\n"
    );
}

#[test]
fn detached_head_is_retained_even_when_equal_to_several_branches() {
    let source = Source::new();
    fs::write(
        source.root.path().join("HEAD"),
        format!("{}\n", source.second),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let report = clone(
        &source,
        &root.path().join("copy"),
        InitKind::Bare,
        BranchSelection::Default,
    );
    assert_eq!(report.head, CloneHead::Detached(source.second));
    let repo = report.repository.unwrap();
    assert_eq!(stored(&repo, "HEAD"), Some(Target::Direct(source.second)));
    assert!(
        repo.references()
            .unwrap()
            .list_namespace(&name("refs/heads"))
            .unwrap()
            .is_empty()
    );
    git(repo.git_dir(), &["fsck", "--strict", "--full"], b"");
}

#[rstest]
#[case::default(BranchSelection::Default, "refs/heads/main")]
#[case::selected(BranchSelection::Branch(name("refs/heads/topic")), "refs/heads/topic")]
fn empty_repository_has_unborn_head_and_persistent_remote(
    #[case] selection: BranchSelection,
    #[case] head: &str,
) {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    fs::create_dir(&source).unwrap();
    git(
        &source,
        &[
            "init",
            "--bare",
            "--template=",
            "--object-format=sha1",
            "--initial-branch=unadvertised",
            ".",
        ],
        b"",
    );
    let path = root.path().join("copy");
    let request = CloneRequest::prepare(
        &path,
        InitKind::Worktree,
        source.as_os_str().as_encoded_bytes(),
        selection,
        Reflog::Preserve,
    )
    .unwrap();
    let report = request
        .receive_local(
            &source,
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap()
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.head, CloneHead::Unborn(name(head)));
    let repo = report.repository.unwrap();
    assert_eq!(stored(&repo, "HEAD"), Some(Target::Symbolic(name(head))));
    assert!(repo.references().unwrap().list().unwrap().is_empty());
    assert!(Remote::find(repo.config(), b"origin").unwrap().is_some());
    git(&path, &["fsck", "--strict"], b"");
    assert!(!repo.git_dir().join("index").exists());
}

#[test]
fn missing_remote_head_requires_branch_selection_without_destination_effects() {
    let source = Source::new();
    git(
        source.root.path(),
        &["symbolic-ref", "HEAD", "refs/heads/absent"],
        b"",
    );
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let request = CloneRequest::prepare(
        &path,
        InitKind::Bare,
        b"source",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let result = request.receive_local(
        source.root.path(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(result, Err(CloneTransferError::Plan(_))));
    assert!(!path.exists());
    let report = clone(
        &source,
        &path,
        InitKind::Bare,
        BranchSelection::Branch(name("refs/heads/main")),
    );
    assert!(report.repository.is_some());
}

#[rstest]
#[case::directory(true)]
#[case::file(false)]
fn existing_destinations_are_never_accepted(#[case] directory: bool) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    create_existing(&path, directory);
    let result = CloneRequest::prepare(
        &path,
        InitKind::Worktree,
        b"source",
        BranchSelection::Default,
        Reflog::Preserve,
    );
    assert!(matches!(result, Err(CloneFailure::Exists(_))));
    assert!(path.exists());
}
fn create_existing(path: &Path, directory: bool) {
    if directory {
        fs::create_dir(path).unwrap();
    } else {
        fs::write(path, b"keep").unwrap();
    }
}

#[test]
fn dangling_destination_symlink_is_not_followed() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    std::os::unix::fs::symlink(root.path().join("absent"), &path).unwrap();
    let result = CloneRequest::prepare(
        &path,
        InitKind::Bare,
        b"source",
        BranchSelection::Default,
        Reflog::Preserve,
    );
    assert!(matches!(result, Err(CloneFailure::Exists(_))));
    assert!(fs::symlink_metadata(path).unwrap().is_symlink());
    assert!(!root.path().join("absent").exists());
}

#[test]
fn receive_cancellation_leaves_no_destination() {
    let source = Source::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let request = CloneRequest::prepare(
        &path,
        InitKind::Bare,
        b"source",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let result = request.receive_local(
        source.root.path(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(true)),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(
        result,
        Err(CloneTransferError::Transfer(FetchError::Cancelled))
    ));
    assert!(!path.exists());
}

#[test]
fn changed_remote_after_download_does_not_change_published_id() {
    let source = Source::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let request = CloneRequest::prepare(
        &path,
        InitKind::Bare,
        b"source",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let ready = request
        .receive_local(
            source.root.path(),
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    source.set("refs/heads/main", source.second);
    let report = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        stored(&report.repository.unwrap(), "refs/heads/main"),
        Some(Target::Direct(source.first))
    );
}

#[test]
fn noncommit_branch_is_rejected_after_install_without_final_head_or_config() {
    let source = Source::new();
    fs::write(
        source.root.path().join("refs/heads/main"),
        format!("{}\n", source.blob),
    )
    .unwrap();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let request = CloneRequest::prepare(
        &path,
        InitKind::Bare,
        b"source",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let ready = request
        .receive_local(
            source.root.path(),
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        error.source,
        CloneFailure::Verification(FetchError::Kind(_))
    ));
    assert!(error.report.fetch.as_ref().unwrap().installed.is_some());
    assert!(!error.report.configured);
    assert!(!path.join("refs/heads/main").exists());
}

#[test]
fn git_reads_exact_quoted_remote_metadata() {
    let source = Source::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let url = b" /literal/\"quote\\tab\tline\n\xff ";
    let request = CloneRequest::prepare(
        &path,
        InitKind::Bare,
        url,
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let report = request
        .receive_local(
            source.root.path(),
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap()
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert!(report.configured);
    assert_eq!(
        git(&path, &["config", "remote.origin.url"], b""),
        [url.as_slice(), b"\n"].concat()
    );
}

#[test]
fn transfer_object_limit_leaves_destination_absent() {
    let source = Source::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    let request = CloneRequest::prepare(
        &path,
        InitKind::Bare,
        b"source",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let limits = FetchLimits {
        max_objects: 0,
        ..FetchLimits::default()
    };
    let result = request.receive_local(
        source.root.path(),
        limits,
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(
        result,
        Err(CloneTransferError::Transfer(FetchError::Limit(_)))
    ));
    assert!(!path.exists());
}
