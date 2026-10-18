//! Selection walks must not inherit their first-copy hash omission after packed damage.
use std::fs;

use super::*;
use crate::pack::counters;
use crate::pack_git::Fixture;

#[test]
fn selection_fallback_hashes_each_copy_and_clones_skip_damage() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 4);
    let original = fs::read(&fixture.index_path).unwrap();
    let width = 20;
    let count = fixture.records.len();
    let names = &original[1032..1032 + count * width];
    let position = names
        .chunks_exact(width)
        .position(|name| name == fixture.ordinary.as_bytes())
        .unwrap();
    let other = (position + 1) % count;
    let crcs = 1032 + count * width;
    let offsets = crcs + count * 4;
    let first = fixture.repo.object_dir().join("pack/a.idx");
    let second = fixture.repo.object_dir().join("pack/z.idx");
    let pack = fs::read(fixture.index_path.with_extension("pack")).unwrap();
    fs::write(first.with_extension("pack"), &pack).unwrap();
    fs::write(second.with_extension("pack"), &pack).unwrap();
    let mut crc_damage = original.clone();
    crc_damage[crcs + position * 4] ^= 1;
    rehash(&mut crc_damage);
    fs::write(&first, crc_damage).unwrap();
    let mut identity_damage = original;
    for byte in 0..4 {
        identity_damage.swap(crcs + position * 4 + byte, crcs + other * 4 + byte);
        identity_damage.swap(offsets + position * 4 + byte, offsets + other * 4 + byte);
    }
    rehash(&mut identity_damage);
    fs::write(&second, identity_damage).unwrap();
    fs::remove_file(&fixture.index_path).unwrap();
    fs::remove_file(fixture.index_path.with_extension("pack")).unwrap();
    let objects = fixture.repo.objects_with(PackLimits::default()).unwrap();
    let clone = objects.clone();
    let cancel = AtomicBool::new(false);
    assert!(
        objects
            .read_unverified(fixture.ordinary, ReadLimits::default(), &cancel)
            .is_err()
    );
    counters::reset();
    assert!(
        clone
            .read_with(fixture.ordinary, ReadLimits::default())
            .is_err()
    );
    assert_eq!(counters::OS_READS.with(|reads| reads.get()), 0);
    assert_eq!(
        objects
            .read_with(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
    assert!(fixture.root.path().exists());
}

fn rehash(bytes: &mut [u8]) {
    let end = bytes.len() - 20;
    let checksum = ObjectFormat::Sha1.checksum(&bytes[..end]);
    bytes[end..].copy_from_slice(checksum.as_bytes());
}

#[test]
fn independently_refreshed_clones_share_new_pack_damage() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 4);
    let mut first = fixture.repo.objects_with(PackLimits::default()).unwrap();
    let mut second = first.clone();
    counters::reset();
    assert_eq!(
        first
            .read_with(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.ordinary
    );
    let healthy_reads = counters::OS_READS.with(|reads| reads.get());
    let new = fixture.repo.object_dir().join("pack/a-new.idx");
    fs::copy(
        fixture.index_path.with_extension("pack"),
        new.with_extension("pack"),
    )
    .unwrap();
    let mut bytes = fs::read(&fixture.index_path).unwrap();
    let position = bytes[1032..1032 + fixture.records.len() * 20]
        .as_chunks::<20>()
        .0
        .iter()
        .position(|name| name == fixture.ordinary.as_bytes())
        .unwrap();
    bytes[1032 + fixture.records.len() * 20 + position * 4] ^= 1;
    rehash(&mut bytes);
    fs::write(new, bytes).unwrap();
    first
        .refresh(PackLimits::default(), AlternateLimits::default())
        .unwrap();
    second
        .refresh(PackLimits::default(), AlternateLimits::default())
        .unwrap();
    assert_eq!(
        first
            .read_with(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.ordinary
    );
    counters::reset();
    assert_eq!(
        second
            .read_with(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.ordinary
    );
    assert_eq!(counters::OS_READS.with(|reads| reads.get()), healthy_reads);
}

#[test]
fn listed_lookups_open_only_loose_files_present_when_their_directory_was_listed() {
    let root = tempfile::tempdir().unwrap();
    let loose = LooseObjects::new(root.path(), ObjectFormat::Sha1);
    let before = loose.write_blob(b"before listing").unwrap();
    let objects = Objects::open(
        ObjectFormat::Sha1,
        root.path(),
        crate::PackLimits::default(),
        AlternateLimits::default(),
    )
    .unwrap();
    let listing = LooseListing::default();
    let cancel = AtomicBool::new(false);
    let limits = ReadLimits::default();
    let located = |id| {
        objects
            .locate_listed(id, limits, &cancel, &listing)
            .unwrap()
    };
    assert_eq!(located(before), Some((ObjectKind::Blob, 14)));
    // A file written into an already listed directory stays unseen by this listing only.
    let mut after = None;
    for n in 0.. {
        let id = loose
            .write_blob(format!("after listing {n}").as_bytes())
            .unwrap();
        if id.as_bytes().first() == before.as_bytes().first() {
            after = Some(id);
            break;
        }
    }
    let after = after.unwrap();
    assert_eq!(located(after), None);
    assert!(objects.locate(after, limits, &cancel).unwrap().is_some());
    assert!(
        objects
            .transfer_object_listed(after, limits, &cancel, &LooseListing::default())
            .unwrap()
            .is_some()
    );
    // The empty tree is synthesized without a listed file.
    let empty = ObjectFormat::Sha1.empty_tree();
    assert_eq!(located(empty), Some((ObjectKind::Tree, 0)));
}

#[rstest::rstest]
#[case::enabled("true", true)]
#[case::disabled("false", false)]
#[case::invalid("invalid", false)]
fn midx_configuration_controls_loading_and_refresh(#[case] value: &str, #[case] expected: bool) {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 4);
    crate::pack_git::git(fixture.root.path(), &["multi-pack-index", "write"], b"");
    crate::pack_git::git(
        fixture.root.path(),
        &["config", "core.multiPackIndex", value],
        b"",
    );
    let repo = crate::Repository::open(fixture.root.path()).unwrap();
    let mut objects = repo.objects().unwrap();
    assert_eq!(objects.stores[0].midx.is_some(), expected);

    objects
        .refresh(PackLimits::default(), AlternateLimits::default())
        .unwrap();
    assert_eq!(objects.stores[0].midx.is_some(), expected);
    assert!(objects.read(fixture.ordinary).unwrap().is_some());
}

/// No variant outgrows the pack-pair paths, so errors embedding `ObjectReadError` stay under
/// Clippy's `result_large_err` bound on every platform, including Windows' larger paths.
#[test]
fn read_error_stays_small() {
    type PackArtifacts = (std::path::PathBuf, std::path::PathBuf, Box<ObjectReadError>);
    assert!(
        std::mem::size_of::<ObjectReadError>()
            <= std::mem::size_of::<PackArtifacts>() + std::mem::align_of::<PackArtifacts>()
    );
}
