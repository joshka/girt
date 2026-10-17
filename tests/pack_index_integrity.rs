//! A damaged `.idx` must fail a read or an explicit verification, never yield wrong content.
//!
//! Opening a store checks what Git checks when it opens an index: the header, the table sizes the
//! entry count implies, a monotonic fanout, the pack's object count and the pack checksum the index
//! records. Damage Git only meets during a lookup, such as an out-of-order name, a bad offset or a
//! large-offset slot outside its table, fails the read that meets it, as in Git, and leaves other
//! objects readable. The index's own trailing checksum and its whole structure belong to
//! [`girt::Objects::verify_packs`]. Each case damages a Git-generated index, often with a
//! recomputed index checksum so that the structural checks rather than the checksum are what catch
//! the damage, and compares every recorded object's read with `git cat-file`.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::fs;
use std::sync::atomic::AtomicBool;

use girt::{ObjectFormat, ObjectReadError, Objects, PackLimits, ReadLimits};
use pack_git::{Fixture, git_output};
use rstest::rstest;

struct Layout {
    count: usize,
    width: usize,
}

impl Layout {
    fn of(format: ObjectFormat, index: &[u8]) -> Self {
        let count = u32::from_be_bytes(index[1028..1032].try_into().unwrap()) as usize;
        Self {
            count,
            width: format.digest_len(),
        }
    }

    fn name(&self, position: usize) -> usize {
        1032 + position * self.width
    }

    fn offset(&self, position: usize) -> usize {
        1032 + self.count * (self.width + 4) + position * 4
    }
}

fn rehash_index(format: ObjectFormat, bytes: &mut [u8]) {
    use sha2::Digest;
    let end = bytes.len() - format.digest_len();
    let checksum = match format {
        ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes[..end]).to_vec(),
        ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes[..end]).to_vec(),
        _ => panic!("unsupported hash format in test fixture"),
    };
    bytes[end..].copy_from_slice(&checksum);
}

fn damage(fixture: &Fixture, rehash: bool, edit: impl FnOnce(&Layout, &mut Vec<u8>)) {
    let format = fixture.repo.object_format();
    let mut index = fs::read(&fixture.index_path).unwrap();
    edit(&Layout::of(format, &index), &mut index);
    if rehash {
        rehash_index(format, &mut index);
    }
    fs::remove_file(&fixture.index_path).unwrap();
    fs::write(&fixture.index_path, index).unwrap();
}

fn open(fixture: &Fixture) -> Result<Objects, ObjectReadError> {
    fixture.repo.objects_with(PackLimits::default())
}

fn corrupt_artifacts(result: Result<Objects, ObjectReadError>) -> &'static str {
    match result {
        Err(ObjectReadError::PackArtifacts { source, .. }) => match *source {
            ObjectReadError::Corrupt(reason) => reason,
            other => panic!("unexpected open failure: {other}"),
        },
        Err(other) => panic!("unexpected open failure: {other}"),
        Ok(_) => panic!("damaged index opened"),
    }
}

/// Which recorded objects girt reads, asserting that each read returns exactly what Git wrote.
fn girt_reads(fixture: &Fixture, objects: &Objects) -> Vec<bool> {
    fixture
        .records
        .iter()
        .map(
            |(id, kind, data)| match objects.read_with(*id, ReadLimits::default()) {
                Ok(Some(object)) => {
                    assert_eq!(
                        (object.id(), object.kind(), object.data()),
                        (*id, *kind, &data[..])
                    );
                    true
                }
                Ok(None) | Err(_) => false,
            },
        )
        .collect()
}

/// Which recorded objects `git cat-file` reads from the same damaged repository.
fn git_reads(fixture: &Fixture) -> Vec<bool> {
    fixture
        .records
        .iter()
        .map(|(id, kind, _)| {
            git_output(
                fixture.root.path(),
                &["cat-file", kind.as_str(), &id.to_string()],
                b"",
            )
            .status
            .success()
        })
        .collect()
}

/// Opens the damaged store and checks that girt reads exactly the objects Git reads. Returns the
/// store and the number of objects that fail.
fn reads_like_git(fixture: &Fixture) -> (Objects, usize) {
    let objects = open(fixture).unwrap();
    let girt = girt_reads(fixture, &objects);
    assert_eq!(girt, git_reads(fixture));
    let failed = girt.iter().filter(|read| !**read).count();
    (objects, failed)
}

/// Like [`reads_like_git`] for damage that some read meets and explicit verification finds.
fn fails_reads_like_git(fixture: &Fixture) -> usize {
    let (objects, failed) = reads_like_git(fixture);
    assert!(failed > 0, "the damage was not met");
    assert!(objects.verify_packs(&AtomicBool::new(false)).is_err());
    failed
}

/// The read error for `id` with all of its causes.
fn read_error(objects: &Objects, id: girt::ObjectId) -> String {
    let error = objects.read_with(id, ReadLimits::default()).unwrap_err();
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message = format!("{message}: {cause}");
        source = cause.source();
    }
    message
}

fn git_rejects(fixture: &Fixture) -> bool {
    let index = fixture.index_path.to_str().unwrap();
    !git_output(fixture.root.path(), &["verify-pack", index], b"")
        .status
        .success()
}

/// Every object Git reads fails in Git too: no lookup succeeds.
fn git_reads_nothing(fixture: &Fixture) -> bool {
    !git_reads(fixture).into_iter().any(|read| read)
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn damaged_index_checksum_reads_like_git_and_fails_verification(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, false, |_, index| *index.last_mut().unwrap() ^= 1);
    assert!(git_rejects(&fixture));
    let objects = open(&fixture).unwrap();
    assert_eq!(girt_reads(&fixture, &objects), git_reads(&fixture));
    assert!(git_reads(&fixture).into_iter().all(|read| read));
    // Both a whole entry and a delta read through the damaged index.
    for id in [fixture.ordinary, fixture.delta] {
        assert!(
            objects
                .read_with(id, ReadLimits::default())
                .unwrap()
                .is_some()
        );
    }
    assert!(matches!(
        objects.verify_packs(&AtomicBool::new(false)),
        Err(ObjectReadError::Corrupt("index checksum"))
    ));
}

/// A fanout that decreases fails every lookup in Git, and fails the open in girt.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn non_monotonic_fanout_fails_open_like_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, true, |_, index| {
        index[8 + 0xfe * 4..8 + 0xff * 4].fill(0xff);
    });
    assert!(git_reads_nothing(&fixture));
    assert_eq!(corrupt_artifacts(open(&fixture)), "index fanout");
}

/// Moving one entry into the previous bucket keeps the fanout monotonic and its total, so neither
/// Git nor girt notices at open; lookups in the two damaged buckets search the wrong range.
#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn shifted_fanout_bucket_reads_like_git(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    let first = fixture.records[0].0.as_bytes()[0] as usize;
    damage(&fixture, rehash, |_, index| {
        let bucket = 8 + first.max(1) * 4 - 4;
        let value = u32::from_be_bytes(index[bucket..bucket + 4].try_into().unwrap()) + 1;
        index[bucket..bucket + 4].copy_from_slice(&value.to_be_bytes());
    });
    fails_reads_like_git(&fixture);
}

/// An entry count that disagrees with the pack's fails every lookup in Git, and the open in girt.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn fanout_total_fails_open_like_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, true, |layout, index| {
        index[8 + 255 * 4..8 + 256 * 4].copy_from_slice(&(layout.count as u32 + 1).to_be_bytes());
    });
    assert!(git_reads_nothing(&fixture));
    assert_eq!(corrupt_artifacts(open(&fixture)), "index table length");
}

#[rstest]
#[case::past_pack_sha1(ObjectFormat::Sha1, 0x7fff_0000)]
#[case::past_pack_sha256(ObjectFormat::Sha256, 0x7fff_0000)]
#[case::in_header_sha1(ObjectFormat::Sha1, 5)]
#[case::in_header_sha256(ObjectFormat::Sha256, 5)]
fn offset_outside_pack_fails_its_read_like_git(#[case] format: ObjectFormat, #[case] offset: u32) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, true, |layout, index| {
        let at = layout.offset(0);
        index[at..at + 4].copy_from_slice(&offset.to_be_bytes());
    });
    let objects = open(&fixture).unwrap();
    let id = fixture.records[position_of(&fixture, 0)].0;
    assert!(read_error(&objects, id).contains("offset outside pack entries"));
    fails_reads_like_git(&fixture);
}

/// The record at index position `position` in identity order.
fn position_of(fixture: &Fixture, position: usize) -> usize {
    let mut ids: Vec<_> = fixture.records.iter().map(|record| record.0).collect();
    ids.sort_unstable();
    fixture
        .records
        .iter()
        .position(|record| record.0 == ids[position])
        .unwrap()
}

/// One entry pointing at another's bytes reads that entry, which neither its CRC nor its identity
/// matches. Git can serve the other object's content for it, so only girt's result is checked.
#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn duplicated_offset_fails_its_read(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, rehash, |layout, index| {
        let (from, to) = (layout.offset(0), layout.offset(1));
        let copy: [u8; 4] = index[from..from + 4].try_into().unwrap();
        index[to..to + 4].copy_from_slice(&copy);
    });
    let objects = open(&fixture).unwrap();
    let reads = girt_reads(&fixture, &objects);
    assert!(!reads[position_of(&fixture, 1)]);
    assert!(objects.verify_packs(&AtomicBool::new(false)).is_err());
}

/// Swapped offsets still tile the pack, so only the entry CRCs and identities tell them apart.
#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn swapped_offsets_fail_reads_not_content(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, rehash, |layout, index| {
        let (a, b) = (layout.offset(0), layout.offset(1));
        let first: [u8; 4] = index[a..a + 4].try_into().unwrap();
        index.copy_within(b..b + 4, a);
        index[b..b + 4].copy_from_slice(&first);
    });
    let objects = open(&fixture).unwrap();
    let reads = girt_reads(&fixture, &objects);
    assert!(reads.iter().filter(|read| !**read).count() >= 2);
    assert!(objects.verify_packs(&AtomicBool::new(false)).is_err());
}

/// A damaged name usually stays in order. The original object then reads as absent and the
/// damaged name, which no object hashes to, fails its identity check.
#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn damaged_name_never_reads_wrong_content(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    let width = format.digest_len();
    let mut damaged = None;
    damage(&fixture, rehash, |layout, index| {
        let at = layout.name(0) + width - 1;
        index[at] ^= 1;
        damaged = Some(index[layout.name(0)..layout.name(0) + width].to_vec());
    });
    let damaged = girt::ObjectId::from_bytes(format, &damaged.unwrap()).unwrap();
    let (objects, failed) = reads_like_git(&fixture);
    assert_eq!(failed, 1);
    assert!(objects.read_with(damaged, ReadLimits::default()).is_err());
    if !rehash {
        assert!(matches!(
            objects.verify_packs(&AtomicBool::new(false)),
            Err(ObjectReadError::Corrupt("index checksum"))
        ));
    }
}

/// A name moved out of order hides itself, and can hide neighbours, from the binary search: Git
/// reports those objects missing rather than the index corrupt, and so does girt.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn unsorted_name_reads_like_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let width = format.digest_len();
    // The middle name becomes just above its successor, so it sorts after it.
    damage(&fixture, true, |layout, index| {
        let (at, next) = (
            layout.name(layout.count / 2),
            layout.name(layout.count / 2 + 1),
        );
        index.copy_within(next..next + width, at);
        index[at + width - 1] = index[at + width - 1].wrapping_add(1);
    });
    fails_reads_like_git(&fixture);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn damaged_trailer_pack_hash_fails_open_like_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, false, |_, index| {
        let at = index.len() - 2 * format.digest_len();
        index[at] ^= 1;
    });
    assert!(git_reads_nothing(&fixture));
    assert_eq!(
        corrupt_artifacts(open(&fixture)),
        "index/pack checksum disagreement"
    );
}

/// Four stray bytes before the trailer make the large-offset table end in a partial slot. Git
/// still reads every object, and so does girt; explicit verification rejects the table.
#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn partial_large_offset_slot_reads_like_git(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, rehash, |_, index| {
        let at = index.len() - 2 * format.digest_len();
        index.splice(at..at, [0; 4]);
    });
    let (objects, failed) = reads_like_git(&fixture);
    assert_eq!(failed, 0);
    assert!(git_rejects(&fixture));
    assert!(objects.verify_packs(&AtomicBool::new(false)).is_err());
}

/// A large-offset table with a slot for every entry is longer than any index needs: Git refuses
/// the size on every lookup, and girt at open.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn oversized_large_offset_table_fails_open_like_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, true, |layout, index| {
        let at = index.len() - 2 * format.digest_len();
        index.splice(at..at, vec![0; 8 * layout.count]);
    });
    assert!(git_reads_nothing(&fixture));
    assert_eq!(corrupt_artifacts(open(&fixture)), "index table length");
}

/// Truncation inside each index table, with a recomputed checksum, is rejected by Git's
/// `verify-pack` and by opening, so the structural checks rather than the checksum reject it.
#[rstest]
#[case::sha1_names(ObjectFormat::Sha1, "names")]
#[case::sha1_crcs(ObjectFormat::Sha1, "crcs")]
#[case::sha1_offsets(ObjectFormat::Sha1, "offsets")]
#[case::sha1_trailer(ObjectFormat::Sha1, "trailer")]
#[case::sha256_names(ObjectFormat::Sha256, "names")]
#[case::sha256_crcs(ObjectFormat::Sha256, "crcs")]
#[case::sha256_offsets(ObjectFormat::Sha256, "offsets")]
#[case::sha256_trailer(ObjectFormat::Sha256, "trailer")]
fn truncated_table_fails_like_git(#[case] format: ObjectFormat, #[case] table: &str) {
    let fixture = Fixture::new(format, true, 8);
    let width = format.digest_len();
    damage(&fixture, true, |layout, index| {
        let crcs = layout.name(layout.count);
        let length = match table {
            "names" => layout.name(layout.count / 2) + 3,
            "crcs" => crcs + 4 * (layout.count / 2) + 1,
            "offsets" => layout.offset(layout.count / 2) + 2,
            _ => index.len() - width - 1,
        };
        // Keep room for the recomputed checksum after the cut.
        index.truncate(length + width);
    });
    assert!(
        git_rejects(&fixture),
        "Git accepted a truncated {table} table"
    );
    assert!(git_reads_nothing(&fixture));
    assert!(open(&fixture).is_err());
}

/// A large-offset slot beyond the one-slot table is rejected by Git's `verify-pack` and by a
/// lookup of that entry, in Git and girt; other entries still read.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn large_offset_slot_outside_table_fails_its_read_like_git(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, true, |layout, index| {
        let at = layout.offset(0);
        let offset = u32::from_be_bytes(index[at..at + 4].try_into().unwrap());
        index[at..at + 4].copy_from_slice(&0x8000_0001_u32.to_be_bytes());
        let trailer = index.len() - 2 * format.digest_len();
        index.splice(trailer..trailer, u64::from(offset).to_be_bytes());
    });
    assert!(
        git_rejects(&fixture),
        "Git accepted an out-of-table large offset"
    );
    let objects = open(&fixture).unwrap();
    let id = fixture.records[position_of(&fixture, 0)].0;
    assert!(read_error(&objects, id).contains("large offset index"));
    assert_eq!(fails_reads_like_git(&fixture), 1);
}
