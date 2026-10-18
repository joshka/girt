//! Original Git CLI-generated loose/packed duplicates; no upstream implementation fixtures.
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use girt::retention::{MaintenanceIsolation, PrunePackedLimits};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};

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
    packed: ObjectId,
    loose: ObjectId,
    pack: PathBuf,
}

impl Fixture {
    fn new(format: ObjectFormat) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
        let packed = repo.loose_objects().write_blob(b"packed survivor").unwrap();
        let loose = repo.loose_objects().write_blob(b"loose only").unwrap();
        let checksum = git(
            repo.git_dir(),
            &["pack-objects", "objects/pack/pack"],
            format!("{packed}\n").as_bytes(),
        );
        let checksum = std::str::from_utf8(&checksum).unwrap().trim();
        let pack = repo.object_dir().join(format!("pack/pack-{checksum}.pack"));
        Self {
            _root: root,
            repo,
            packed,
            loose,
            pack,
        }
    }

    fn loose_path(&self, id: ObjectId) -> PathBuf {
        let hex = id.to_string();
        self.repo.object_dir().join(&hex[..2]).join(&hex[2..])
    }
}

#[rstest::rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn verified_duplicates_match_git_and_preserve_readability(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format);
    let reference = Fixture::new(format);
    let original_pack = fs::read(&fixture.pack).unwrap();
    let original_index = fs::read(fixture.pack.with_extension("idx")).unwrap();
    let report = fixture
        .repo
        .prune_packed(
            &mut PrivateFixture,
            PrunePackedLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(report.deleted, [fixture.packed]);
    assert!(!fixture.loose_path(fixture.packed).exists());
    assert!(fixture.loose_path(fixture.loose).exists());
    assert_eq!(fs::read(&fixture.pack).unwrap(), original_pack);
    assert_eq!(
        fs::read(fixture.pack.with_extension("idx")).unwrap(),
        original_index
    );
    let objects = fixture.repo.objects().unwrap();
    for id in [fixture.packed, fixture.loose] {
        assert!(objects.read_with(id, Default::default()).unwrap().is_some());
    }
    git(fixture.repo.git_dir(), &["fsck", "--full"], b"");
    git(reference.repo.git_dir(), &["prune-packed"], b"");
    assert_eq!(
        git(fixture.repo.git_dir(), &["count-objects", "-v"], b""),
        git(reference.repo.git_dir(), &["count-objects", "-v"], b"")
    );
}

#[test]
fn damaged_packed_survivor_never_deletes_the_readable_loose_copy() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let mut bytes = fs::read(&fixture.pack).unwrap();
    bytes[14] ^= 0xff;
    fs::rename(&fixture.pack, fixture._root.path().join("original.pack")).unwrap();
    fs::write(&fixture.pack, bytes).unwrap();
    let failure = fixture
        .repo
        .prune_packed(
            &mut PrivateFixture,
            PrunePackedLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(failure.report.deleted.is_empty());
    assert!(
        fixture
            .repo
            .loose_objects()
            .read_blob(fixture.packed, 1024)
            .is_ok()
    );
}

#[test]
fn unindexed_pack_is_not_deletion_evidence() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    fs::remove_file(fixture.pack.with_extension("idx")).unwrap();
    let report = fixture
        .repo
        .prune_packed(
            &mut PrivateFixture,
            PrunePackedLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.deleted.is_empty());
    assert!(fixture.loose_path(fixture.packed).exists());
}

#[test]
fn cancellation_and_budget_refuse_before_any_deletion() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let failure = fixture
        .repo
        .prune_packed(
            &mut PrivateFixture,
            PrunePackedLimits::default(),
            &AtomicBool::new(true),
        )
        .unwrap_err();
    assert!(failure.report.deleted.is_empty());
    let mut limits = PrunePackedLimits::default();
    limits.max_entries = 0;
    let failure = fixture
        .repo
        .prune_packed(&mut PrivateFixture, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(failure.report.deleted.is_empty());
    assert!(fixture.loose_path(fixture.packed).exists());
    assert!(fixture.loose_path(fixture.loose).exists());
}

#[test]
fn byte_budget_and_kept_pack_protection() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let keep = fixture.pack.with_extension("keep");
    fs::write(&keep, b"keep this pack").unwrap();
    let mut limits = PrunePackedLimits::default();
    limits.max_bytes = 0;
    let failure = fixture
        .repo
        .prune_packed(&mut PrivateFixture, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert!(failure.report.deleted.is_empty());
    assert!(fixture.loose_path(fixture.packed).exists());
    let report = fixture
        .repo
        .prune_packed(
            &mut PrivateFixture,
            PrunePackedLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(report.deleted, [fixture.packed]);
    assert_eq!(fs::read(&keep).unwrap(), b"keep this pack");
    assert!(fixture.pack.exists());
}

#[test]
fn alternate_packed_copy_never_authorizes_primary_loose_deletion() {
    let alternate = Fixture::new(ObjectFormat::Sha1);
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r"), InitKind::Bare).unwrap();
    let id = repo.loose_objects().write_blob(b"packed survivor").unwrap();
    fs::write(
        repo.object_dir().join("info/alternates"),
        format!("{}\n", alternate.repo.object_dir().display()),
    )
    .unwrap();
    let report = repo
        .prune_packed(
            &mut PrivateFixture,
            PrunePackedLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.deleted.is_empty());
    assert_eq!(report.retained, [id]);
    assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
}

#[cfg(unix)]
#[test]
fn symlink_fanout_is_never_followed_for_deletion() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let root = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(root.path(), fixture.repo.object_dir().join("00")).unwrap();
    let failure = fixture
        .repo
        .prune_packed(
            &mut PrivateFixture,
            PrunePackedLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(failure.report.deleted.is_empty());
    assert!(fixture.loose_path(fixture.packed).exists());
}
