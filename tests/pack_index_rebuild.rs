//! Rebuilding damaged or missing `.idx` files from Git-generated packs, checked against the index
//! `git index-pack` wrote for the same pack.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, thread};

use girt::pack::{IndexRebuildError, IndexRebuildLimits, IndexRebuildOutcome, rebuild_index};
use girt::{ObjectFormat, PackLimits, ReadLimits};
use pack_git::{Fixture, git, git_output};
use rstest::rstest;

const NAMES: usize = 1032;

fn rebuild(
    fixture: &Fixture,
    format: ObjectFormat,
) -> Result<IndexRebuildOutcome, IndexRebuildError> {
    rebuild_index(
        &fixture.index_path.with_extension("pack"),
        format,
        IndexRebuildLimits::default(),
        &AtomicBool::new(false),
    )
    .map(|rebuilt| rebuilt.outcome)
}

/// Reads a published index the way girt's pack reader opens one: Windows may briefly refuse to
/// open a name while a rename replaces it, so retry those errors within the same bound.
fn read_published(path: &Path) -> Vec<u8> {
    #[cfg(windows)]
    {
        let mut delay = std::time::Duration::from_millis(1);
        for _ in 0..7 {
            match fs::read(path) {
                Err(error)
                    if error
                        .raw_os_error()
                        .is_some_and(|code| [5, 32, 303].contains(&code)) =>
                {
                    thread::sleep(delay);
                    delay *= 2;
                }
                result => return result.unwrap(),
            }
        }
    }
    fs::read(path).unwrap()
}

fn count(index: &[u8]) -> usize {
    u32::from_be_bytes(index[1028..1032].try_into().unwrap()) as usize
}

fn offsets(index: &[u8], width: usize) -> usize {
    NAMES + count(index) * (width + 4)
}

fn flip_checksum(mut index: Vec<u8>, _: usize) -> Vec<u8> {
    *index.last_mut().unwrap() ^= 1;
    index
}

fn bump_fanout(mut index: Vec<u8>, _: usize) -> Vec<u8> {
    index[8 + 4 * 0x80 + 3] ^= 1;
    index
}

fn flip_name(mut index: Vec<u8>, _: usize) -> Vec<u8> {
    index[NAMES + 5] ^= 1;
    index
}

fn offset_outside_pack(mut index: Vec<u8>, width: usize) -> Vec<u8> {
    let at = offsets(&index, width);
    index[at..at + 4].copy_from_slice(&0x7fff_fff0u32.to_be_bytes());
    index
}

fn duplicate_offset(mut index: Vec<u8>, width: usize) -> Vec<u8> {
    let at = offsets(&index, width);
    index.copy_within(at..at + 4, at + 4);
    index
}

fn swap_offsets(mut index: Vec<u8>, width: usize) -> Vec<u8> {
    let at = offsets(&index, width);
    let first: [u8; 4] = index[at..at + 4].try_into().unwrap();
    index.copy_within(at + 4..at + 8, at);
    index[at + 4..at + 8].copy_from_slice(&first);
    index
}

fn flip_trailer_pack_hash(mut index: Vec<u8>, width: usize) -> Vec<u8> {
    let at = index.len() - 2 * width;
    index[at] ^= 1;
    index
}

fn truncate(mut index: Vec<u8>, _: usize) -> Vec<u8> {
    index.truncate(index.len() / 2);
    index
}

/// Git writes pack artifacts read-only, so replace rather than open them for writing.
fn overwrite(path: &Path, bytes: &[u8]) {
    fs::remove_file(path).unwrap();
    fs::write(path, bytes).unwrap();
}

/// Checks the rebuilt index with Git, then reads every fixture object through it with girt.
fn verify_pack(fixture: &Fixture) {
    let objects = fixture.repo.objects_with(PackLimits::default()).unwrap();
    let read: Vec<_> = fixture
        .records
        .iter()
        .map(|(id, _, _)| {
            let object = objects
                .read_with(*id, ReadLimits::default())
                .unwrap()
                .unwrap();
            (object.id(), object.kind(), object.into_data())
        })
        .collect();
    assert_eq!(read, fixture.records);
    assert!(
        fixture
            .records
            .iter()
            .any(|(id, _, _)| *id == fixture.delta)
    );
    assert!(
        fixture
            .records
            .iter()
            .any(|(id, _, _)| *id == fixture.ordinary)
    );
    git(
        fixture.root.path(),
        &["verify-pack", fixture.index_path.to_str().unwrap()],
        b"",
    );
}

/// Every index-only damage is repaired to the exact bytes `git index-pack` wrote.
#[rstest]
#[case::checksum(flip_checksum)]
#[case::fanout(bump_fanout)]
#[case::name(flip_name)]
#[case::offset_outside_pack(offset_outside_pack)]
#[case::duplicated_offset(duplicate_offset)]
#[case::swapped_offsets(swap_offsets)]
#[case::trailer_pack_hash(flip_trailer_pack_hash)]
#[case::truncated(truncate)]
fn damaged_index_is_rebuilt_to_git_bytes(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[values(true, false)] ofs: bool,
    #[case] damage: fn(Vec<u8>, usize) -> Vec<u8>,
) {
    let fixture = Fixture::new(format, ofs, 16);
    let original = fs::read(&fixture.index_path).unwrap();
    let damaged = damage(original.clone(), format.digest_len());
    overwrite(&fixture.index_path, &damaged);

    assert_eq!(
        rebuild(&fixture, format).unwrap(),
        IndexRebuildOutcome::Replaced
    );
    assert_eq!(fs::read(&fixture.index_path).unwrap(), original);
    verify_pack(&fixture);
    #[cfg(unix)]
    {
        let mode = |path: &Path| {
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(path).unwrap().permissions())
        };
        assert_eq!(
            mode(&fixture.index_path),
            mode(&fixture.index_path.with_extension("pack"))
        );
    }
}

/// A deleted index is recreated.
#[rstest]
fn missing_index_is_created(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format, true, 16);
    let original = fs::read(&fixture.index_path).unwrap();
    fs::remove_file(&fixture.index_path).unwrap();

    assert_eq!(
        rebuild(&fixture, format).unwrap(),
        IndexRebuildOutcome::Created
    );
    assert_eq!(fs::read(&fixture.index_path).unwrap(), original);
    verify_pack(&fixture);
}

/// A correct index is left alone rather than rewritten.
#[rstest]
fn intact_index_is_not_rewritten(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let fixture = Fixture::new(format, true, 16);
    let before = fs::metadata(&fixture.index_path).unwrap();

    assert_eq!(
        rebuild(&fixture, format).unwrap(),
        IndexRebuildOutcome::Unchanged
    );
    let after = fs::metadata(&fixture.index_path).unwrap();
    assert_eq!(after.modified().unwrap(), before.modified().unwrap());
    assert_eq!(
        listing(fixture.index_path.parent().unwrap()).len(),
        listing_len(&fixture)
    );
}

fn listing(directory: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn listing_len(fixture: &Fixture) -> usize {
    listing(fixture.index_path.parent().unwrap())
        .iter()
        .filter(|name| !name.starts_with("tmp_"))
        .count()
}

fn flip_body(mut pack: Vec<u8>, _: usize) -> Vec<u8> {
    let middle = pack.len() / 2;
    pack[middle] ^= 1;
    pack
}

fn flip_pack_trailer(mut pack: Vec<u8>, _: usize) -> Vec<u8> {
    *pack.last_mut().unwrap() ^= 1;
    pack
}

fn truncate_pack(mut pack: Vec<u8>, width: usize) -> Vec<u8> {
    pack.truncate(pack.len() - width - 1);
    pack
}

/// A damaged pack is refused and nothing in the pack directory changes.
#[rstest]
#[case::body(flip_body)]
#[case::trailer(flip_pack_trailer)]
#[case::truncated(truncate_pack)]
fn damaged_pack_is_refused_without_changes(
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
    #[case] damage: fn(Vec<u8>, usize) -> Vec<u8>,
) {
    let fixture = Fixture::new(format, true, 16);
    let pack_path = fixture.index_path.with_extension("pack");
    let pack = damage(fs::read(&pack_path).unwrap(), format.digest_len());
    overwrite(&pack_path, &pack);
    let index = flip_checksum(fs::read(&fixture.index_path).unwrap(), 0);
    overwrite(&fixture.index_path, &index);
    let directory = listing(pack_path.parent().unwrap());

    assert!(matches!(
        rebuild(&fixture, format),
        Err(IndexRebuildError::Corrupt(_))
    ));
    assert_eq!(fs::read(&pack_path).unwrap(), pack);
    assert_eq!(fs::read(&fixture.index_path).unwrap(), index);
    assert_eq!(listing(pack_path.parent().unwrap()), directory);
    assert!(
        !git_output(fixture.root.path(), &["index-pack", "--stdin"], &pack,)
            .status
            .success()
    );
}

/// The store's format must match the pack; a SHA-1 pack is not indexed as SHA-256.
#[test]
fn wrong_format_is_refused() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 4);
    let original = fs::read(&fixture.index_path).unwrap();

    assert!(matches!(
        rebuild(&fixture, ObjectFormat::Sha256),
        Err(IndexRebuildError::Corrupt(_))
    ));
    assert_eq!(fs::read(&fixture.index_path).unwrap(), original);
}

#[test]
fn rejects_non_pack_path() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 4);

    assert!(matches!(
        rebuild_index(
            &fixture.index_path,
            ObjectFormat::Sha1,
            IndexRebuildLimits::default(),
            &AtomicBool::new(false),
        ),
        Err(IndexRebuildError::NotPack(_))
    ));
}

#[test]
fn cancellation_leaves_index_unchanged() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 4);
    let damaged = flip_checksum(fs::read(&fixture.index_path).unwrap(), 0);
    overwrite(&fixture.index_path, &damaged);

    assert!(matches!(
        rebuild_index(
            &fixture.index_path.with_extension("pack"),
            ObjectFormat::Sha1,
            IndexRebuildLimits::default(),
            &AtomicBool::new(true),
        ),
        Err(IndexRebuildError::Cancelled)
    ));
    assert_eq!(fs::read(&fixture.index_path).unwrap(), damaged);
}

/// Readers racing repeated replacements see either the damaged or the rebuilt index, whole.
#[test]
fn concurrent_readers_never_see_a_partial_index() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 64);
    let original = fs::read(&fixture.index_path).unwrap();
    let damaged = flip_name(original.clone(), 0);
    let done = AtomicBool::new(false);
    let staged = fixture.root.path().join("damaged-idx");

    // Stop the reader however the writer exits, so a failure panics rather than hangs.
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let mut reads = 0;
            while !done.load(Ordering::Relaxed) {
                let seen = read_published(&fixture.index_path);
                assert!(
                    seen == original || seen == damaged,
                    "partial index observed"
                );
                reads += 1;
            }
            reads
        });
        let stop = Stop(&done);
        for _ in 0..50 {
            fs::write(&staged, &damaged).unwrap();
            // Windows refuses to rename over the read-only index the rebuild publishes.
            #[cfg(windows)]
            {
                let mut permissions = fs::metadata(&fixture.index_path).unwrap().permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                fs::set_permissions(&fixture.index_path, permissions).unwrap();
            }
            fs::rename(&staged, &fixture.index_path).unwrap();
            assert_eq!(
                rebuild(&fixture, ObjectFormat::Sha1).unwrap(),
                IndexRebuildOutcome::Replaced
            );
        }
        drop(stop);
        assert!(reader.join().unwrap() > 0);
    });
    assert_eq!(fs::read(&fixture.index_path).unwrap(), original);
}
