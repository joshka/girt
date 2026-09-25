use rstest::rstest;

use super::*;
use crate::fetch::FetchFinishFailure;
use crate::{Commit, CommitFields, ObjectFormat, ObjectKind, PackObject, Signature};

fn ready() -> (tempfile::TempDir, CloneReady) {
    let root = tempfile::tempdir().unwrap();
    let tree = ObjectFormat::Sha1.hash_object(ObjectKind::Tree, b"");
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
    let encoded = commit.encode();
    let id = ObjectFormat::Sha1.hash_object(ObjectKind::Commit, &encoded);
    let mut pack = Vec::new();
    crate::write_pack(
        ObjectFormat::Sha1,
        &[
            PackObject {
                id: tree,
                kind: ObjectKind::Tree,
                data: b"",
            },
            PackObject {
                id,
                kind: ObjectKind::Commit,
                data: &encoded,
            },
        ],
        &mut pack,
        &mut Vec::new(),
        Default::default(),
    )
    .unwrap();
    // The completion tests need a validated transfer, not an owned child process.
    let mut response =
        packet(format!("{id} HEAD\0side-band-64k symref=HEAD:refs/heads/main\n").as_bytes());
    response.extend(packet(format!("{id} refs/heads/main\n").as_bytes()));
    response.extend(b"0000");
    response.extend(packet(b"NAK\n"));
    let mut band = vec![1];
    band.extend(pack);
    response.extend(packet(&band));
    response.extend(b"0000");
    let request = CloneRequest::prepare_tracking(
        root.path().join("copy"),
        InitKind::Bare,
        b"fixture",
        BranchSelection::Default,
        Reflog::Preserve,
    )
    .unwrap();
    let mut plan = None;
    let received = crate::fetch::receive(
        &mut response.as_slice(),
        &mut Vec::new(),
        |advertisement| super::super::plan::select(request.plan(advertisement), &mut plan),
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    let (plan, received) = selected(plan, received).unwrap();
    (
        root,
        CloneReady {
            request,
            head: plan.head,
            received,
        },
    )
}

fn packet(bytes: &[u8]) -> Vec<u8> {
    let mut packet = format!("{:04x}", bytes.len() + 4).into_bytes();
    packet.extend_from_slice(bytes);
    packet
}

fn initialized(ready: &CloneReady) -> (Repository, CloneReport) {
    let repo = Repository::init(
        crate::ObjectFormat::Sha1,
        &ready.request.destination,
        ready.request.kind,
    )
    .unwrap();
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
        CloneRequest::prepare_tracking(
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
        *fetch.source,
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
    let copy = Repository::init(
        crate::ObjectFormat::Sha1,
        staging.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
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
    assert!(matches!(*error.source, FetchFinishFailure::Installation(_)));
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
    let request = CloneRequest::prepare_tracking(
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
        *error.source,
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
    assert!(matches!(*error.source, FetchFinishFailure::Publication(_)));
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

#[test]
fn detached_append_dependency_lock_failure_keeps_initial_head_and_completed_fetch() {
    let (_root, mut ready) = ready();
    let CloneHead::Branch { id, .. } = ready.head else {
        panic!("fixture branch")
    };
    ready.head = CloneHead::Detached(id);
    ready.request.reflog = Reflog::Append {
        committer: Signature {
            name: b"Clone".to_vec(),
            email: b"clone@example.com".to_vec(),
            seconds: 1700000000,
            offset_minutes: 0,
        },
        message: b"detach".to_vec(),
    };
    let (repo, mut report) = initialized(&ready);
    fs::write(
        repo.git_dir().join("refs/heads/main.lock"),
        b"foreign writer",
    )
    .unwrap();
    let result = ready.finish_in(
        repo,
        FetchUpdateLimits::default(),
        &AtomicBool::new(false),
        &mut report,
    );
    assert!(matches!(
        result,
        Err(CloneFailure::Publication(
            crate::refs::TransactionError::Prepare {
                source: crate::refs::ReferenceError::Locked(_),
                ..
            }
        ))
    ));
    assert!(report.configured);
    assert!(report.fetch.as_ref().unwrap().installed.is_some());
    assert!(report.repository.is_none());
    assert_eq!(
        fs::read(report.destination.join("HEAD")).unwrap(),
        b"ref: refs/heads/main\n"
    );
    assert!(!report.destination.join("logs/HEAD").exists());
    assert_eq!(
        fs::read(report.destination.join("refs/heads/main.lock")).unwrap(),
        b"foreign writer"
    );
}
