//! Independently generated Git 2.56 CLI fixtures for advisory MIDX batch selection.
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use girt::multi_pack_index::{BatchLimits, BatchSkipReason};
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
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("r"), InitKind::Bare).unwrap();
        let sizes: &[usize] = if duplicate { &[20; 10] } else { &[20, 80, 200, 500] };
        let mut ids = Vec::new();
        for (number, size) in sizes.iter().enumerate() {
            ids.push(repo.loose_objects().write_blob(&vec![b'A' + number as u8; *size]).unwrap());
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
            let name = git(repo.git_dir(), &[
                "pack-objects", "--compression=0", "--no-reuse-object", "--no-reuse-delta",
                "objects/pack/pack",
            ], input.as_bytes());
            let name = std::str::from_utf8(&name).unwrap().trim();
            packs.push(repo.object_dir().join(format!("pack/pack-{name}.pack")));
        }
        let mut args = vec!["multi-pack-index".to_owned(), "write".to_owned()];
        if duplicate {
            args.push(format!("--preferred-pack={}", packs[1].file_name().unwrap().to_str().unwrap()));
        }
        git(repo.git_dir(), &args.iter().map(String::as_str).collect::<Vec<_>>(), b"");
        Self { _root: root, repo, packs, ids }
    }

    fn git_batch_objects(&self, bytes: u64) -> BTreeSet<String> {
        git(self.repo.git_dir(), &["multi-pack-index", "repack", &format!("--batch-size={bytes}")], b"");
        let mut result = BTreeSet::new();
        for entry in fs::read_dir(self.repo.object_dir().join("pack")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|extension| extension == "pack") && !self.packs.contains(&path) {
                let output = git(self.repo.git_dir(), &["show-index"], &fs::read(path.with_extension("idx")).unwrap());
                for line in std::str::from_utf8(&output).unwrap().lines() {
                    result.insert(line.split_whitespace().nth(1).unwrap().to_owned());
                }
            }
        }
        result
    }

    fn selected_objects(&self, paths: &[PathBuf]) -> BTreeSet<String> {
        let bytes = fs::read(self.repo.object_dir().join("pack/multi-pack-index")).unwrap();
        let index = girt::multi_pack_index::MultiPackIndex::parse(&bytes, self.repo.object_format()).unwrap();
        self.ids.iter().filter(|id| {
            let location = index.find(**id).unwrap();
            paths.contains(&self.repo.object_dir().join("pack").join(index.pack_names()[location.pack as usize]).with_extension("pack"))
        }).map(ToString::to_string).collect()
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
    let batch = fixture.repo.plan_incremental_repack(bytes, BatchLimits::default(), &AtomicBool::new(false)).unwrap();
    let paths: Vec<_> = batch.packs.iter().map(|pack| pack.path.clone()).collect();
    assert_eq!(fixture.selected_objects(&paths), reference.git_batch_objects(bytes));
    assert_eq!(batch.estimated_bytes, batch.packs.iter().map(|pack| pack.estimated_bytes).sum());
    assert!(batch.packs.is_empty() || batch.packs.len() >= 2);
    assert_eq!(fs::read_dir(fixture.repo.object_dir().join("pack")).unwrap().count(), fixture.packs.len() * 2 + 1);
}

#[test]
fn effective_size_counts_only_midx_owned_objects() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true);
    let reference = Fixture::new(ObjectFormat::Sha1, true);
    let batch = fixture.repo.plan_incremental_repack(150, BatchLimits::default(), &AtomicBool::new(false)).unwrap();
    let paths: Vec<_> = batch.packs.iter().map(|pack| pack.path.clone()).collect();
    assert_eq!(fixture.selected_objects(&paths), reference.git_batch_objects(150));
    assert_eq!(paths, vec![fixture.packs[0].clone(), fixture.packs[2].clone()]);
    assert_eq!(batch.packs[0].objects, 1);
    assert_eq!(batch.packs[0].estimated_bytes, fs::metadata(&fixture.packs[0]).unwrap().len() / 10);
}

#[rstest::rstest]
#[case("keep", BatchSkipReason::Kept)]
#[case("promisor", BatchSkipReason::Promisor)]
#[case("mtimes", BatchSkipReason::Cruft)]
fn protected_packs_are_reported_and_untouched(#[case] extension: &str, #[case] reason: BatchSkipReason) {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    fs::write(fixture.packs[0].with_extension(extension), b"").unwrap();
    let batch = fixture.repo.plan_incremental_repack(10_000, BatchLimits::default(), &AtomicBool::new(false)).unwrap();
    assert!(!batch.packs.iter().any(|pack| pack.path == fixture.packs[0]));
    assert!(batch.skipped.iter().any(|pack| pack.path == fixture.packs[0] && pack.reason == reason));
    assert!(fixture.packs[0].exists());
}

#[test]
fn cancellation_and_parser_limits_refuse_without_mutation() {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    assert!(fixture.repo.plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(true)).is_err());
    let mut limits = BatchLimits::default();
    limits.index.max_objects = 1;
    assert!(fixture.repo.plan_incremental_repack(0, limits, &AtomicBool::new(false)).is_err());
    for path in &fixture.packs { assert!(path.exists()); }
}

#[test]
fn chain_manifest_refuses_even_when_shadowed() {
    let fixture = Fixture::new(ObjectFormat::Sha1, false);
    let directory = fixture.repo.object_dir().join("pack/multi-pack-index.d");
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("multi-pack-index-chain"), b"marker\n").unwrap();
    assert!(fixture.repo.plan_incremental_repack(0, BatchLimits::default(), &AtomicBool::new(false)).is_err());
}
