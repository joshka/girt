//! Independently generated Git 2.56 CLI fixtures for advisory MIDX batch selection.
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use girt::multi_pack_index::{BatchError, BatchLimits, BatchSkipReason, Error};
use girt::{InitKind, ObjectFormat, ObjectId, Repository};

#[path = "support/maintenance_git.rs"]
mod oracle;
use oracle::git;

struct Fixture {
    _root: tempfile::TempDir,
    repo: Repository,
    packs: Vec<PathBuf>,
    ids: Vec<ObjectId>,
}

impl Fixture {
    fn new(format: ObjectFormat, duplicate: bool) -> Self {
        let sizes: &[usize] = if duplicate {
            &[20; 10]
        } else {
            &[20, 80, 200, 500]
        };
        Self::with_sizes(format, duplicate, sizes)
    }

    fn with_sizes(format: ObjectFormat, duplicate: bool, sizes: &[usize]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
        let mut ids = Vec::new();
        for (number, size) in sizes.iter().enumerate() {
            ids.push(
                repo.loose_objects()
                    .write_blob(&vec![b'A' + number as u8; *size])
                    .unwrap(),
            );
        }
        let groups = if duplicate {
            ids.push(repo.loose_objects().write_blob(&[b'Z'; 80]).unwrap());
            vec![ids[..10].to_vec(), ids[1..10].to_vec(), vec![ids[10]]]
        } else {
            ids.iter().map(|id| vec![*id]).collect()
        };
        let mut packs = Vec::new();
        for group in groups {
            let input: String = group.iter().map(|id| format!("{id}\n")).collect();
            let name = git(
                repo.git_dir(),
                &[
                    "pack-objects",
                    "--compression=0",
                    "--no-reuse-object",
                    "--no-reuse-delta",
                    "objects/pack/pack",
                ],
                input.as_bytes(),
            );
            let name = std::str::from_utf8(&name).unwrap().trim();
            packs.push(repo.object_dir().join(format!("pack/pack-{name}.pack")));
        }
        // Deliberately invert size and age: the largest pack is oldest. This distinguishes
        // age-first selection from a size-first policy.
        for (number, path) in packs.iter().enumerate() {
            let bytes = fs::read(path).unwrap();
            fs::rename(path, root.path().join(format!("original-{number}"))).unwrap();
            fs::write(path, bytes).unwrap();
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(
                    std::time::UNIX_EPOCH
                        + std::time::Duration::from_secs(
                            1_700_000_000 + (packs.len() - number) as u64 * 10,
                        ),
                )
                .unwrap();
        }
        let mut args = vec!["multi-pack-index".to_owned(), "write".to_owned()];
        if duplicate {
            args.push(format!(
                "--preferred-pack={}",
                packs[1].file_name().unwrap().to_str().unwrap()
            ));
        }
        git(
            repo.git_dir(),
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            b"",
        );
        Self {
            _root: root,
            repo,
            packs,
            ids,
        }
    }

    fn git_batch_objects(&self, bytes: u64) -> BTreeSet<String> {
        git(
            self.repo.git_dir(),
            &[
                "multi-pack-index",
                "repack",
                &format!("--batch-size={bytes}"),
            ],
            b"",
        );
        let mut result = BTreeSet::new();
        for entry in fs::read_dir(self.repo.object_dir().join("pack")).unwrap() {
            let path = entry.unwrap().path();
            if path
                .extension()
                .is_some_and(|extension| extension == "pack")
                && !self.packs.contains(&path)
            {
                let output = git(
                    self.repo.git_dir(),
                    &["show-index"],
                    &fs::read(path.with_extension("idx")).unwrap(),
                );
                for line in std::str::from_utf8(&output).unwrap().lines() {
                    result.insert(line.split_whitespace().nth(1).unwrap().to_owned());
                }
            }
        }
        result
    }

    fn selected_objects(&self, paths: &[PathBuf]) -> BTreeSet<String> {
        let bytes = fs::read(self.repo.object_dir().join("pack/multi-pack-index")).unwrap();
        let index =
            girt::multi_pack_index::MultiPackIndex::parse(&bytes, self.repo.object_format())
                .unwrap();
        self.ids
            .iter()
            .filter(|id| {
                let location = index.find(**id).unwrap();
                paths.contains(
                    &self
                        .repo
                        .object_dir()
                        .join("pack")
                        .join(index.pack_names()[location.pack as usize])
                        .with_extension("pack"),
                )
            })
            .map(ToString::to_string)
            .collect()
    }
}

#[rstest::rstest]
#[case(ObjectFormat::Sha1, 0)]
#[case(ObjectFormat::Sha1, 1)]
#[case(ObjectFormat::Sha1, 100)]
#[case(ObjectFormat::Sha1, 150)]
#[case(ObjectFormat::Sha1, 400)]
#[case(ObjectFormat::Sha1, 10_000)]
#[case(ObjectFormat::Sha256, 150)]
#[case(ObjectFormat::Sha256, 10_000)]
fn batch_objects_match_git(#[case] format: ObjectFormat, #[case] bytes: u64) {
    let fixture = Fixture::new(format, false);
    let reference = Fixture::new(format, false);
    let before: BTreeSet<_> = fs::read_dir(fixture.repo.object_dir().join("pack"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    let batch = fixture
        .repo
        .plan_incremental_repack(bytes, BatchLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let paths: Vec<_> = batch.packs.iter().map(|pack| pack.path.clone()).collect();
    assert_eq!(
        fixture.selected_objects(&paths),
        reference.git_batch_objects(bytes)
    );
    assert_eq!(
        batch.estimated_bytes,
        batch.packs.iter().map(|pack| pack.estimated_bytes).sum()
    );
    assert!(batch.packs.is_empty() || batch.packs.len() >= 2);
    if bytes == 0 {
        assert_eq!(batch.packs.len(), fixture.packs.len());
        assert!(batch.skipped.is_empty());
    }
    if bytes == 1 {
        assert!(
            batch
                .skipped
                .iter()
                .all(|pack| pack.reason == BatchSkipReason::TooLarge)
        );
    }
    if bytes == 100 {
        assert!(
            batch
                .skipped
                .iter()
                .any(|pack| pack.reason == BatchSkipReason::InsufficientPacks)
        );
    }

    let after: BTreeSet<_> = fs::read_dir(fixture.repo.object_dir().join("pack"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn effective_size_counts_only_midx_owned_objects() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true);
    let reference = Fixture::new(ObjectFormat::Sha1, true);
    let batch = fixture
        .repo
        .plan_incremental_repack(150, BatchLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let paths: Vec<_> = batch.packs.iter().map(|pack| pack.path.clone()).collect();
    assert_eq!(
        fixture.selected_objects(&paths),
        reference.git_batch_objects(150)
    );
    assert_eq!(
        paths,
        vec![fixture.packs[2].clone(), fixture.packs[0].clone()]
    );
    let scaled = batch
        .packs
        .iter()
        .find(|pack| pack.path == fixture.packs[0])
        .unwrap();
    assert_eq!(scaled.objects, 1);
    assert_eq!(
        scaled.estimated_bytes,
        fs::metadata(&fixture.packs[0]).unwrap().len() / 10
    );
}

#[rstest::rstest]
#[case("keep", BatchSkipReason::Kept)]
#[case("promisor", BatchSkipReason::Promisor)]
#[case("mtimes", BatchSkipReason::Cruft)]
fn protected_packs_are_reported_and_untouched(
    #[case] extension: &str,
    #[case] reason: BatchSkipReason,
) {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    fs::write(fixture.packs[0].with_extension(extension), b"").unwrap();
    let batch = fixture
        .repo
        .plan_incremental_repack(10_000, BatchLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert!(!batch.packs.iter().any(|pack| pack.path == fixture.packs[0]));
    assert!(
        batch
            .skipped
            .iter()
            .any(|pack| pack.path == fixture.packs[0] && pack.reason == reason)
    );
    assert!(fixture.packs[0].exists());
}

#[test]
fn cancellation_and_parser_limits_refuse_without_mutation() {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    assert!(matches!(
        fixture
            .repo
            .plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(true)),
        Err(BatchError::Cancelled)
    ));
    let mut limits = BatchLimits::default();
    limits.index.max_objects = 1;
    assert!(matches!(
        fixture
            .repo
            .plan_incremental_repack(0, limits, &AtomicBool::new(false)),
        Err(BatchError::Index(Error::Limit(_)))
    ));
    for path in &fixture.packs {
        assert!(path.exists());
    }
}

#[test]
fn chain_manifest_refuses_even_when_shadowed() {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    let directory = fixture.repo.object_dir().join("pack/multi-pack-index.d");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("multi-pack-index-chain"), b"marker\n").unwrap();
    assert!(matches!(
        fixture
            .repo
            .plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(false)),
        Err(BatchError::IncrementalChain)
    ));
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn missing_primary_index_is_not_rescued_by_alternates(#[case] alternate: bool) {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    let other = Fixture::new(ObjectFormat::Sha1, false);
    if alternate {
        fs::write(
            fixture.repo.object_dir().join("info/alternates"),
            format!("{}\n", other.repo.object_dir().display()),
        )
        .unwrap();
    }
    fs::remove_file(fixture.packs[0].with_extension("idx")).unwrap();
    assert!(matches!(
        fixture
            .repo
            .plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(false)),
        Err(BatchError::Index(Error::MissingPack))
    ));
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn equal_size_packs_prefer_oldest_first_like_git(#[case] same_second: bool) {
    let root = tempfile::tempdir().unwrap();
    let repo = Repository::init(ObjectFormat::Sha1, root.path().join("r"), InitKind::Bare).unwrap();
    let mut paths = Vec::new();
    for (number, age) in [30, 10, 20].into_iter().enumerate() {
        let id = repo
            .loose_objects()
            .write_blob(&[b'A' + number as u8; 20])
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
        // Create a writable fixture copy before any reader pins it; Git publishes read-only packs.
        let bytes = fs::read(&path).unwrap();
        fs::rename(&path, root.path().join(format!("original-{number}"))).unwrap();
        fs::write(&path, bytes).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000 + age),
            )
            .unwrap();
        paths.push(path);
    }
    let expected = if same_second {
        let mut sorted = paths.clone();
        sorted.sort();
        for (rank, path) in sorted.iter().enumerate() {
            fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(
                    std::time::UNIX_EPOCH
                        + std::time::Duration::new(1_700_000_000, (3 - rank as u32) * 100_000_000),
                )
                .unwrap();
        }
        sorted[..2].to_vec()
    } else {
        vec![paths[1].clone(), paths[2].clone()]
    };
    git(repo.git_dir(), &["multi-pack-index", "write"], b"");
    let batch = repo
        .plan_incremental_repack(100, BatchLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        batch
            .packs
            .iter()
            .map(|pack| &pack.path)
            .collect::<Vec<_>>(),
        expected.iter().collect::<Vec<_>>()
    );
    assert_eq!(batch.skipped.len(), 1);
    assert_eq!(batch.skipped[0].reason, BatchSkipReason::OutsideBatch);
}

#[rstest::rstest]
#[case(false)]
#[case(true)]
fn damaged_index_claims_refuse_without_selecting(#[case] pack_index: bool) {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    let path = if pack_index {
        fixture.packs[0].with_extension("idx")
    } else {
        fixture.repo.object_dir().join("pack/multi-pack-index")
    };
    let mut bytes = fs::read(&path).unwrap();
    let end = bytes.len() - 1;
    bytes[end] ^= 0xff;
    fs::rename(&path, fixture._root.path().join("original-index")).unwrap();
    fs::write(&path, bytes).unwrap();
    let error = fixture
        .repo
        .plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(false))
        .unwrap_err();
    if pack_index {
        assert!(matches!(error, BatchError::Read(_)));
    } else {
        assert!(matches!(error, BatchError::Index(Error::Checksum)));
    }
    for pack in &fixture.packs {
        assert!(pack.exists());
    }
}

#[test]
fn checksum_valid_wrong_offset_refuses_the_midx_claim() {
    use sha1::{Digest, Sha1};
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    let path = fixture.repo.object_dir().join("pack/multi-pack-index");
    let mut bytes = fs::read(&path).unwrap();
    let chunks = bytes[6] as usize;
    let row = bytes[12..12 + chunks * 12]
        .as_chunks::<12>()
        .0
        .iter()
        .find(|row| &row[..4] == b"OOFF")
        .unwrap();
    let start = u64::from_be_bytes(row[4..12].try_into().unwrap()) as usize + 4;
    let offset = u32::from_be_bytes(bytes[start..start + 4].try_into().unwrap());
    bytes[start..start + 4].copy_from_slice(&(offset + 1).to_be_bytes());
    let end = bytes.len() - 20;
    let checksum = Sha1::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(&checksum);
    fs::rename(&path, fixture._root.path().join("original-midx")).unwrap();
    fs::write(&path, bytes).unwrap();
    assert!(matches!(
        fixture
            .repo
            .plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(false)),
        Err(BatchError::Index(Error::Malformed(
            "object offset disagrees with pack index"
        )))
    ));
}

#[test]
fn zero_owner_pack_is_left_for_expiry() {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    let output = git(
        fixture.repo.git_dir(),
        &[
            "pack-objects",
            "--compression=9",
            "--no-reuse-object",
            "objects/pack/pack",
        ],
        format!("{}\n", fixture.ids[0]).as_bytes(),
    );
    let preferred = format!(
        "--preferred-pack=pack-{}.pack",
        std::str::from_utf8(&output).unwrap().trim()
    );
    git(
        fixture.repo.git_dir(),
        &["multi-pack-index", "write", &preferred],
        b"",
    );
    let batch = fixture
        .repo
        .plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert!(batch.skipped.iter().any(
        |pack| pack.path == fixture.packs[0] && pack.reason == BatchSkipReason::NoOwnedObjects
    ));
    assert!(fixture.packs[0].exists());
}

#[test]
fn oldest_eligible_packs_win_over_size_order_in_git() {
    let fixture = Fixture::with_sizes(ObjectFormat::Sha1, false, &[20, 30, 40]);
    let reference = Fixture::with_sizes(ObjectFormat::Sha1, false, &[20, 30, 40]);
    let batch = fixture
        .repo
        .plan_incremental_repack(140, BatchLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let paths: Vec<_> = batch.packs.iter().map(|pack| pack.path.clone()).collect();
    assert_eq!(
        paths,
        vec![fixture.packs[2].clone(), fixture.packs[1].clone()]
    );
    assert_eq!(
        fixture.selected_objects(&paths),
        reference.git_batch_objects(140)
    );
}
