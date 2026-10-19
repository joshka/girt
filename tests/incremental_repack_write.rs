//! Original Git-generated acceptance fixtures for additive incremental batch publication.
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use girt::multi_pack_index::{BatchPublication, BatchRepackLimits};
use girt::retention::MaintenanceIsolation;
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
    packs: Vec<PathBuf>,
    ids: Vec<ObjectId>,
}

impl Fixture {
    fn new(format: ObjectFormat) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
        let mut packs = Vec::new();
        let mut ids = Vec::new();
        for (number, size) in [20, 30, 40].into_iter().enumerate() {
            let id = repo
                .loose_objects()
                .write_blob(&vec![b'A' + number as u8; size])
                .unwrap();
            let name = git(
                repo.git_dir(),
                &["pack-objects", "--compression=0", "objects/pack/pack"],
                format!("{id}\n").as_bytes(),
            );
            let path = repo.object_dir().join(format!(
                "pack/pack-{}.pack",
                std::str::from_utf8(&name).unwrap().trim()
            ));
            packs.push(path);
            ids.push(id);
        }
        git(repo.git_dir(), &["multi-pack-index", "write"], b"");
        Self {
            _root: root,
            repo,
            packs,
            ids,
        }
    }

    fn assert_old_storage(&self, before: &[Vec<u8>]) {
        for (path, bytes) in self.packs.iter().zip(before) {
            assert_eq!(fs::read(path).unwrap(), *bytes);
        }
        let objects = self.repo.objects().unwrap();
        for id in &self.ids {
            assert!(objects.read(*id).unwrap().is_some());
        }
    }
}

#[rstest::rstest]
#[case(ObjectFormat::Sha1)]
#[case(ObjectFormat::Sha256)]
fn publishes_one_batch_and_midx_without_retiring_any_pack(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format);
    let before: Vec<_> = fixture
        .packs
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
    let report = fixture
        .repo
        .repack_incremental_batch(
            &mut PrivateFixture,
            0,
            BatchRepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(report.publication, BatchPublication::Pair);
    assert!(report.index_published);
    assert_eq!(report.written.unwrap().objects, 3);
    fixture.assert_old_storage(&before);
    git(fixture.repo.git_dir(), &["multi-pack-index", "verify"], b"");
    git(fixture.repo.git_dir(), &["fsck", "--full"], b"");
    let again = fixture
        .repo
        .repack_incremental_batch(
            &mut PrivateFixture,
            0,
            BatchRepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(again.publication, BatchPublication::None);
    assert!(!again.index_published);
}

#[test]
fn write_limit_and_precancellation_preserve_prior_storage() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let before: Vec<_> = fixture
        .packs
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
    let mut limits = BatchRepackLimits::default();
    limits.write.max_input_bytes = 1;
    let failure = fixture
        .repo
        .repack_incremental_batch(&mut PrivateFixture, 0, limits, &AtomicBool::new(false))
        .unwrap_err();
    assert_eq!(failure.report.publication, BatchPublication::None);
    assert!(!failure.report.index_published);
    let failure = fixture
        .repo
        .repack_incremental_batch(
            &mut PrivateFixture,
            0,
            BatchRepackLimits::default(),
            &AtomicBool::new(true),
        )
        .unwrap_err();
    assert_eq!(failure.report.publication, BatchPublication::None);
    fixture.assert_old_storage(&before);
}

#[test]
fn isolation_refusal_cannot_publish() {
    struct Refuse;
    impl MaintenanceIsolation for Refuse {
        type Guard = ();
        fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
            Err(std::io::Error::other("fixture isolation refused"))
        }
    }
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let before = fs::read(fixture.repo.object_dir().join("pack/multi-pack-index")).unwrap();
    let failure = fixture
        .repo
        .repack_incremental_batch(
            &mut Refuse,
            0,
            BatchRepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(failure.report.publication, BatchPublication::None);
    assert_eq!(
        before,
        fs::read(fixture.repo.object_dir().join("pack/multi-pack-index")).unwrap()
    );
}

#[test]
fn duplicate_promisor_object_cannot_enter_an_ordinary_batch_pack() {
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let name = git(
        fixture.repo.git_dir(),
        &[
            "pack-objects",
            "--compression=9",
            "--no-reuse-object",
            "objects/pack/pack",
        ],
        format!("{}\n", fixture.ids[0]).as_bytes(),
    );
    let path = fixture.repo.object_dir().join(format!(
        "pack/pack-{}.pack",
        std::str::from_utf8(&name).unwrap().trim()
    ));
    fs::write(path.with_extension("promisor"), b"promisor fixture\n").unwrap();
    // The ordinary pack owns this identity in the MIDX; classification must still notice
    // the duplicate promisor copy, rather than relying only on the selected owner.
    let preferred = format!(
        "--preferred-pack={}",
        fixture.packs[0].file_name().unwrap().to_str().unwrap()
    );
    git(
        fixture.repo.git_dir(),
        &["multi-pack-index", "write", &preferred],
        b"",
    );
    let report = fixture
        .repo
        .repack_incremental_batch(
            &mut PrivateFixture,
            0,
            BatchRepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(report.protected_objects, 1);
    let written = report.written.unwrap();
    assert_eq!(written.objects, 2);
    let index = fixture
        .repo
        .object_dir()
        .join(format!("pack/pack-{}.idx", written.checksum));
    let names = git(
        fixture.repo.git_dir(),
        &["show-index"],
        &fs::read(index).unwrap(),
    );
    assert!(
        !std::str::from_utf8(&names)
            .unwrap()
            .contains(&fixture.ids[0].to_string())
    );
    assert_eq!(
        fs::read(path.with_extension("promisor")).unwrap(),
        b"promisor fixture\n"
    );
    git(fixture.repo.git_dir(), &["fsck", "--full"], b"");
}

#[test]
fn admission_limits_leave_midx_and_pack_inventory_unchanged() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for open in [
            girt::OpenLimits::default().with_metadata_bytes(0),
            girt::OpenLimits::default()
                .with_configuration(girt::config::ResolveLimits::default().with_bytes(0)),
        ] {
            let fixture = Fixture::new(format);
            let directory = fixture.repo.object_dir().join("pack");
            let mut before: Vec<_> = fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            before.sort();
            let index = fs::read(directory.join("multi-pack-index")).unwrap();
            let failure = fixture
                .repo
                .repack_incremental_batch(
                    &mut PrivateFixture,
                    0,
                    BatchRepackLimits::default().with_open(open),
                    &AtomicBool::new(false),
                )
                .unwrap_err();
            assert!(matches!(
                failure.cause,
                girt::multi_pack_index::BatchRepackCause::Open(_)
            ));
            assert_eq!(failure.report.publication, BatchPublication::None);
            assert!(!failure.report.index_published);
            let mut after: Vec<_> = fs::read_dir(&directory)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            after.sort();
            assert_eq!(before, after);
            assert_eq!(fs::read(directory.join("multi-pack-index")).unwrap(), index);
        }
    }
}

#[test]
fn cancellation_acquired_with_isolation_reaches_batch_opener() {
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
        .repack_incremental_batch(
            &mut CancelOnAcquire(&cancel),
            0,
            BatchRepackLimits::default(),
            &cancel,
        )
        .unwrap_err();
    let girt::multi_pack_index::BatchRepackCause::Open(source) = failure.cause else {
        panic!("expected controlled opener cancellation");
    };
    assert!(matches!(*source, girt::repository::OpenError::Cancelled));
    assert_eq!(failure.report.publication, BatchPublication::None);
    assert!(!failure.report.index_published);
}

#[test]
fn subset_batch_copies_only_owners_in_selected_pack_ordinals() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let target = fs::metadata(&fixture.packs[1]).unwrap().len() + 1;
        let before: Vec<_> = fixture
            .packs
            .iter()
            .map(|path| fs::read(path).unwrap())
            .collect();
        let report = fixture
            .repo
            .repack_incremental_batch(
                &mut PrivateFixture,
                target,
                BatchRepackLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert_eq!(report.batch.packs.len(), 2);
        assert!(
            !report
                .batch
                .packs
                .iter()
                .any(|pack| pack.path == fixture.packs[2])
        );
        let written = report.written.unwrap();
        assert_eq!(written.objects, 2);
        let index = fixture
            .repo
            .object_dir()
            .join(format!("pack/pack-{}.idx", written.checksum));
        let names = git(
            fixture.repo.git_dir(),
            &["show-index"],
            &fs::read(index).unwrap(),
        );
        let names = std::str::from_utf8(&names).unwrap();
        assert!(names.contains(&fixture.ids[0].to_string()));
        assert!(names.contains(&fixture.ids[1].to_string()));
        assert!(!names.contains(&fixture.ids[2].to_string()));
        fixture.assert_old_storage(&before);
        git(fixture.repo.git_dir(), &["multi-pack-index", "verify"], b"");
        git(
            fixture.repo.git_dir(),
            &["fsck", "--strict", "--no-dangling"],
            b"",
        );
    }
}

#[test]
fn checksum_valid_corrupt_claim_in_unselected_pack_still_refuses_before_publication() {
    use sha1::{Digest, Sha1};
    let fixture = Fixture::new(ObjectFormat::Sha1);
    let path = fixture.repo.object_dir().join("pack/multi-pack-index");
    let mut bytes = fs::read(&path).unwrap();
    let parsed = girt::multi_pack_index::MultiPackIndex::parse(&bytes, ObjectFormat::Sha1).unwrap();
    let ordinal = parsed
        .pack_names()
        .iter()
        .position(|name| {
            *name
                == fixture.packs[2]
                    .with_extension("idx")
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
        })
        .unwrap();
    let chunks = bytes[6] as usize;
    let row = bytes[12..12 + chunks * 12]
        .as_chunks::<12>()
        .0
        .iter()
        .find(|row| &row[..4] == b"OOFF")
        .unwrap();
    let start = u64::from_be_bytes(row[4..12].try_into().unwrap()) as usize;
    let entry = bytes[start..]
        .as_chunks::<8>()
        .0
        .iter()
        .take(3)
        .position(|entry| u32::from_be_bytes(entry[..4].try_into().unwrap()) as usize == ordinal)
        .unwrap();
    let offset = start + entry * 8 + 4;
    let old = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap());
    bytes[offset..offset + 4].copy_from_slice(&(old + 1).to_be_bytes());
    let end = bytes.len() - 20;
    let checksum = Sha1::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(&checksum);
    fs::rename(&path, fixture._root.path().join("original-midx")).unwrap();
    fs::write(&path, &bytes).unwrap();
    let before: Vec<_> = fixture
        .packs
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect();
    let target = fs::metadata(&fixture.packs[1]).unwrap().len() + 1;
    let failure = fixture
        .repo
        .repack_incremental_batch(
            &mut PrivateFixture,
            target,
            BatchRepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert_eq!(failure.report.publication, BatchPublication::None);
    assert!(matches!(
        failure.cause,
        girt::multi_pack_index::BatchRepackCause::Plan(_)
    ));
    assert_eq!(fs::read(path).unwrap(), bytes);
    for (path, expected) in fixture.packs.iter().zip(before) {
        assert_eq!(fs::read(path).unwrap(), expected);
    }
}
