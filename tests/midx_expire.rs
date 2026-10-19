//! Original duplicate-pack fixtures for isolated standalone MIDX expiry.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, UNIX_EPOCH};

use girt::multi_pack_index::ExpireLimits;
use girt::retention::MaintenanceIsolation;
use girt::{InitKind, ObjectFormat, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;
use oracle::git;

struct PrivateFixture;
impl MaintenanceIsolation for PrivateFixture {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    repo: Repository,
    old: PathBuf,
    survivor: PathBuf,
}

impl Fixture {
    fn new(format: ObjectFormat) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
        let id = repo
            .loose_objects()
            .write_blob(&b"packed survivor\n".repeat(100))
            .unwrap();
        let mut packs = Vec::new();
        for compression in ["0", "9"] {
            let name = git(
                repo.git_dir(),
                &[
                    "pack-objects",
                    &format!("--compression={compression}"),
                    "--no-reuse-object",
                    "--no-reuse-delta",
                    "objects/pack/pack",
                ],
                format!("{id}\n").as_bytes(),
            );
            let name = std::str::from_utf8(&name).unwrap().trim();
            packs.push(repo.object_dir().join(format!("pack/pack-{name}.pack")));
        }
        assert_ne!(packs[0], packs[1]);
        let old = packs.remove(0);
        let survivor = packs.remove(0);
        for path in [&old, &old.with_extension("idx")] {
            fs::File::open(path)
                .unwrap()
                .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                .unwrap();
        }
        git(
            repo.git_dir(),
            &[
                "multi-pack-index",
                "write",
                &format!(
                    "--preferred-pack={}",
                    survivor.file_name().unwrap().to_str().unwrap()
                ),
            ],
            b"",
        );
        Self {
            _root: root,
            repo,
            old,
            survivor,
        }
    }
}

#[rstest::rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn expire_zero_owner_pack_matches_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format);
    let reference = Fixture::new(format);
    let survivor = fs::read(&fixture.survivor).unwrap();
    let report = fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.index_published);
    assert!(report.removed.contains(&fixture.old));
    assert!(!fixture.old.exists());
    assert_eq!(fs::read(&fixture.survivor).unwrap(), survivor);
    git(fixture.repo.git_dir(), &["multi-pack-index", "verify"], b"");
    git(fixture.repo.git_dir(), &["fsck", "--full"], b"");
    git(
        reference.repo.git_dir(),
        &["multi-pack-index", "expire"],
        b"",
    );
    assert_eq!(
        git(fixture.repo.git_dir(), &["count-objects", "-v"], b""),
        git(reference.repo.git_dir(), &["count-objects", "-v"], b"")
    );
}

#[rstest::rstest]
#[case("keep")]
#[case("promisor")]
#[case("mtimes")]
fn protected_pack_survives_without_index_replacement(#[case] sidecar: &str) {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    fs::write(fixture.old.with_extension(sidecar), b"").unwrap();
    let path = fixture.repo.object_dir().join("pack/multi-pack-index");
    let before = fs::read(&path).unwrap();
    let report = fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(!report.index_published);
    assert!(report.removed.is_empty());
    assert!(fixture.old.exists());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn corrupt_survivor_preserves_old_pack_and_original_index() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let midx = fixture.repo.object_dir().join("pack/multi-pack-index");
    let before = fs::read(&midx).unwrap();
    let mut permissions = fs::metadata(&fixture.survivor).unwrap().permissions();
    permissions.set_mode(permissions.mode() | 0o200);
    fs::set_permissions(&fixture.survivor, permissions).unwrap();
    let mut bytes = fs::read(&fixture.survivor).unwrap();
    bytes[14] ^= 0xff;
    fs::write(&fixture.survivor, bytes).unwrap();
    let failure = fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(!failure.report.index_published);
    assert!(failure.report.removed.is_empty());
    assert!(fixture.old.exists());
    assert_eq!(fs::read(midx).unwrap(), before);
}

#[test]
fn pre_cancellation_and_zero_budget_publish_nothing() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let failure = fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(true),
        )
        .unwrap_err();
    assert!(!failure.report.index_published);
    let mut limits = ExpireLimits::default();
    limits.max_entries = 0;
    let failure = fixture
        .repo
        .expire_multi_pack_index(&mut PrivateFixture, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(failure.report.removed.is_empty());
    assert!(fixture.old.exists());
}

#[test]
fn pack_absent_from_starting_index_is_never_expired() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let id = fixture
        .repo
        .loose_objects()
        .write_blob(b"new unlisted pack")
        .unwrap();
    let name = git(
        fixture.repo.git_dir(),
        &["pack-objects", "objects/pack/pack"],
        format!("{id}\n").as_bytes(),
    );
    let name = std::str::from_utf8(&name).unwrap().trim();
    let unlisted = fixture
        .repo
        .object_dir()
        .join(format!("pack/pack-{name}.pack"));
    let report = fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(!report.removed.contains(&unlisted));
    assert!(unlisted.exists());
    git(fixture.repo.git_dir(), &["fsck", "--full"], b"");
}

#[test]
fn aggregate_survivor_budget_refuses_before_publication() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut limits = ExpireLimits::default();
    limits.max_bytes = 0;
    let failure = fixture
        .repo
        .expire_multi_pack_index(&mut PrivateFixture, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(!failure.report.index_published);
    assert!(failure.report.removed.is_empty());
    assert!(fixture.old.exists());
}

#[test]
fn replacement_preserves_source_index_permissions() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let path = fixture.repo.object_dir().join("pack/multi-pack-index");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn shadowed_incremental_chain_refuses_before_publication() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let directory = fixture.repo.object_dir().join("pack/multi-pack-index.d");
    fs::create_dir(&directory).unwrap();
    fs::write(
        directory.join("multi-pack-index-chain"),
        b"fixture chain marker\n",
    )
    .unwrap();
    let failure = fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        failure.cause,
        girt::multi_pack_index::ExpireCause::IncrementalChain
    ));
    assert!(!failure.report.index_published);
    assert!(fixture.old.exists());
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn missing_primary_index_refuses_even_with_complete_alternate(#[case] use_alternate: bool) {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let alternate = Fixture::new(ObjectFormat::Sha1);
    if use_alternate {
        fs::write(
            fixture.repo.object_dir().join("info/alternates"),
            format!("{}\n", alternate.repo.object_dir().display()),
        )
        .unwrap();
    }
    fs::remove_file(fixture.old.with_extension("idx")).unwrap();
    let before = fs::read(fixture.repo.object_dir().join("pack/multi-pack-index")).unwrap();
    let failure = fixture
        .repo
        .expire_multi_pack_index(
            &mut PrivateFixture,
            ExpireLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(!failure.report.index_published);
    assert!(failure.report.removed.is_empty());
    assert!(fixture.old.exists());
    assert_eq!(
        before,
        fs::read(fixture.repo.object_dir().join("pack/multi-pack-index")).unwrap()
    );
}

#[test]
fn refresh_limits_refuse_before_index_publication_or_pack_removal() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for open in [
            girt::OpenLimits::default().with_metadata_bytes(0),
            girt::OpenLimits::default()
                .with_configuration(girt::config::ResolveLimits::default().with_bytes(0)),
        ] {
            let fixture = Fixture::new(format);
            let index = fixture.repo.object_dir().join("pack/multi-pack-index");
            let before = fs::read(&index).unwrap();
            let failure = fixture
                .repo
                .expire_multi_pack_index(
                    &mut PrivateFixture,
                    ExpireLimits::default().with_open(open),
                    &AtomicBool::new(false),
                )
                .unwrap_err();
            assert!(matches!(
                failure.cause,
                girt::multi_pack_index::ExpireCause::Open(_)
            ));
            assert!(!failure.report.index_published);
            assert!(failure.report.removed.is_empty());
            assert_eq!(fs::read(index).unwrap(), before);
            assert!(fixture.old.exists());
            assert!(fixture.survivor.exists());
        }
    }
}

#[test]
fn cancellation_at_isolation_is_observed_before_refresh() {
    struct CancelOnAcquire<'a>(&'a AtomicBool);
    impl MaintenanceIsolation for CancelOnAcquire<'_> {
        type Guard = ();
        fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
    }
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let cancel = AtomicBool::new(false);
    let failure = fixture
        .repo
        .expire_multi_pack_index(
            &mut CancelOnAcquire(&cancel),
            ExpireLimits::default(),
            &cancel,
        )
        .unwrap_err();
    let girt::multi_pack_index::ExpireCause::Open(source) = failure.cause else {
        panic!("expected controlled opener cancellation");
    };
    assert!(matches!(*source, girt::repository::OpenError::Cancelled));
    assert!(!failure.report.index_published);
    assert!(failure.report.removed.is_empty());
    assert!(fixture.old.exists());
    assert!(fixture.survivor.exists());
}
