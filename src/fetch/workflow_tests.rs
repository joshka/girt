use rstest::rstest;

use super::*;
use crate::InitKind;
use crate::fetch::{AdvertisedRef, LocalFetchProgress};

fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}
fn id(byte: u8) -> ObjectId {
    ObjectId::Sha1([byte; 20])
}
fn request(specs: &[&str]) -> (tempfile::TempDir, FetchRequest) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        crate::ObjectFormat::Sha1,
        root.path().join("repo"),
        InitKind::Bare,
    )
    .unwrap();
    let specs =
        Refspecs::parse(Direction::Fetch, specs.iter().map(|spec| spec.as_bytes())).unwrap();
    let request = FetchRequest::prepare(repo, specs, BTreeSet::new(), Reflog::Preserve).unwrap();
    (root, request)
}
fn advertisement() -> Advertisement {
    Advertisement {
        refs: vec![
            AdvertisedRef {
                name: name("refs/heads/main"),
                id: id(1),
                peeled: false,
            },
            AdvertisedRef {
                name: name("refs/heads/private"),
                id: id(2),
                peeled: false,
            },
        ],
        capabilities: vec![],
    }
}

#[test]
fn wildcard_exclusion_and_source_only_keep_advertised_ids() {
    let (_root, request) = request(&[
        "refs/heads/*:refs/remotes/origin/*",
        "^refs/heads/private",
        "refs/heads/main",
    ]);
    let plan = request.plan(&advertisement()).unwrap();
    assert_eq!(plan.len(), 2);
    assert_eq!(
        plan[0].mapping.destination,
        Some(name("refs/remotes/origin/main"))
    );
    assert_eq!(plan[0].kind, FetchUpdateKind::Create);
    assert_eq!(plan[1].kind, FetchUpdateKind::SourceOnly);
    assert_eq!(plan[1].mapping.source.as_ref().unwrap().id, id(1));
}

#[rstest]
#[case::branch("refs/heads/main")]
#[case::private("refs/worktree/main")]
#[case::other("refs/custom/main")]
fn rejects_unsupported_destinations(#[case] destination: &str) {
    let spec = format!("+refs/heads/main:{destination}");
    let (_root, request) = request(&[&spec]);
    assert!(matches!(
        request.plan(&advertisement()),
        Err(FetchPlanError::Destination(_))
    ));
}

#[rstest]
#[case::create(None, false, false, FetchUpdateKind::Create)]
#[case::same(Some(id(2)), false, false, FetchUpdateKind::Unchanged)]
#[case::authorized(Some(id(1)), true, true, FetchUpdateKind::ForcedTag)]
fn tag_decisions(
    #[case] old: Option<ObjectId>,
    #[case] force: bool,
    #[case] authorized: bool,
    #[case] expected: FetchUpdateKind,
) {
    assert_eq!(
        update_kind(&name("refs/tags/v1"), old, id(2), force, authorized, true).unwrap(),
        expected
    );
}

#[rstest]
#[case::neither(false, false)]
#[case::syntax_only(true, false)]
#[case::authorization_only(false, true)]
fn tag_replacement_requires_both(#[case] force: bool, #[case] authorized: bool) {
    assert!(matches!(
        update_kind(
            &name("refs/tags/v1"),
            Some(id(1)),
            id(2),
            force,
            authorized,
            true
        ),
        Err(FetchPlanError::TagReplacement(_))
    ));
}

#[test]
fn remote_tracking_replacement_defers_object_checks() {
    assert_eq!(
        update_kind(
            &name("refs/remotes/origin/main"),
            Some(id(1)),
            id(2),
            false,
            false,
            false
        )
        .unwrap(),
        FetchUpdateKind::Replace
    );
}

#[test]
fn additional_tag_namespace_has_exact_boundary() {
    let spec = "+refs/heads/main:refs/jj/remote-tags/origin/main";
    let (_root, initial) = request(&[spec]);
    assert!(matches!(
        initial.plan(&advertisement()),
        Err(FetchPlanError::Destination(_))
    ));
    let planned = initial.with_tag_destination_namespace(name("refs/jj/remote-tags/origin"));
    assert_eq!(
        planned.plan(&advertisement()).unwrap()[0].kind,
        FetchUpdateKind::Create
    );

    let spec = "+refs/heads/main:refs/jj/remote-tags/origin-other/main";
    let (_root, request) = request(&[spec]);
    let request = request.with_tag_destination_namespace(name("refs/jj/remote-tags/origin"));
    assert!(matches!(
        request.plan(&advertisement()),
        Err(FetchPlanError::Destination(_))
    ));
}

#[test]
fn additional_tag_namespace_requires_force_and_authorization() {
    let dest = name("refs/jj/remote-tags/origin/v1");
    let (_root, mut request) = request(&["+refs/heads/main:refs/jj/remote-tags/origin/v1"]);
    request.snapshot.insert(dest.clone(), Target::Direct(id(2)));
    request = request.with_tag_destination_namespace(name("refs/jj/remote-tags/origin"));
    assert!(matches!(
        request.plan(&advertisement()),
        Err(FetchPlanError::TagReplacement(_))
    ));
    request.authorized_force.insert(dest);
    assert_eq!(
        request.plan(&advertisement()).unwrap()[0].kind,
        FetchUpdateKind::ForcedTag
    );
}

#[test]
fn additional_tag_namespace_prunes_missing_sources_only() {
    let (_root, mut request) = request(&[
        "+refs/heads/*:refs/jj/remote-tags/origin/*",
        "^refs/heads/private",
    ]);
    request.snapshot.insert(
        name("refs/jj/remote-tags/origin/old"),
        Target::Direct(id(3)),
    );
    request
        .snapshot
        .insert(name("refs/jj/remote-tags/other/old"), Target::Direct(id(4)));
    request = request
        .with_tag_destination_namespace(name("refs/jj/remote-tags/origin"))
        .with_prune();
    let plan = request.plan(&advertisement()).unwrap();
    assert!(plan.iter().any(|update| {
        update.mapping.destination.as_ref() == Some(&name("refs/jj/remote-tags/origin/old"))
            && update.kind == FetchUpdateKind::Prune
    }));
    assert!(!plan.iter().any(|update| {
        update.mapping.destination.as_ref() == Some(&name("refs/jj/remote-tags/other/old"))
    }));
}

#[test]
fn retained_missing_exact_source_keeps_only_its_cached_destination() {
    let (_root, mut request) = request(&[
        "+refs/heads/main:refs/remotes/origin/main",
        "+refs/tags/v1:refs/jj/remote-tags/origin/v1",
        "+refs/tags/v2:refs/jj/remote-tags/origin/v2",
    ]);
    for tag in ["v1", "v2"] {
        request.snapshot.insert(
            name(&format!("refs/jj/remote-tags/origin/{tag}")),
            Target::Direct(id(3)),
        );
    }
    let request = request
        .with_tag_destination_namespace(name("refs/jj/remote-tags/origin"))
        .with_prune()
        .with_retained_missing_sources([name("refs/tags/v1")]);
    let plan = request.plan(&advertisement()).unwrap();
    assert!(plan.iter().any(|update| {
        update.mapping.destination == Some(name("refs/remotes/origin/main"))
            && update.kind == FetchUpdateKind::Create
    }));
    assert!(!plan.iter().any(|update| {
        update.mapping.destination == Some(name("refs/jj/remote-tags/origin/v1"))
    }));
    assert!(plan.iter().any(|update| {
        update.mapping.destination == Some(name("refs/jj/remote-tags/origin/v2"))
            && update.kind == FetchUpdateKind::Prune
    }));
    let mut advertised = advertisement();
    advertised.refs.push(AdvertisedRef {
        name: name("refs/tags/v1"),
        id: id(3),
        peeled: false,
    });
    let plan = request.plan(&advertised).unwrap();
    assert!(plan.iter().any(|update| {
        update.mapping.destination == Some(name("refs/jj/remote-tags/origin/v1"))
            && update.kind == FetchUpdateKind::Unchanged
    }));
}

#[test]
fn symbolic_destination_cannot_redirect_to_branch() {
    let (_root, mut request) = request(&["refs/heads/main:refs/remotes/origin/main"]);
    request.snapshot.insert(
        name("refs/remotes/origin/main"),
        Target::Symbolic(name("refs/heads/main")),
    );
    assert!(matches!(
        request.plan(&advertisement()),
        Err(FetchPlanError::Symbolic(_))
    ));
}

#[test]
fn changed_advertisement_replans_mapping() {
    let (_root, request) = request(&["refs/heads/main:refs/remotes/origin/main"]);
    let preview = request.plan(&advertisement()).unwrap();
    let mut current = advertisement();
    current.refs[0].id = id(3);
    let actual = request.plan(&current).unwrap();
    assert_eq!(preview[0].mapping.source.as_ref().unwrap().id, id(1));
    assert_eq!(actual[0].mapping.source.as_ref().unwrap().id, id(3));
}

#[test]
fn mapping_error_selects_nothing_and_preserves_error() {
    let mut saved = None;
    let wants = select(
        Err(FetchPlanError::Destination(name("refs/heads/main"))),
        &mut saved,
    );
    let transfer: Result<(), FetchError> = Err(FetchError::Protocol("later error"));
    assert!(wants.is_empty());
    assert!(matches!(
        selected(saved, &transfer),
        Err(FetchWorkflowError::Plan(FetchPlanError::Destination(_)))
    ));
}

#[test]
fn empty_refspec_list_is_a_noop_plan() {
    let (_root, request) = request(&[]);
    assert!(request.plan(&advertisement()).unwrap().is_empty());
}

#[test]
fn mismatched_known_shallow_roots_fail_before_transfer() {
    let (_root, request) = request(&["refs/heads/main:refs/remotes/origin/main"]);
    let mut known = KnownHistory::default();
    known.shallow.push(id(3));
    assert!(matches!(
        request.check_known(&known),
        Err(FetchError::Unsupported(
            "known shallow boundaries differ from destination"
        ))
    ));
}

#[test]
fn local_progress_plan_failure_has_no_completion() {
    let (root, request) = request(&["refs/heads/main:refs/heads/main"]);
    let source = Repository::init(
        crate::ObjectFormat::Sha1,
        root.path().join("source"),
        InitKind::Bare,
    )
    .unwrap();
    let id = source.loose_objects().write_blob(b"source").unwrap();
    std::fs::write(source.git_dir().join("refs/heads/main"), format!("{id}\n")).unwrap();
    let mut events = Vec::new();
    let result = request.receive_local_with_progress(
        source.git_dir(),
        &KnownHistory::default(),
        FetchLimits::default(),
        TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
        |event| events.push(event),
    );
    assert!(matches!(
        result,
        Err(FetchWorkflowError::Plan(FetchPlanError::Destination(_)))
    ));
    assert!(!events.contains(&LocalFetchProgress::Complete));
    assert_eq!(events, [LocalFetchProgress::Reading { objects: 0 }]);
}

fn git_fixture(repository: &Repository, args: &[&str]) -> std::process::Output {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .arg("-C")
        .arg(repository.git_dir())
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            repository.git_dir().join("absent-config"),
        )
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.test")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.test")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

/// Independently generate a complete history with Git before receiving through the public API.
fn received_git_history() -> (tempfile::TempDir, FetchReady) {
    let source_root = tempfile::tempdir().unwrap();
    let source = Repository::init(
        crate::ObjectFormat::Sha1,
        source_root.path().join("source"),
        InitKind::Bare,
    )
    .unwrap();
    let tree = git_fixture(&source, &["mktree"]);
    let tree = std::str::from_utf8(&tree.stdout).unwrap().trim();
    let commit = git_fixture(&source, &["commit-tree", tree, "-m", "receive proof"]);
    let commit: ObjectId = std::str::from_utf8(&commit.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    source
        .references()
        .unwrap()
        .update_without_reflog(
            &name("refs/heads/main"),
            Target::Direct(commit),
            Expected::Absent,
        )
        .unwrap();
    let (_root, request) = request(&["refs/heads/main:refs/remotes/origin/main"]);
    let cancel = AtomicBool::new(false);
    let ready = request
        .receive_local(
            source.git_dir(),
            &KnownHistory::default(),
            FetchLimits::default(),
            TransportControl::new(&cancel),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
    (_root, ready)
}

/// Git creates the history; finish must reuse receive evidence without another inflate pass.
#[test]
fn publication_reuses_received_identity_checks() {
    let (_root, ready) = received_git_history();
    let cancel = AtomicBool::new(false);
    crate::pack::counters::reset();
    ready.finish(FetchUpdateLimits::default(), &cancel).unwrap();
    let inflations =
        crate::pack::counters::INFLATIONS.with(|counts| counts.borrow().values().sum::<usize>());
    assert_eq!(
        inflations, 0,
        "finish redundantly decoded the identity-checked received graph"
    );
}

/// Both the spool and installed copy could contain the same damage. Equality alone is not proof.
#[test]
fn damaged_receive_spool_cannot_publish_refs() {
    let (_root, ready) = received_git_history();
    let path = ready.received.pack.path().unwrap();
    let mut bytes = std::fs::read(path).unwrap();
    bytes[15] ^= 1;
    std::fs::write(path, bytes).unwrap();
    let repository = Repository::open(ready.request.repository.git_dir()).unwrap();
    let error = ready
        .finish(FetchUpdateLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        *error.source,
        FetchFinishFailure::BeforePublication(_)
    ));
    assert!(
        repository
            .references()
            .unwrap()
            .read(&name("refs/remotes/origin/main"))
            .unwrap()
            .is_none()
    );
}
