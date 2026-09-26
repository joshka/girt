//! Original Git CLI fixtures exercise orchestration through public APIs only.
#![cfg(unix)]
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::ops::ControlFlow;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use girt::fetch::{
    FetchFinishFailure, FetchLimits, FetchPlanError, FetchReady, FetchRequest, FetchUpdateKind,
    FetchUpdateLimits, FetchWorkflowError, KnownHistory,
};
use girt::refs::{Expected, RefName, Reflog, Target, TransactionError};
use girt::remote::{Direction, Refspecs};
use girt::transport::TransportControl;
use girt::{InitKind, ObjectId, PackLimits, ReadLimits, Repository};
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
fn destination() -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        girt::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    (root, repository)
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
fn request(
    repository: &Repository,
    specs: &[&str],
    authorized: &[&str],
    reflog: Reflog,
) -> FetchRequest {
    FetchRequest::prepare(
        Repository::open(repository.git_dir()).unwrap(),
        Refspecs::parse(Direction::Fetch, specs.iter().map(|s| s.as_bytes())).unwrap(),
        authorized.iter().map(|s| name(s)).collect(),
        reflog,
    )
    .unwrap()
}
fn receive(request: FetchRequest, source: &Source, known: &KnownHistory) -> FetchReady {
    request
        .receive_local(
            source.root.path(),
            known,
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap()
}
fn fetch(repository: &Repository, source: &Source, specs: &[&str]) -> girt::fetch::FetchReport {
    receive(
        request(repository, specs, &[], Reflog::Preserve),
        source,
        &KnownHistory::default(),
    )
    .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
    .unwrap()
}
fn stored(repository: &Repository, refname: &str) -> Option<Target> {
    repository
        .references()
        .unwrap()
        .read(&name(refname))
        .unwrap()
}
fn assert_available(repository: &Repository, id: ObjectId) {
    assert!(
        repository
            .objects(PackLimits::default())
            .unwrap()
            .read(id, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    git(
        repository.git_dir(),
        &["cat-file", "-e", &id.to_string()],
        b"",
    );
}

#[test]
fn initial_multiple_refs_exclusions_and_incremental_fetch_agree_with_git() {
    let source = Source::new();
    let (_root, repository) = destination();
    let (_git_root, git_repository) = destination();
    let specs = [
        "refs/heads/*:refs/remotes/origin/*",
        "^refs/heads/private",
        "refs/tags/*:refs/tags/*",
    ];
    let first = fetch(&repository, &source, &specs);
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            specs[0],
            specs[1],
            specs[2],
        ],
        b"",
    );
    assert_eq!(first.references.len(), 3);
    assert_eq!(
        git(repository.git_dir(), &["show-ref"], b""),
        git(git_repository.git_dir(), &["show-ref"], b"")
    );
    assert_eq!(stored(&repository, "refs/remotes/origin/private"), None);
    assert_available(&repository, source.tag);
    source.set("refs/heads/main", source.second);
    let known = KnownHistory::new(
        &repository.objects(PackLimits::default()).unwrap(),
        &[source.first],
        FetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let incremental = receive(
        request(&repository, &specs, &[], Reflog::Preserve),
        &source,
        &known,
    )
    .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
    .unwrap();
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            specs[0],
            specs[1],
            specs[2],
        ],
        b"",
    );
    assert_eq!(incremental.references.len(), 1);
    assert_eq!(
        stored(&repository, "refs/remotes/origin/main"),
        Some(Target::Direct(source.second))
    );
    assert_eq!(
        git(repository.git_dir(), &["show-ref"], b""),
        git(git_repository.git_dir(), &["show-ref"], b"")
    );
    assert!(!repository.git_dir().join("FETCH_HEAD").exists());
}

#[test]
fn source_only_and_empty_selection_report_no_reference_edits() {
    let source = Source::new();
    let (_root, repository) = destination();
    let report = fetch(&repository, &source, &["refs/heads/main"]);
    assert_eq!(report.updates[0].kind, FetchUpdateKind::SourceOnly);
    assert!(report.references.is_empty());
    assert_available(&repository, source.first);
    let empty = fetch(&repository, &source, &[]);
    assert!(empty.updates.is_empty());
    assert_eq!(empty.pack_bytes, 0);
    assert_eq!(empty.installed.unwrap().checksum, None);
    assert!(repository.references().unwrap().list().unwrap().is_empty());
    assert!(!repository.git_dir().join("FETCH_HEAD").exists());
}

#[test]
fn missing_exact_source_does_not_discard_valid_update() {
    let source = Source::new();
    let (_root, repository) = destination();
    let (_git_root, git_repository) = destination();
    let report = fetch(
        &repository,
        &source,
        &[
            "refs/heads/missing:refs/remotes/origin/missing",
            "refs/heads/main:refs/remotes/origin/main",
        ],
    );
    assert_eq!(report.missing, vec![b"refs/heads/missing".to_vec()]);
    assert_eq!(report.references.len(), 1);
    assert_eq!(
        stored(&repository, "refs/remotes/origin/main"),
        Some(Target::Direct(source.first))
    );
    assert_eq!(stored(&repository, "refs/remotes/origin/missing"), None);
    let git_result = command(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            "refs/heads/missing:refs/remotes/origin/missing",
            "refs/heads/main:refs/remotes/origin/main",
        ],
        b"",
    );
    assert!(!git_result.status.success());
    // Git 2.55.0 aborts this CLI fetch before publishing either mapping. Girt exposes the
    // missing selection alongside the successful explicit mapping for caller policy.
    assert_eq!(stored(&git_repository, "refs/remotes/origin/main"), None);
    assert_eq!(stored(&git_repository, "refs/remotes/origin/missing"), None);
}

#[test]
fn prune_deletes_only_owned_missing_remote_tracking_refs() {
    let source = Source::new();
    let (_root, repository) = destination();
    let stale = name("refs/remotes/origin/gone");
    let excluded = name("refs/remotes/origin/private");
    let unrelated = name("refs/remotes/other/gone");
    let tag = name("refs/tags/gone");
    let alias = name("refs/remotes/origin/HEAD");
    let refs = repository.references().unwrap();
    for name in [&stale, &excluded, &unrelated, &tag] {
        refs.update_without_reflog(name, Target::Direct(source.first), Expected::Absent)
            .unwrap();
    }
    refs.update_without_reflog(
        &alias,
        Target::Symbolic(name("refs/remotes/origin/main")),
        Expected::Absent,
    )
    .unwrap();
    let specs = ["refs/heads/*:refs/remotes/origin/*", "^refs/heads/private"];
    let ready = receive(
        request(&repository, &specs, &[], Reflog::Preserve).with_prune(),
        &source,
        &KnownHistory::default(),
    );
    let report = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert!(
        report
            .updates
            .iter()
            .any(|update| update.kind == FetchUpdateKind::Prune
                && update.mapping.destination.as_ref() == Some(&stale))
    );
    assert_eq!(stored(&repository, "refs/remotes/origin/gone"), None);
    assert!(stored(&repository, "refs/remotes/origin/private").is_some());
    assert!(stored(&repository, "refs/remotes/other/gone").is_some());
    assert!(stored(&repository, "refs/tags/gone").is_some());
    assert_eq!(
        stored(&repository, "refs/remotes/origin/HEAD"),
        Some(Target::Symbolic(name("refs/remotes/origin/main")))
    );
}

#[test]
fn prune_refuses_a_concurrent_destination_change() {
    let source = Source::new();
    let (_root, repository) = destination();
    let stale = name("refs/remotes/origin/gone");
    let refs = repository.references().unwrap();
    refs.update_without_reflog(&stale, Target::Direct(source.first), Expected::Absent)
        .unwrap();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/*:refs/remotes/origin/*"],
            &[],
            Reflog::Preserve,
        )
        .with_prune(),
        &source,
        &KnownHistory::default(),
    );
    refs.update_without_reflog(
        &stale,
        Target::Direct(source.second),
        Expected::Value(Target::Direct(source.first)),
    )
    .unwrap();
    let failure = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(failure.report.installed.is_some());
    assert!(matches!(
        *failure.source,
        FetchFinishFailure::Publication(TransactionError::Prepare { .. })
    ));
    assert_eq!(
        stored(&repository, "refs/remotes/origin/gone"),
        Some(Target::Direct(source.second))
    );
    assert_available(&repository, source.first);
}

#[test]
fn known_only_unchanged_fetch_does_not_write_refs_or_logs() {
    let source = Source::new();
    let (_root, repository) = destination();
    let specs = ["refs/heads/main:refs/remotes/origin/main"];
    fetch(&repository, &source, &specs);
    let before = fs::metadata(repository.git_dir().join("refs/remotes/origin/main"))
        .unwrap()
        .modified()
        .unwrap();
    let known = KnownHistory::new(
        &repository.objects(PackLimits::default()).unwrap(),
        &[source.first],
        FetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let report = receive(
        request(&repository, &specs, &[], Reflog::Preserve),
        &source,
        &known,
    )
    .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
    .unwrap();
    assert_eq!(report.updates[0].kind, FetchUpdateKind::Unchanged);
    assert_eq!(report.pack_bytes, 0);
    assert!(report.references.is_empty());
    assert_eq!(
        fs::metadata(repository.git_dir().join("refs/remotes/origin/main"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    assert!(!repository.git_dir().join("logs").exists());
}

#[rstest]
#[case::tree("tree")]
#[case::blob("blob")]
fn remote_tracking_kind_changes_and_rewinds_agree_with_git(#[case] kind: &str) {
    let source = Source::new();
    let (_root, repository) = destination();
    let (_git_root, git_repository) = destination();
    source.set("refs/tags/value", source.second);
    let spec = "refs/tags/value:refs/remotes/origin/value";
    fetch(&repository, &source, &[spec]);
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            spec,
        ],
        b"",
    );
    let targets = [
        ("commit", source.first),
        ("tree", source.tree),
        ("blob", source.blob),
        ("tag", source.tag),
    ];
    let target = targets.into_iter().find(|(key, _)| *key == kind).unwrap().1;
    source.set("refs/tags/value", target);
    let report = fetch(&repository, &source, &[spec]);
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            spec,
        ],
        b"",
    );
    assert_eq!(report.updates[0].kind, FetchUpdateKind::Replace);
    assert_eq!(
        stored(&repository, "refs/remotes/origin/value"),
        Some(Target::Direct(target))
    );
    assert_eq!(
        git(repository.git_dir(), &["show-ref"], b""),
        git(git_repository.git_dir(), &["show-ref"], b"")
    );
    assert_available(&repository, target);
}

#[test]
fn tag_replacement_needs_authorization_as_well_as_git_force_syntax() {
    let source = Source::new();
    let (_root, repository) = destination();
    let (_git_root, git_repository) = destination();
    let spec = "refs/tags/v1:refs/tags/v1";
    fetch(&repository, &source, &[spec]);
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            spec,
        ],
        b"",
    );
    source.set("refs/tags/v1", source.blob);
    let rejected = request(
        &repository,
        &["+refs/tags/v1:refs/tags/v1"],
        &[],
        Reflog::Preserve,
    )
    .receive_local(
        source.root.path(),
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(
        rejected,
        Err(FetchWorkflowError::Plan(FetchPlanError::TagReplacement(_)))
    ));
    assert!(
        !command(
            git_repository.git_dir(),
            &[
                "fetch",
                "--no-tags",
                source.root.path().to_str().unwrap(),
                spec
            ],
            b""
        )
        .status
        .success()
    );
    assert_eq!(
        stored(&repository, "refs/tags/v1"),
        Some(Target::Direct(source.tag))
    );
    let ready = receive(
        request(
            &repository,
            &["+refs/tags/v1:refs/tags/v1"],
            &["refs/tags/v1"],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    let report = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            "+refs/tags/v1:refs/tags/v1",
        ],
        b"",
    );
    assert_eq!(report.updates[0].kind, FetchUpdateKind::ForcedTag);
    assert_eq!(
        git(repository.git_dir(), &["show-ref"], b""),
        git(git_repository.git_dir(), &["show-ref"], b"")
    );
}

#[test]
fn changed_server_advertisement_is_used_instead_of_preview() {
    let source = Source::new();
    let (_root, repository) = destination();
    let request = request(
        &repository,
        &["refs/heads/main:refs/remotes/origin/main"],
        &[],
        Reflog::Preserve,
    );
    source.set("refs/heads/main", source.second);
    let ready = receive(request, &source, &KnownHistory::default());
    assert_eq!(
        ready.updates()[0].mapping.source.as_ref().unwrap().id,
        source.second
    );
    ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        stored(&repository, "refs/remotes/origin/main"),
        Some(Target::Direct(source.second))
    );
}

#[test]
fn destination_race_keeps_installed_objects_and_does_not_overwrite_writer() {
    let source = Source::new();
    let (_root, repository) = destination();
    let ready = receive(
        request(
            &repository,
            &[
                "refs/heads/main:refs/remotes/origin/main",
                "refs/heads/topic:refs/remotes/origin/topic",
            ],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    let competing = repository
        .loose_objects()
        .write_blob(b"competing writer")
        .unwrap();
    repository
        .references()
        .unwrap()
        .update_without_reflog(
            &name("refs/remotes/origin/main"),
            Target::Direct(competing),
            Expected::Absent,
        )
        .unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Publication(TransactionError::Prepare { .. })
    ));
    assert!(error.report.installed.is_some());
    assert_eq!(
        stored(&repository, "refs/remotes/origin/main"),
        Some(Target::Direct(competing))
    );
    assert_eq!(stored(&repository, "refs/remotes/origin/topic"), None);
    assert_available(&repository, source.first);
}

#[test]
fn failed_installation_never_publishes_refs() {
    let source = Source::new();
    let (_root, repository) = destination();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    fs::remove_dir(repository.object_dir().join("pack")).unwrap();
    fs::write(repository.object_dir().join("pack"), b"obstruction").unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(*error.source, FetchFinishFailure::Installation(_)));
    assert!(error.report.installed.is_none());
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
}

#[test]
fn cancellation_before_installation_leaves_refs_and_objects_untouched() {
    let source = Source::new();
    let (_root, repository) = destination();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(true))
        .unwrap_err();
    let cause = std::error::Error::source(&error).unwrap();
    assert!(matches!(
        cause
            .downcast_ref::<Box<FetchFinishFailure>>()
            .map(Box::as_ref),
        Some(FetchFinishFailure::Installation(
            girt::fetch::FetchError::Cancelled
        ))
    ));
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Installation(girt::fetch::FetchError::Cancelled)
    ));
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
    assert_eq!(
        fs::read_dir(repository.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn cancelled_transfer_has_no_storage_effects() {
    let source = Source::new();
    let (_root, repository) = destination();
    let cancel = AtomicBool::new(false);
    let result = request(
        &repository,
        &["refs/heads/main:refs/remotes/origin/main"],
        &[],
        Reflog::Preserve,
    )
    .receive_local(
        source.root.path(),
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        |_| {
            cancel.store(true, Ordering::Relaxed);
            ControlFlow::Break(())
        },
    );
    assert!(matches!(
        result,
        Err(FetchWorkflowError::Transfer(
            girt::fetch::FetchError::Cancelled
        ))
    ));
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
    assert_eq!(
        fs::read_dir(repository.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn publication_lock_failure_keeps_installed_objects() {
    let source = Source::new();
    let (_root, repository) = destination();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    fs::write(
        repository.git_dir().join("packed-refs.lock"),
        b"competing lock",
    )
    .unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(error.report.installed.is_some());
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Publication(TransactionError::Prepare { .. })
    ));
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
    assert_available(&repository, source.first);
}

#[test]
fn head_alias_into_destination_is_rejected_after_transfer() {
    let source = Source::new();
    let (_root, repository) = destination();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    fs::write(
        repository.git_dir().join("HEAD"),
        b"ref: refs/remotes/origin/main\n",
    )
    .unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Safety(FetchPlanError::Head(_))
    ));
    assert!(error.report.installed.is_some());
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
}

#[test]
fn linked_worktree_branch_alias_cannot_bypass_destination_restriction() {
    let source = Source::new();
    let linked = tempfile::tempdir().unwrap();
    git(
        source.root.path(),
        &[
            "worktree",
            "add",
            linked.path().join("linked").to_str().unwrap(),
            "topic",
        ],
        b"",
    );
    git(
        source.root.path(),
        &[
            "symbolic-ref",
            "refs/heads/topic",
            "refs/remotes/origin/main",
        ],
        b"",
    );
    let repo = Repository::open(source.root.path()).unwrap();
    let result = FetchRequest::prepare(
        repo,
        Refspecs::parse(
            Direction::Fetch,
            [b"refs/heads/topic:refs/remotes/origin/main".as_slice()],
        )
        .unwrap(),
        BTreeSet::new(),
        Reflog::Preserve,
    );
    assert!(matches!(result, Err(FetchPlanError::Head(_))));
}

#[rstest]
#[case::commit(false)]
#[case::peeled_tag(true)]
fn commit_rewinds_require_force_and_authorization_like_git(#[case] tag: bool) {
    let source = Source::new();
    let (_root, repository) = destination();
    let (_git_root, git_repository) = destination();
    let target = [source.first, source.tag][usize::from(tag)];
    source.set("refs/tags/value", source.second);
    let spec = "refs/tags/value:refs/remotes/origin/value";
    fetch(&repository, &source, &[spec]);
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            spec,
        ],
        b"",
    );
    source.set("refs/tags/value", target);
    let ready = receive(
        request(&repository, &[spec], &[], Reflog::Preserve),
        &source,
        &KnownHistory::default(),
    );
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Update(girt::fetch::FetchUpdateError::NonFastForward(_))
    ));
    assert!(error.report.installed.is_some());
    assert!(
        !command(
            git_repository.git_dir(),
            &[
                "fetch",
                "--no-tags",
                source.root.path().to_str().unwrap(),
                spec
            ],
            b""
        )
        .status
        .success()
    );
    assert_eq!(
        stored(&repository, "refs/remotes/origin/value"),
        Some(Target::Direct(source.second))
    );
    let ready = receive(
        request(
            &repository,
            &["+refs/tags/value:refs/remotes/origin/value"],
            &["refs/remotes/origin/value"],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    let report = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    git(
        git_repository.git_dir(),
        &[
            "fetch",
            "--no-tags",
            source.root.path().to_str().unwrap(),
            "+refs/tags/value:refs/remotes/origin/value",
        ],
        b"",
    );
    assert_eq!(report.updates[0].kind, FetchUpdateKind::ForcedTracking);
    assert_eq!(
        git(repository.git_dir(), &["show-ref"], b""),
        git(git_repository.git_dir(), &["show-ref"], b"")
    );
}

#[test]
fn failed_index_installation_reports_possible_unindexed_pack() {
    let source = Source::new();
    let (_root, repository) = destination();
    let (_scratch, scratch) = destination();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    let checksum = ready
        .received()
        .install(&scratch, PackLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .checksum
        .unwrap();
    let basename = repository
        .object_dir()
        .join(format!("pack/pack-{checksum}"));
    fs::create_dir(basename.with_extension("idx")).unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(*error.source, FetchFinishFailure::Installation(_)));
    assert!(error.report.installed.is_none());
    assert!(basename.with_extension("pack").is_file());
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
}

#[test]
fn missing_known_dependency_prevents_publication() {
    let source = Source::new();
    let (_root, repository) = destination();
    let specs = ["refs/heads/main:refs/remotes/origin/main"];
    fetch(&repository, &source, &specs);
    let known = KnownHistory::new(
        &repository.objects(PackLimits::default()).unwrap(),
        &[source.first],
        FetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/other"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &known,
    );
    fs::remove_dir_all(repository.object_dir().join("pack")).unwrap();
    fs::create_dir(repository.object_dir().join("pack")).unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Installation(girt::fetch::FetchError::Missing(_))
    ));
    assert_eq!(stored(&repository, "refs/remotes/origin/other"), None);
}

#[test]
fn explicit_reflog_identity_is_used_only_for_changed_refs() {
    let source = Source::new();
    let (_root, repository) = destination();
    let committer = girt::Signature {
        name: b"Fetch Writer".to_vec(),
        email: b"fetch@example.com".to_vec(),
        seconds: 1700000010,
        offset_minutes: 0,
    };
    let reflog = Reflog::Append {
        committer: committer.clone(),
        message: b"explicit fetch".to_vec(),
    };
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            reflog,
        ),
        &source,
        &KnownHistory::default(),
    );
    let report = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let log = repository
        .references()
        .unwrap()
        .reflog(&name("refs/remotes/origin/main"))
        .unwrap()
        .unwrap();
    assert_eq!(
        report.references[0].logs[0].1,
        girt::refs::LogOutcome::Appended
    );
    assert_eq!(log[0].committer, committer);
    assert_eq!(log[0].new, source.first);
    assert_eq!(log[0].message, b"explicit fetch");
}

#[test]
fn ancestry_budget_failure_leaves_installed_objects_without_advancing_ref() {
    let source = Source::new();
    let (_root, repository) = destination();
    let specs = ["refs/heads/main:refs/remotes/origin/main"];
    fetch(&repository, &source, &specs);
    source.set("refs/heads/main", source.second);
    let ready = receive(
        request(&repository, &specs, &[], Reflog::Preserve),
        &source,
        &KnownHistory::default(),
    );
    let limits = FetchUpdateLimits {
        history: girt::HistoryLimits {
            max_commits: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let error = ready.finish(limits, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Update(girt::fetch::FetchUpdateError::History(
            girt::HistoryError::Limit(_)
        ))
    ));
    assert!(error.report.installed.is_some());
    assert_available(&repository, source.second);
    assert_eq!(
        stored(&repository, "refs/remotes/origin/main"),
        Some(Target::Direct(source.first))
    );
}

#[test]
fn tag_peeling_budget_failure_prevents_publication() {
    let source = Source::new();
    let (_root, repository) = destination();
    source.set("refs/tags/value", source.tag);
    let specs = ["refs/tags/value:refs/remotes/origin/value"];
    fetch(&repository, &source, &specs);
    source.set("refs/tags/value", source.second);
    let ready = receive(
        request(&repository, &specs, &[], Reflog::Preserve),
        &source,
        &KnownHistory::default(),
    );
    let limits = FetchUpdateLimits {
        max_tag_depth: 0,
        ..Default::default()
    };
    let error = ready.finish(limits, &AtomicBool::new(false)).unwrap_err();
    assert!(matches!(
        *error.source,
        FetchFinishFailure::Update(girt::fetch::FetchUpdateError::Peel(error))
            if matches!(*error.source, girt::PeelFailure::Depth)
    ));
    assert_eq!(
        stored(&repository, "refs/remotes/origin/value"),
        Some(Target::Direct(source.tag))
    );
}

#[test]
fn corrupt_loose_shadow_cannot_receive_a_published_ref() {
    let source = Source::new();
    let (_root, repository) = destination();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    // A corrupt descendant matters even when the selected commit itself reads correctly.
    let hex = source.blob.to_string();
    fs::create_dir_all(repository.object_dir().join(&hex[..2])).unwrap();
    fs::write(
        repository.object_dir().join(&hex[..2]).join(&hex[2..]),
        b"corrupt loose shadow",
    )
    .unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(error.report.installed.is_some());
    assert!(matches!(
        *error.source,
        FetchFinishFailure::BeforePublication(girt::fetch::FetchError::LocalRead { .. })
    ));
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
}

#[test]
fn installed_graph_verification_budget_is_explicit() {
    let source = Source::new();
    let (_root, repository) = destination();
    let ready = receive(
        request(
            &repository,
            &["refs/heads/main:refs/remotes/origin/main"],
            &[],
            Reflog::Preserve,
        ),
        &source,
        &KnownHistory::default(),
    );
    let limits = FetchUpdateLimits {
        verification: FetchLimits {
            max_known_objects: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let error = ready.finish(limits, &AtomicBool::new(false)).unwrap_err();
    assert!(error.report.installed.is_some());
    assert!(matches!(
        *error.source,
        FetchFinishFailure::BeforePublication(girt::fetch::FetchError::Limit(_))
    ));
    assert_eq!(stored(&repository, "refs/remotes/origin/main"), None);
}
