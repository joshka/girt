//! Git-generated loose-named packs stay visible through maintenance and sidecar publication.
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use girt::retention::{MaintenanceIsolation, RetentionPolicy};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;

struct Isolation;
impl MaintenanceIsolation for Isolation {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    repo: Repository,
    pack: PathBuf,
    commit: ObjectId,
}
impl Fixture {
    fn new(format: ObjectFormat) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("r.git"), InitKind::Bare).unwrap();
        let blob = repo
            .loose_objects()
            .write_blob(b"Git loose-pack fixture\n")
            .unwrap();
        let tree = oracle::git(
            repo.git_dir(),
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        );
        let tree = std::str::from_utf8(&tree).unwrap().trim();
        let commit = oracle::git(repo.git_dir(), &["commit-tree", tree], b"fixture\n");
        let commit: ObjectId = std::str::from_utf8(&commit)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        oracle::git(
            repo.git_dir(),
            &["update-ref", "refs/heads/main", &commit.to_string()],
            b"",
        );
        oracle::git(
            repo.git_dir(),
            &["maintenance", "run", "--task=loose-objects"],
            b"",
        );
        let pack = fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
            .unwrap();
        assert!(
            pack.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("loose-")
        );
        Self {
            _root: root,
            repo,
            pack,
            commit,
        }
    }
    fn readable(&self) {
        let objects = self.repo.objects().unwrap();
        let ids = objects.object_ids(Default::default()).unwrap();
        assert_eq!(ids.len(), 3);
        for id in ids {
            assert!(objects.read(id).unwrap().is_some());
        }
        assert!(objects.read(self.commit).unwrap().is_some());
        oracle::git(self.repo.git_dir(), &["fsck", "--strict"], b"");
    }
}

#[test]
fn git_loose_pack_is_visible_to_retention_and_prune_packed() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        fixture.readable();
        let plan = fixture
            .repo
            .plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
        assert!(plan.is_complete(), "{:?}", plan.outcome);
        assert!(plan.reachable.contains(&fixture.commit));
        let report = fixture
            .repo
            .prune_packed(&mut Isolation, Default::default(), &AtomicBool::new(false))
            .unwrap();
        assert_eq!(report.deleted.len(), 3);
        assert!(fixture.pack.exists());
        fixture.readable();
    }
}

#[test]
fn git_loose_pack_midx_round_trips_through_parser_and_writer() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        oracle::git(fixture.repo.git_dir(), &["multi-pack-index", "write"], b"");
        let path = fixture.repo.object_dir().join("pack/multi-pack-index");
        let bytes = fs::read(&path).unwrap();
        let parsed = girt::multi_pack_index::MultiPackIndex::parse(&bytes, format).unwrap();
        assert_eq!(
            parsed.pack_names(),
            [fixture
                .pack
                .with_extension("idx")
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()]
        );
        fixture
            .repo
            .write_multi_pack_index(&mut Isolation, Default::default())
            .unwrap();
        oracle::git(fixture.repo.git_dir(), &["multi-pack-index", "verify"], b"");
        fixture.readable();
    }
}

#[test]
fn pack_bitmap_and_reverse_index_use_the_installed_loose_basename() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let objects = fixture.repo.objects().unwrap();
        let checksum = objects.packs().next().unwrap().checksum();
        drop(objects);
        fixture
            .repo
            .write_pack_bitmap(&mut Isolation, checksum, Default::default())
            .unwrap();
        assert!(fixture.pack.with_extension("bitmap").is_file());
        assert!(fixture.pack.with_extension("rev").is_file());
        assert!(
            !fixture
                .repo
                .object_dir()
                .join(format!("pack/pack-{checksum}.bitmap"))
                .exists()
        );
        oracle::git(
            fixture.repo.git_dir(),
            &["rev-list", "--test-bitmap", "refs/heads/main"],
            b"",
        );
        fixture.readable();
    }
}

#[cfg(unix)]
#[test]
fn expiry_and_retirement_handle_loose_named_packs_and_sidecars() {
    use std::time::{Duration, UNIX_EPOCH};
    for retirement in [false, true] {
        let fixture = Fixture::new(ObjectFormat::Sha1);
        fixture
            .repo
            .prune_packed(&mut Isolation, Default::default(), &AtomicBool::new(false))
            .unwrap();
        let policy = RetentionPolicy::default()
            .with_recent_cutoff(UNIX_EPOCH + Duration::from_secs(1_800_000_000));
        fs::File::open(&fixture.pack)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .unwrap();
        if retirement {
            let checksum = fixture
                .repo
                .objects()
                .unwrap()
                .packs()
                .next()
                .unwrap()
                .checksum();
            fixture
                .repo
                .write_pack_bitmap(&mut Isolation, checksum, Default::default())
                .unwrap();
            let report = fixture
                .repo
                .retire_old_packs(
                    &mut Isolation,
                    &policy,
                    Default::default(),
                    &AtomicBool::new(false),
                )
                .unwrap();
            assert!(report.removed.contains(&fixture.pack));
            assert!(!fixture.pack.with_extension("rev").exists());
            assert!(!fixture.pack.with_extension("bitmap").exists());
        } else {
            // Git writes a different compressed copy; preference proves the old pack owns nothing.
            let ids = fixture
                .repo
                .objects()
                .unwrap()
                .object_ids(Default::default())
                .unwrap();
            let input: String = ids.iter().map(|id| format!("{id}\n")).collect();
            let checksum = oracle::git(
                fixture.repo.git_dir(),
                &[
                    "pack-objects",
                    "--compression=0",
                    "--no-reuse-object",
                    "--no-reuse-delta",
                    "objects/pack/pack",
                ],
                input.as_bytes(),
            );
            let checksum = std::str::from_utf8(&checksum).unwrap().trim();
            oracle::git(
                fixture.repo.git_dir(),
                &[
                    "multi-pack-index",
                    "write",
                    &format!("--preferred-pack=pack-{checksum}.pack"),
                ],
                b"",
            );
            let report = fixture
                .repo
                .expire_multi_pack_index(
                    &mut Isolation,
                    Default::default(),
                    &AtomicBool::new(false),
                )
                .unwrap();
            assert!(report.removed.contains(&fixture.pack));
        }
        fixture.readable();
    }
}

#[cfg(unix)]
#[test]
fn explicit_gc_consolidation_counts_git_loose_named_packs() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = Fixture::new(format);
        let policy = RetentionPolicy::default()
            .with_pack_consolidation(girt::retention::PackConsolidation::Always);
        let report = fixture
            .repo
            .collect_garbage_with(
                &mut Isolation,
                &policy,
                Default::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(report.consolidated);
        assert!(
            report
                .retired
                .as_ref()
                .unwrap()
                .removed
                .contains(&fixture.pack)
        );
        fixture.readable();
    }
}
