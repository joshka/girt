//! Controlled transfer uses the admitted source and advertisement without rediscovery.
use super::prefetch_plan::PrefetchPlanLimits;
use super::*;
use crate::fetch::{FetchFinishOptions, LocalAdvertisementLimits, ObjectFilter};
use crate::{InitKind, ObjectFormat};

#[path = "../../tests/support/maintenance_git.rs"]
mod oracle;

fn git(repository: &Repository, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut options = vec!["-c", "maintenance.auto=false", "-c", "gc.auto=0"];
    options.extend_from_slice(args);
    oracle::git(repository.git_dir(), &options, input)
}

fn name(value: &str) -> RefName {
    RefName::new(value).unwrap()
}

fn commit(repository: &Repository, message: &[u8]) -> ObjectId {
    let tree = git(repository, &["mktree"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let id = git(repository, &["commit-tree", tree], message);
    std::str::from_utf8(&id).unwrap().trim().parse().unwrap()
}

struct Fixture {
    _root: tempfile::TempDir,
    source: Repository,
    destination: Repository,
    first: ObjectId,
    second: ObjectId,
}

impl Fixture {
    fn new(format: ObjectFormat) -> Self {
        let root = tempfile::tempdir().unwrap();
        let source =
            Repository::init(format, root.path().join("source.git"), InitKind::Bare).unwrap();
        let destination =
            Repository::init(format, root.path().join("destination.git"), InitKind::Bare).unwrap();
        let first = commit(&source, b"first independent tip\n");
        let second = commit(&source, b"second independent tip\n");
        source
            .references()
            .unwrap()
            .update_without_reflog(
                &name("refs/heads/main"),
                Target::Direct(first),
                Expected::Absent,
            )
            .unwrap();
        Self {
            _root: root,
            source,
            destination,
            first,
            second,
        }
    }

    fn request(&self) -> FetchRequest {
        FetchRequest::prepare_controlled(
            self.destination.clone(),
            Refspecs::parse(
                Direction::Fetch,
                ["refs/heads/*:refs/prefetch/remotes/origin/*"],
            )
            .unwrap(),
            BTreeSet::new(),
            Reflog::Preserve,
            FetchPrepareLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn assert_destination_empty(&self) {
        assert!(
            self.destination
                .references()
                .unwrap()
                .list()
                .unwrap()
                .is_empty()
        );
        assert!(
            self.destination
                .objects()
                .unwrap()
                .read(self.first)
                .unwrap()
                .is_none()
        );
        let packs = self.destination.common_dir().join("objects/pack");
        if packs.exists() {
            assert_eq!(std::fs::read_dir(packs).unwrap().count(), 0);
        }
    }
}

#[rstest::rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn admitted_source_survives_ref_changes_and_unreadable_configuration(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format);
    let cancel = AtomicBool::new(false);
    let admitted = super::super::local_native::admit_source(
        fixture.source.git_dir(),
        LocalAdvertisementLimits::default(),
        &cancel,
    )
    .unwrap();
    fixture
        .source
        .references()
        .unwrap()
        .update_without_reflog(
            &name("refs/heads/main"),
            Target::Direct(fixture.second),
            Expected::Value(Target::Direct(fixture.first)),
        )
        .unwrap();
    std::fs::write(
        fixture.source.git_dir().join("config"),
        b"invalid configuration\n",
    )
    .unwrap();
    let ready = fixture
        .request()
        .receive_prefetch_controlled(
            admitted,
            &KnownHistory::default(),
            PrefetchPlanLimits::default(),
            FetchLimits::default(),
            &cancel,
        )
        .unwrap();
    assert_eq!(ready.received().wants(), [fixture.first]);
    assert_eq!(ready.received().object_count(), 2);
    fixture.assert_destination_empty();
    let report = FetchFinishOptions::default()
        .finish(ready, &cancel)
        .unwrap();
    assert_eq!(report.updates[0].kind, FetchUpdateKind::Create);
    assert_eq!(
        fixture
            .destination
            .references()
            .unwrap()
            .read(&name("refs/prefetch/remotes/origin/main"))
            .unwrap(),
        Some(Target::Direct(fixture.first))
    );
    assert!(
        fixture
            .destination
            .objects()
            .unwrap()
            .read(fixture.second)
            .unwrap()
            .is_none()
    );
    git(
        &fixture.destination,
        &["fsck", "--strict", "--no-dangling"],
        b"",
    );
}

#[rstest::rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn known_tip_needs_no_new_pack(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format);
    let cancel = AtomicBool::new(false);
    let admit = || {
        super::super::local_native::admit_source(
            fixture.source.git_dir(),
            LocalAdvertisementLimits::default(),
            &cancel,
        )
        .unwrap()
    };
    let first = fixture
        .request()
        .receive_prefetch_controlled(
            admit(),
            &KnownHistory::default(),
            PrefetchPlanLimits::default(),
            FetchLimits::default(),
            &cancel,
        )
        .unwrap();
    FetchFinishOptions::default()
        .finish(first, &cancel)
        .unwrap();
    let known = KnownHistory::from_store(
        fixture
            .destination
            .objects_controlled(crate::PackLimits::default(), Default::default(), &cancel)
            .unwrap(),
        &[fixture.first],
        FetchLimits::default(),
        &cancel,
    )
    .unwrap();
    let second = fixture
        .request()
        .receive_prefetch_controlled(
            admit(),
            &known,
            PrefetchPlanLimits::default(),
            FetchLimits::default(),
            &cancel,
        )
        .unwrap();
    assert_eq!(second.updates()[0].kind, FetchUpdateKind::Unchanged);
    assert_eq!(second.received().object_count(), 0);
    assert_eq!(second.received().pack_bytes(), 0);
    let report = FetchFinishOptions::default()
        .finish(second, &cancel)
        .unwrap();
    assert!(report.installed.is_none());
    git(
        &fixture.destination,
        &["fsck", "--strict", "--no-dangling"],
        b"",
    );
}

#[test]
fn wants_limit_refuses_before_source_payloads_or_destination_effects() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let admitted = super::super::local_native::admit_source(
        fixture.source.git_dir(),
        LocalAdvertisementLimits::default(),
        &cancel,
    )
    .unwrap();
    let error = fixture
        .request()
        .receive_prefetch_controlled(
            admitted,
            &KnownHistory::default(),
            PrefetchPlanLimits::default(),
            FetchLimits::default().with_max_wants(0),
            &cancel,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        FetchWorkflowError::Transfer(FetchError::Limit("wants"))
    ));
    fixture.assert_destination_empty();
}

#[test]
fn cancellation_after_admission_precedes_transfer() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let admitted = super::super::local_native::admit_source(
        fixture.source.git_dir(),
        LocalAdvertisementLimits::default(),
        &cancel,
    )
    .unwrap();
    cancel.store(true, Ordering::Relaxed);
    let error = fixture
        .request()
        .receive_prefetch_controlled(
            admitted,
            &KnownHistory::default(),
            PrefetchPlanLimits::default(),
            FetchLimits::default(),
            &cancel,
        )
        .unwrap_err();
    assert!(matches!(
        error,
        FetchWorkflowError::Transfer(FetchError::Cancelled)
    ));
    fixture.assert_destination_empty();
}

#[rstest::rstest]
#[case::depth(true)]
#[case::filter(false)]
fn unsupported_request_policy_has_no_effects(#[case] depth: bool) {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let admitted = super::super::local_native::admit_source(
        fixture.source.git_dir(),
        LocalAdvertisementLimits::default(),
        &cancel,
    )
    .unwrap();
    let request = if depth {
        fixture.request().with_depth(NonZeroU32::MIN)
    } else {
        fixture.request().with_filter(ObjectFilter::BlobNone)
    };
    assert!(matches!(
        request.receive_prefetch_controlled(
            admitted,
            &KnownHistory::default(),
            PrefetchPlanLimits::default(),
            FetchLimits::default(),
            &cancel,
        ),
        Err(FetchWorkflowError::Transfer(FetchError::Unsupported(_)))
    ));
    fixture.assert_destination_empty();
}
