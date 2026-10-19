//! Controlled prune-packed refreshes are bounded before any loose copy is removed.
use std::sync::atomic::{AtomicBool, Ordering};

use girt::retention::{MaintenanceIsolation, PrunePackedCause, PrunePackedLimits};
use girt::{InitKind, ObjectFormat, ObjectId, OpenError, OpenLimits, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

struct Exclusive<'a>(Option<&'a AtomicBool>);
impl MaintenanceIsolation for Exclusive<'_> {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        // Only this fixture uses its disposable repository.
        if let Some(cancel) = self.0 {
            cancel.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

fn fixture(format: ObjectFormat) -> (tempfile::TempDir, Repository, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
    let id = repo
        .loose_objects()
        .write_blob(b"survives refused refresh")
        .unwrap();
    oracle::git(
        repo.git_dir(),
        &["pack-objects", "objects/pack/pack"],
        format!("{id}\n").as_bytes(),
    );
    (root, repo, id)
}

#[test]
fn metadata_budget_refuses_refresh_before_deleting_any_loose_copy() {
    let (_root, repo, id) = fixture(ObjectFormat::Sha1);
    let limits =
        PrunePackedLimits::default().with_open(OpenLimits::default().with_metadata_bytes(0));
    let failure = repo
        .prune_packed(&mut Exclusive(None), limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(
        matches!(failure.cause, PrunePackedCause::Open(error) if matches!(error.as_ref(), OpenError::Limit(_)))
    );
    assert!(failure.report.deleted.is_empty());
    assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
    oracle::git(repo.git_dir(), &["fsck", "--strict"], b"");
}

#[test]
fn configuration_budget_is_applied_to_the_fresh_repository() {
    let (_root, repo, id) = fixture(ObjectFormat::Sha1);
    let open = OpenLimits::default()
        .with_configuration(girt::config::ResolveLimits::default().with_bytes(0));
    let failure = repo
        .prune_packed(
            &mut Exclusive(None),
            PrunePackedLimits::default().with_open(open),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    let PrunePackedCause::Open(error) = &failure.cause else {
        panic!("expected refresh failure: {failure:?}");
    };
    let OpenError::Resolve(error) = error.as_ref() else {
        panic!("expected configuration resolution failure: {failure:?}");
    };
    assert!(matches!(
        &error.source,
        girt::config::ResolveFailure::Limit(_)
    ));
    assert!(failure.report.deleted.is_empty());
    assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
}

#[test]
fn cancellation_during_isolation_is_seen_by_the_opener() {
    let (_root, repo, id) = fixture(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let failure = repo
        .prune_packed(
            &mut Exclusive(Some(&cancel)),
            PrunePackedLimits::default(),
            &cancel,
        )
        .unwrap_err();
    assert!(
        matches!(failure.cause, PrunePackedCause::Open(error) if matches!(error.as_ref(), OpenError::Cancelled))
    );
    assert!(failure.report.deleted.is_empty());
    assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
}

#[test]
fn default_refresh_limits_still_allow_verified_duplicate_pruning() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let (_root, repo, id) = fixture(format);
        let report = repo
            .prune_packed(
                &mut Exclusive(None),
                PrunePackedLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(report.deleted, [id]);
        assert!(repo.objects().unwrap().read(id).unwrap().is_some());
        oracle::git(repo.git_dir(), &["fsck", "--strict"], b"");
    }
}
