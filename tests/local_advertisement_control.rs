//! Native advertisement admits the source and inventory before returning a complete result.
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::fetch::{
    Advertisement, AdvertisementKind, FetchError, LocalAdvertisementLimits,
    LocalAdvertisementOptions,
};
use girt::{ObjectFormat, OpenLimits, PackLimits, PeelLimits};

#[path = "support/maintenance_git.rs"]
mod oracle;

fn git(root: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut isolated = vec![
        "-c",
        "maintenance.auto=false",
        "-c",
        "gc.auto=0",
        "-c",
        "tag.gpgsign=false",
    ];
    isolated.extend_from_slice(args);
    oracle::git(root, &isolated, input)
}

fn fixture(format: ObjectFormat, reftable: bool) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--initial-branch=main",
            &format!("--object-format={format}"),
            "source.git",
        ],
        b"",
    );
    let source = root.path().join("source.git");
    let tree = String::from_utf8(git(&source, &["mktree"], b"")).unwrap();
    let commit =
        String::from_utf8(git(&source, &["commit-tree", tree.trim()], b"fixture\n")).unwrap();
    git(
        &source,
        &["update-ref", "refs/heads/main", commit.trim()],
        b"",
    );
    git(
        &source,
        &["tag", "-a", "v1", "-m", "fixture", commit.trim()],
        b"",
    );
    git(
        &source,
        &["symbolic-ref", "refs/heads/alias", "refs/heads/main"],
        b"",
    );
    if reftable {
        git(&source, &["refs", "migrate", "--ref-format=reftable"], b"");
    }
    root
}

fn observed(advertisement: Advertisement) -> BTreeMap<Vec<u8>, String> {
    advertisement
        .refs
        .into_iter()
        .map(|reference| {
            let mut name = reference.name.as_bytes().to_vec();
            if reference.kind == AdvertisementKind::PeeledHint {
                name.extend_from_slice(b"^{}");
            }
            (name, reference.id.to_string())
        })
        .collect()
}

#[test]
fn defaults_match_git_tips_aliases_and_verified_peeled_hints() {
    let cancel = AtomicBool::new(false);
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        for reftable in [false, true] {
            let fixture = fixture(format, reftable);
            let source = fixture.path().join("source.git");
            let expected: BTreeMap<_, _> = git(
                fixture.path(),
                &["ls-remote", source.to_str().unwrap()],
                b"",
            )
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let (id, name) =
                    line.split_at(line.iter().position(|byte| *byte == b'\t').unwrap());
                (name[1..].to_vec(), String::from_utf8(id.to_vec()).unwrap())
            })
            .collect();
            let result = LocalAdvertisementOptions::default()
                .advertise(&source, &cancel)
                .unwrap();
            assert!(
                result
                    .capabilities
                    .iter()
                    .any(|value| value == b"symref=HEAD:refs/heads/main")
            );
            assert_eq!(observed(result), expected);
            git(&source, &["fsck", "--strict"], b"");
        }
    }
}

#[test]
fn cancellation_precedes_opening_a_missing_source() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        LocalAdvertisementOptions::default()
            .advertise(root.path().join("missing"), &AtomicBool::new(true)),
        Err(FetchError::Cancelled)
    ));
}

#[test]
fn source_open_and_pack_snapshot_limits_refuse() {
    for format in [ObjectFormat::Sha1, ObjectFormat::Sha256] {
        let fixture = fixture(format, false);
        let source = fixture.path().join("source.git");
        let cancel = AtomicBool::new(false);
        assert!(matches!(
            LocalAdvertisementOptions::default()
                .with_limits(
                    LocalAdvertisementLimits::default()
                        .with_open(OpenLimits::default().with_metadata_bytes(0))
                )
                .advertise(&source, &cancel),
            Err(FetchError::Source(_))
        ));
        git(&source, &["repack", "-ad"], b"");
        assert!(matches!(
            LocalAdvertisementOptions::default()
                .with_limits(
                    LocalAdvertisementLimits::default()
                        .with_packs(PackLimits::default().with_max_index_bytes(0))
                )
                .advertise(&source, &cancel),
            Err(FetchError::SourceObjects(_))
        ));
    }
}

#[test]
fn reference_inventory_and_head_limits_refuse() {
    let fixture = fixture(ObjectFormat::Sha1, false);
    let source = fixture.path().join("source.git");
    let cancel = AtomicBool::new(false);
    for limits in [
        LocalAdvertisementLimits::default().with_max_ref_entries(0),
        LocalAdvertisementLimits::default().with_max_ref_bytes(0),
        LocalAdvertisementLimits::default().with_max_head_bytes(0),
    ] {
        assert!(matches!(
            LocalAdvertisementOptions::default()
                .with_limits(limits)
                .advertise(&source, &cancel),
            Err(FetchError::LocalReferences(_))
        ));
    }
}

#[test]
fn output_and_verified_peel_limits_refuse_without_a_partial_advertisement() {
    let fixture = fixture(ObjectFormat::Sha256, false);
    let source = fixture.path().join("source.git");
    let cancel = AtomicBool::new(false);
    for limits in [
        LocalAdvertisementLimits::default().with_max_refs(0),
        LocalAdvertisementLimits::default().with_max_bytes(0),
    ] {
        assert!(matches!(
            LocalAdvertisementOptions::default()
                .with_limits(limits)
                .advertise(&source, &cancel),
            Err(FetchError::Limit(_))
        ));
    }
    assert!(matches!(
        LocalAdvertisementOptions::default()
            .with_limits(
                LocalAdvertisementLimits::default()
                    .with_peel(PeelLimits::default().with_max_bytes(0))
            )
            .advertise(&source, &cancel),
        Err(FetchError::Peel(_))
    ));
}

#[test]
fn symbolic_cycles_refuse_instead_of_rereading_files() {
    let fixture = fixture(ObjectFormat::Sha1, false);
    let source = fixture.path().join("source.git");
    git(
        &source,
        &["symbolic-ref", "refs/heads/a", "refs/heads/b"],
        b"",
    );
    git(
        &source,
        &["symbolic-ref", "refs/heads/b", "refs/heads/a"],
        b"",
    );
    assert!(matches!(
        LocalAdvertisementOptions::default().advertise(&source, &AtomicBool::new(false)),
        Err(FetchError::LocalReferences(_))
    ));
}

#[test]
fn linked_source_and_registered_worktree_are_typed_refusals() {
    let fixture = fixture(ObjectFormat::Sha1, false);
    let source = fixture.path().join("source.git");
    let linked = fixture.path().join("linked");
    git(
        &source,
        &[
            "worktree",
            "add",
            "-b",
            "topic",
            linked.to_str().unwrap(),
            "main",
        ],
        b"",
    );
    let cancel = AtomicBool::new(false);
    for path in [&source, &linked] {
        assert!(matches!(
            LocalAdvertisementOptions::default().advertise(path, &cancel),
            Err(FetchError::LinkedWorktrees)
        ));
    }
}

#[test]
fn source_files_are_unchanged_after_success_or_refusal() {
    fn files(path: &Path) -> BTreeMap<std::path::PathBuf, Vec<u8>> {
        let mut result = BTreeMap::new();
        for entry in fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                result.extend(files(&entry.path()));
            } else {
                result.insert(entry.path(), fs::read(entry.path()).unwrap());
            }
        }
        result
    }
    let fixture = fixture(ObjectFormat::Sha1, false);
    let source = fixture.path().join("source.git");
    let before = files(&source);
    let cancel = AtomicBool::new(false);
    LocalAdvertisementOptions::default()
        .advertise(&source, &cancel)
        .unwrap();
    assert_eq!(files(&source), before);
    assert!(
        LocalAdvertisementOptions::default()
            .with_limits(LocalAdvertisementLimits::default().with_max_refs(1))
            .advertise(&source, &cancel)
            .is_err()
    );
    assert_eq!(files(&source), before);
}
