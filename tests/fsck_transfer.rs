//! Original malformed tree transfers compared with isolated Git policy enforcement.
#[path = "support/apply_git.rs"]
mod support;
use std::sync::atomic::AtomicBool;

use girt::fetch::{FetchLimits, FetchRequest, KnownHistory, LocalFetchObserver};
use girt::fsck::{MessageId, Severity, TransferError};
use girt::refs::Reflog;
use girt::remote::Remote;
use girt::transport::TransportControl;
use girt::{ObjectFormat, ObjectId, Repository};

fn local_path(path: &std::path::Path) -> &str {
    let path = path.to_str().unwrap();
    path.strip_prefix(r"\\?\").unwrap_or(path)
}

fn source(root: &std::path::Path) -> (Repository, ObjectId, ObjectId) {
    support::git(root, &["init", "--bare", "--template=", "."], b"");
    let blob = support::git(root, &["hash-object", "-w", "--stdin"], b"content");
    let blob = ObjectId::from_hex(
        ObjectFormat::Sha1,
        std::str::from_utf8(&blob).unwrap().trim(),
    )
    .unwrap();
    let tree = support::git(
        root,
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &[b"100644 .git\0".as_slice(), blob.as_bytes()].concat(),
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let commit = support::git(root, &["commit-tree", tree], b"legacy tree\n");
    let commit = std::str::from_utf8(&commit).unwrap().trim();
    support::git(root, &["update-ref", "refs/heads/main", commit], b"");
    support::git(root, &["symbolic-ref", "HEAD", "refs/heads/main"], b"");
    (
        Repository::open(root).unwrap(),
        ObjectId::from_hex(ObjectFormat::Sha1, tree).unwrap(),
        ObjectId::from_hex(ObjectFormat::Sha1, commit).unwrap(),
    )
}

fn make_destination(
    root: &std::path::Path,
    source: &Repository,
    settings: &[(&str, &str)],
) -> Repository {
    std::fs::create_dir(root).unwrap();
    support::git(root, &["init", "--bare", "--template=", "."], b"");
    support::git(
        root,
        &["config", "remote.origin.url", local_path(source.git_dir())],
        b"",
    );
    support::git(
        root,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
        b"",
    );
    for &(key, value) in settings {
        support::git(root, &["config", key, value], b"");
    }
    Repository::open(root).unwrap()
}

#[test]
fn fetch_policy_precedes_installation_and_matches_git() {
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    std::fs::create_dir(&source_root).unwrap();
    let (source, _, tip) = source(&source_root);
    let cases: &[(&[(&str, &str)], bool)] = &[
        (&[("fetch.fsckObjects", "true")], false),
        (&[("transfer.fsckObjects", "true")], false),
        (
            &[
                ("transfer.fsckObjects", "true"),
                ("fetch.fsckObjects", "false"),
            ],
            true,
        ),
        (
            &[
                ("fetch.fsckObjects", "true"),
                ("fetch.fsck.hasDotgit", "warn"),
            ],
            true,
        ),
        (
            &[("fetch.fsckObjects", "true"), ("fsck.hasDotgit", "ignore")],
            false,
        ),
    ];
    for (case, &(settings, accepted)) in cases.iter().enumerate() {
        let destination =
            make_destination(&root.path().join(format!("girt-{case}")), &source, settings);
        let git_destination =
            make_destination(&root.path().join(format!("git-{case}")), &source, settings);
        let url = local_path(source.git_dir());
        let oracle = support::git_output(
            git_destination.git_dir(),
            &["fetch", url, "refs/heads/main:refs/remotes/origin/main"],
            b"",
        );
        assert_eq!(oracle.status.success(), accepted);
        let remote = Remote::find(destination.config(), b"origin")
            .unwrap()
            .unwrap();
        let request = FetchRequest::prepare(
            destination.clone(),
            remote.fetch_refspecs().clone(),
            Default::default(),
            Reflog::Preserve,
        )
        .unwrap();
        let cancel = AtomicBool::new(false);
        let ready = request
            .receive_local(
                source.git_dir(),
                &KnownHistory::default(),
                FetchLimits::default(),
                TransportControl::new(&cancel),
                LocalFetchObserver::default(),
            )
            .unwrap();
        let result = ready.finish(Default::default(), &cancel);
        assert_eq!(result.is_ok(), accepted);
        if let Ok(report) = result {
            if let Some(integrity) = report.integrity {
                assert!(!integrity.has_errors());
                assert!(
                    integrity
                        .findings
                        .iter()
                        .all(|finding| finding.finding.severity == Severity::Warning)
                );
            }
            assert!(destination.objects().unwrap().contains(tip).unwrap());
        } else {
            let error = result.unwrap_err();
            assert!(
                matches!(error.source.as_ref(), girt::fetch::FetchFinishFailure::Integrity(source) if matches!(source.as_ref(), TransferError::Rejected(report) if report.findings.iter().any(|finding| finding.finding.message_id == MessageId::HasDotgit)))
            );
            assert!(error.report.installed.is_none());
            assert!(!destination.objects().unwrap().contains(tip).unwrap());
        }
    }
}

#[test]
fn receive_policy_matches_git_and_refuses_before_object_installation() {
    use girt::push::{
        LocalPushRequest, PreparedPush, PushCommand, PushError, PushFailure, PushLimits, send_local,
    };
    use girt::refs::RefName;
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    std::fs::create_dir(&source_root).unwrap();
    let (source, _, tip) = source(&source_root);
    let cases: &[(&[(&str, &str)], bool)] = &[
        (&[("receive.fsckObjects", "true")], false),
        (&[("transfer.fsckObjects", "true")], false),
        (
            &[
                ("transfer.fsckObjects", "true"),
                ("receive.fsckObjects", "false"),
            ],
            true,
        ),
        (
            &[
                ("receive.fsckObjects", "true"),
                ("receive.fsck.hasDotgit", "warn"),
            ],
            true,
        ),
        (
            &[
                ("receive.fsckObjects", "true"),
                ("fsck.hasDotgit", "ignore"),
            ],
            false,
        ),
    ];
    for (case, &(settings, accepted)) in cases.iter().enumerate() {
        let destination =
            make_destination(&root.path().join(format!("girt-{case}")), &source, settings);
        let git_destination =
            make_destination(&root.path().join(format!("git-{case}")), &source, settings);
        let oracle = support::git_output(
            source.git_dir(),
            &[
                "push",
                local_path(git_destination.git_dir()),
                "refs/heads/main:refs/heads/main",
            ],
            b"",
        );
        assert_eq!(
            oracle.status.success(),
            accepted,
            "{}",
            String::from_utf8_lossy(&oracle.stderr)
        );
        let cancel = AtomicBool::new(false);
        let objects = source.objects().unwrap();
        let command = PushCommand::new(RefName::new("refs/heads/main").unwrap(), None, tip);
        let prepared =
            PreparedPush::new(&objects, vec![command], &[], PushLimits::default(), &cancel)
                .unwrap();
        let result = send_local(
            destination.git_dir(),
            LocalPushRequest::new(&prepared),
            TransportControl::new(&cancel),
            |_| {},
        );
        assert_eq!(result.is_ok(), accepted, "{result:?}");
        if let Err(error) = result {
            assert!(
                matches!(error, PushError::NotSent(PushFailure::Integrity(source)) if matches!(source.as_ref(), TransferError::Rejected(report) if report.has_errors()))
            );
            assert!(!destination.objects().unwrap().contains(tip).unwrap());
            assert!(!destination.git_dir().join("refs/heads/main").exists());
        }
    }
}

#[test]
fn explicit_received_pack_check_obeys_limits_and_cancellation() {
    use girt::fsck::{Policy, TransferLimits};
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    std::fs::create_dir(&source_root).unwrap();
    let (source, _, tip) = source(&source_root);
    let destination = make_destination(&root.path().join("destination"), &source, &[]);
    let remote = Remote::find(destination.config(), b"origin")
        .unwrap()
        .unwrap();
    let cancel = AtomicBool::new(false);
    let ready = FetchRequest::prepare(
        destination.clone(),
        remote.fetch_refspecs().clone(),
        Default::default(),
        Reflog::Preserve,
    )
    .unwrap()
    .receive_local(
        source.git_dir(),
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        LocalFetchObserver::default(),
    )
    .unwrap();
    let received = ready.received();
    let policy = Policy::default();
    let report = received.check_fsck(&policy).unwrap();
    assert!(report.has_errors());
    assert_eq!(report.checked_objects, 3);
    let mut limits = TransferLimits::default();
    limits.max_objects = 0;
    assert!(matches!(
        received.check_fsck_with(&policy, limits, &cancel),
        Err(TransferError::Limit("objects"))
    ));
    limits = TransferLimits::default();
    limits.max_bytes = 0;
    assert!(matches!(
        received.check_fsck_with(&policy, limits, &cancel),
        Err(TransferError::Limit("decoded bytes"))
    ));
    limits = TransferLimits::default();
    limits.max_findings = 0;
    assert!(matches!(
        received.check_fsck_with(&policy, limits, &cancel),
        Err(TransferError::Limit("findings"))
    ));
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        received.check_fsck_with(&policy, TransferLimits::default(), &cancelled),
        Err(TransferError::Cancelled)
    ));
    assert!(!destination.objects().unwrap().contains(tip).unwrap());
}

#[test]
fn configured_clone_rejects_before_creating_destination() {
    use girt::config::ConfigInputs;
    use girt::workflow::{CloneError, CloneOptions, Environment, NoCallbacks};
    let root = tempfile::tempdir().unwrap();
    let source_root = root.path().join("source");
    std::fs::create_dir(&source_root).unwrap();
    let (source, tree, _) = source(&source_root);
    let oracle = support::git_output(
        root.path(),
        &[
            "-c",
            "fetch.fsckObjects=true",
            "clone",
            "--no-local",
            "--bare",
            local_path(source.git_dir()),
            "git-clone",
        ],
        b"",
    );
    assert!(!oracle.status.success());
    std::fs::write(root.path().join("skip"), format!("{tree}\n")).unwrap();
    let skipped = support::git_output(
        root.path(),
        &[
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "fetch.fsck.skipList=skip",
            "clone",
            "--no-local",
            "--bare",
            local_path(source.git_dir()),
            "git-skipped",
        ],
        b"",
    );
    assert!(
        skipped.status.success(),
        "{}",
        String::from_utf8_lossy(&skipped.stderr)
    );
    let inputs = ConfigInputs::default().with_command(Some(
        girt::Config::parse(b"[fetch]\nfsckObjects=true\n").unwrap(),
    ));
    let mut options = CloneOptions::default();
    options.kind = girt::InitKind::Bare;
    let destination = root.path().join("clone");
    let error = girt::workflow::clone_with(
        local_path(source.git_dir()),
        &destination,
        &options,
        &inputs,
        &Environment::default().with_current_dir(root.path()),
        &mut NoCallbacks,
    )
    .unwrap_err();
    assert!(
        matches!(error, CloneError::Integrity(source) if matches!(source.as_ref(), TransferError::Rejected(report) if report.has_errors()))
    );
    assert!(!destination.exists());
    let inputs = ConfigInputs::default().with_command(Some(
        girt::Config::parse(b"[fetch]\nfsckObjects=true\n[fetch \"fsck\"]\nskipList=skip\n")
            .unwrap(),
    ));
    let accepted = girt::workflow::clone_with(
        local_path(source.git_dir()),
        root.path().join("girt-skipped"),
        &options,
        &inputs,
        &Environment::default().with_current_dir(root.path()),
        &mut NoCallbacks,
    )
    .unwrap();
    assert!(
        !accepted
            .clone
            .fetch
            .unwrap()
            .integrity
            .unwrap()
            .has_errors()
    );
}

#[test]
fn transfer_skip_list_and_unknown_messages_are_reported_without_installing() {
    use girt::fsck::Scope;
    let root = tempfile::tempdir().unwrap();
    let (source, tree, tip) = source(root.path());
    let destination = make_destination(&root.path().join("destination"), &source, &[]);
    let remote = Remote::find(destination.config(), b"origin")
        .unwrap()
        .unwrap();
    let cancel = AtomicBool::new(false);
    let ready = FetchRequest::prepare(
        destination.clone(),
        remote.fetch_refspecs().clone(),
        Default::default(),
        Reflog::Preserve,
    )
    .unwrap()
    .receive_local(
        source.git_dir(),
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        LocalFetchObserver::default(),
    )
    .unwrap();
    std::fs::write(
        root.path().join("skip"),
        format!("# historical tree\n{tree}\n"),
    )
    .unwrap();
    let config = girt::Config::parse(
        b"[fetch]\nfsckObjects=true\n[fetch \"fsck\"]\nskipList=skip\nunknownFutureId=warn\n",
    )
    .unwrap();
    let report = ready
        .received()
        .enforce_fsck(&config, Scope::Fetch, root.path(), &cancel)
        .unwrap()
        .unwrap();
    assert!(!report.has_errors());
    assert!(report.findings.is_empty());
    assert_eq!(report.unknown_messages, [b"unknownFutureId".to_vec()]);
    assert!(!destination.objects().unwrap().contains(tip).unwrap());
    // The retained installation path must enforce the same policy before creating its marker.
    support::git(
        destination.git_dir(),
        &["config", "fetch.fsckObjects", "true"],
        b"",
    );
    // FetchRequest captures resolved configuration; prepare again with a reopened repository.
    let destination = Repository::open(destination.git_dir()).unwrap();
    let remote = Remote::find(destination.config(), b"origin")
        .unwrap()
        .unwrap();
    let ready = FetchRequest::prepare(
        destination.clone(),
        remote.fetch_refspecs().clone(),
        Default::default(),
        Reflog::Preserve,
    )
    .unwrap()
    .receive_local(
        source.git_dir(),
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&cancel),
        LocalFetchObserver::default(),
    )
    .unwrap();
    let error = ready
        .install_retained(Default::default(), &cancel)
        .unwrap_err();
    assert!(matches!(
        error.source.as_ref(),
        girt::fetch::FetchFinishFailure::Integrity(_)
    ));
    assert!(error.retention.is_none());
    assert!(error.report.installed.is_none());
    assert!(!destination.objects().unwrap().contains(tip).unwrap());
}
