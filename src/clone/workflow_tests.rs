use rstest::rstest;

use super::*;
use crate::fetch::FetchFinishFailure;
use crate::{Commit, CommitFields, Signature, Tree};

fn ready() -> (tempfile::TempDir, CloneReady) {
    let root = tempfile::tempdir().unwrap();
    let source = Repository::init(root.path().join("source"), InitKind::Bare).unwrap();
    let objects = source.loose_objects().unwrap();
    let tree = objects.write_tree(&Tree::new(vec![]).unwrap()).unwrap();
    let who = Signature {
        name: b"Clone".to_vec(),
        email: b"clone@example.com".to_vec(),
        seconds: 1700000000,
        offset_minutes: 0,
    };
    let commit = Commit::new(CommitFields {
        tree,
        parents: vec![],
        author: who.clone(),
        committer: who,
        extra_headers: vec![],
        message: b"original clone fixture\n".to_vec(),
    })
    .unwrap();
    let id = objects.write_commit(&commit).unwrap();
    source
        .references()
        .unwrap()
        .update_without_reflog(
            &RefName::new("refs/heads/main").unwrap(),
            Target::Direct(id),
            Expected::Absent,
        )
        .unwrap();
    let request = CloneRequest::prepare(
        root.path().join("copy"),
        InitKind::Bare,
        b"fixture",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let ready = request
        .receive_local(
            source.git_dir(),
            FetchLimits::default(),
            TransportControl::new(&AtomicBool::new(false)),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
    (root, ready)
}
fn initialized(ready: &CloneReady) -> (Repository, CloneReport) {
    let repo = Repository::init(&ready.request.destination, ready.request.kind).unwrap();
    let report = CloneReport {
        destination: ready.request.destination.clone(),
        reserved: true,
        initialized: true,
        fetch: None,
        configured: false,
        references: vec![],
        repository: None,
        head: ready.head.clone(),
    };
    (repo, report)
}

#[rstest]
#[case::empty(b"".as_slice())]
#[case::nul(b"bad\0url".as_slice())]
#[case::cr(b"bad\rurl".as_slice())]
fn invalid_url_has_no_effects(#[case] url: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("copy");
    assert!(matches!(
        CloneRequest::prepare(
            &path,
            InitKind::Bare,
            url,
            BranchSelection::Default,
            Reflog::Preserve
        ),
        Err(CloneFailure::Url)
    ));
    assert!(!path.exists());
}

#[test]
fn cancellation_before_finish_leaves_destination_absent() {
    let (_root, ready) = ready();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(true))
        .unwrap_err();
    assert!(!error.report.reserved);
    assert!(!error.report.destination.exists());
}

#[test]
fn dropping_ready_does_not_initialize() {
    let (_root, ready) = ready();
    let path = ready.request.destination.clone();
    drop(ready);
    assert!(!path.exists());
}

#[test]
fn destination_race_preserves_other_content() {
    let (_root, ready) = ready();
    fs::create_dir(&ready.request.destination).unwrap();
    fs::write(ready.request.destination.join("keep"), b"other writer").unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.source, CloneFailure::Exists(_)));
    assert!(!error.report.reserved);
    assert_eq!(
        fs::read(error.report.destination.join("keep")).unwrap(),
        b"other writer"
    );
}

#[test]
fn config_lock_failure_retains_fetch_and_initial_head() {
    let (_root, ready) = ready();
    let (repo, mut report) = initialized(&ready);
    fs::write(repo.git_dir().join("config.lock"), b"other writer").unwrap();
    let result = ready.finish_in(
        repo,
        FetchUpdateLimits::default(),
        &AtomicBool::new(false),
        &mut report,
    );
    assert!(matches!(result, Err(CloneFailure::Configuration(_))));
    assert!(report.fetch.as_ref().unwrap().installed.is_some());
    assert!(!report.configured);
    assert_eq!(
        fs::read(report.destination.join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert!(!report.destination.join("refs/heads/main").exists());
    assert!(report.destination.join("refs/remotes/origin/main").exists());
    assert_eq!(
        fs::read(report.destination.join("config.lock")).unwrap(),
        b"other writer"
    );
}

#[test]
fn head_lock_failure_retains_config_and_fetch_without_local_branch() {
    let (_root, ready) = ready();
    let (repo, mut report) = initialized(&ready);
    fs::write(repo.git_dir().join("HEAD.lock"), b"keep").unwrap();
    let result = ready.finish_in(
        repo,
        FetchUpdateLimits::default(),
        &AtomicBool::new(false),
        &mut report,
    );
    assert!(matches!(result, Err(CloneFailure::Publication(_))));
    assert!(report.configured);
    assert!(report.fetch.is_some());
    assert!(report.repository.is_none());
    assert!(!report.destination.join("refs/heads/main").exists());
    assert_eq!(
        fs::read(report.destination.join("HEAD.lock")).unwrap(),
        b"keep"
    );
}

#[test]
fn verification_limit_failure_retains_installed_objects_without_refs() {
    let (_root, ready) = ready();
    let limits = FetchUpdateLimits {
        verification: FetchLimits {
            max_known_objects: 0,
            ..FetchLimits::default()
        },
        ..FetchUpdateLimits::default()
    };
    let error = ready.finish(limits, &AtomicBool::new(false)).unwrap_err();
    let CloneFailure::Fetch(fetch) = error.source else {
        panic!("expected fetch failure")
    };
    assert!(matches!(
        fetch.source,
        FetchFinishFailure::BeforePublication(_)
    ));
    assert!(fetch.report.installed.is_some());
    assert!(error.report.initialized);
    assert!(!error.report.configured);
    assert!(
        Repository::open(&error.report.destination)
            .unwrap()
            .references()
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn index_install_failure_reports_unindexed_residual_pack() {
    let (_root, ready) = ready();
    let (repo, mut report) = initialized(&ready);
    let staging = tempfile::tempdir().unwrap();
    let copy = Repository::init(staging.path().join("repo"), InitKind::Bare).unwrap();
    let installed = ready
        .received
        .install(&copy, Default::default(), &AtomicBool::new(false))
        .unwrap();
    let basename = repo
        .object_dir()
        .join("pack")
        .join(format!("pack-{}", installed.checksum.unwrap()));
    fs::create_dir(basename.with_extension("idx")).unwrap();
    let result = ready.finish_in(
        repo,
        FetchUpdateLimits::default(),
        &AtomicBool::new(false),
        &mut report,
    );
    let Err(CloneFailure::Fetch(error)) = result else {
        panic!("expected fetch failure")
    };
    assert!(matches!(error.source, FetchFinishFailure::Installation(_)));
    assert!(error.report.installed.is_none());
    assert!(basename.with_extension("pack").is_file());
    assert!(!report.configured);
}

#[test]
fn transfer_error_before_selection_keeps_original_cause() {
    let result = selected::<()>(None, Err(FetchError::Cancelled));
    assert!(matches!(
        result,
        Err(CloneTransferError::Transfer(FetchError::Cancelled))
    ));
}

#[test]
fn urls_are_not_in_request_debug() {
    let root = tempfile::tempdir().unwrap();
    let request = CloneRequest::prepare(
        root.path().join("copy"),
        InitKind::Bare,
        b"secret-token",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    assert!(!format!("{request:?}").contains("secret-token"));
}

#[test]
fn cancelled_after_initialization_keeps_empty_repository() {
    let (_root, ready) = ready();
    let (repo, mut report) = initialized(&ready);
    let result = ready.finish_in(
        repo,
        FetchUpdateLimits::default(),
        &AtomicBool::new(true),
        &mut report,
    );
    let Err(CloneFailure::Fetch(error)) = result else {
        panic!("expected fetch cancellation")
    };
    assert!(matches!(
        error.source,
        FetchFinishFailure::Installation(FetchError::Cancelled)
    ));
    assert!(report.initialized);
    assert!(!report.configured);
    assert!(report.destination.join("HEAD").exists());
    assert_eq!(
        fs::read_dir(report.destination.join("objects/pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn fetch_reference_lock_failure_retains_pack_without_configuration() {
    let (_root, ready) = ready();
    let (repo, mut report) = initialized(&ready);
    fs::write(repo.git_dir().join("packed-refs.lock"), b"keep").unwrap();
    let result = ready.finish_in(
        repo,
        FetchUpdateLimits::default(),
        &AtomicBool::new(false),
        &mut report,
    );
    let Err(CloneFailure::Fetch(error)) = result else {
        panic!("expected fetch publication failure")
    };
    assert!(matches!(error.source, FetchFinishFailure::Publication(_)));
    assert!(error.report.installed.is_some());
    assert!(!report.configured);
    assert_eq!(
        fs::read(report.destination.join("packed-refs.lock")).unwrap(),
        b"keep"
    );
}

#[test]
fn missing_destination_parent_is_not_created() {
    let (_root, mut ready) = ready();
    ready.request.destination = ready.request.destination.join("missing/copy");
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.source, CloneFailure::Io(_)));
    assert!(!error.report.reserved);
    assert!(!error.report.initialized);
    assert!(!error.report.destination.parent().unwrap().exists());
}
