//! A damaged `.idx` must fail a read or an explicit verification, never yield wrong content.
//!
//! Opening a store checks the index structure but not its own trailing checksum, as Git does; the
//! checksum belongs to [`girt::Objects::verify_packs`]. Each case damages a Git-generated index and
//! checks every recorded object, with and without a recomputed index checksum, so that the
//! structural checks rather than the checksum are what catch the damage.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::fs;
use std::sync::atomic::AtomicBool;

use girt::{ObjectFormat, ObjectReadError, Objects, PackLimits, ReadLimits};
use pack_git::{Fixture, git, git_output};
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
    fixture.repo.objects(PackLimits::default())
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

/// Every recorded object either fails to read or reads exactly as Git wrote it.
fn assert_no_wrong_content(fixture: &Fixture, objects: &Objects) -> usize {
    let mut failed = 0;
    for (id, kind, data) in &fixture.records {
        match objects.read(*id, ReadLimits::default()) {
            Ok(Some(object)) => {
                assert_eq!(
                    (object.id(), object.kind(), object.data()),
                    (*id, *kind, &data[..])
                );
            }
            Ok(None) | Err(_) => failed += 1,
        }
    }
    failed
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn damaged_index_checksum_reads_like_git_and_fails_verification(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, false, |_, index| *index.last_mut().unwrap() ^= 1);
    // Git reads both a whole and a delta entry through the damaged index.
    for id in [fixture.ordinary, fixture.delta] {
        let (_, _, data) = fixture
            .records
            .iter()
            .find(|record| record.0 == id)
            .unwrap();
        let shown = git(
            fixture.root.path(),
            &["cat-file", "-p", &id.to_string()],
            b"",
        );
        assert_eq!(shown, *data);
    }
    let path = fixture.index_path.to_str().unwrap();
    assert!(
        !git_output(fixture.root.path(), &["verify-pack", path], b"")
            .status
            .success()
    );

    let objects = open(&fixture).unwrap();
    assert_eq!(assert_no_wrong_content(&fixture, &objects), 0);
    assert!(matches!(
        objects.verify_packs(&AtomicBool::new(false)),
        Err(ObjectReadError::Corrupt("index checksum"))
    ));
}

#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn damaged_fanout_fails_open(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    let first = fixture.records[0].0.as_bytes()[0] as usize;
    // Move one entry into the previous bucket: the fanout stays monotonic and keeps the total.
    damage(&fixture, rehash, |_, index| {
        let bucket = 8 + first.max(1) * 4 - 4;
        let value = u32::from_be_bytes(index[bucket..bucket + 4].try_into().unwrap()) + 1;
        index[bucket..bucket + 4].copy_from_slice(&value.to_be_bytes());
    });
    assert_eq!(corrupt_artifacts(open(&fixture)), "index fanout");
}

#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn offset_outside_pack_fails_open(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, rehash, |layout, index| {
        let at = layout.offset(0);
        index[at..at + 4].copy_from_slice(&0x7fff_0000u32.to_be_bytes());
    });
    assert_eq!(
        corrupt_artifacts(open(&fixture)),
        "offset outside pack entries"
    );
}

#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn duplicated_offset_fails_open(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, rehash, |layout, index| {
        let (from, to) = (layout.offset(0), layout.offset(1));
        let copy: [u8; 4] = index[from..from + 4].try_into().unwrap();
        index[to..to + 4].copy_from_slice(&copy);
    });
    assert_eq!(corrupt_artifacts(open(&fixture)), "duplicate pack offset");
}

/// Swapped offsets still tile the pack, so only the entry CRCs can tell them apart.
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
    assert!(assert_no_wrong_content(&fixture, &objects) >= 2);
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
    let objects = open(&fixture).unwrap();
    assert_eq!(assert_no_wrong_content(&fixture, &objects), 1);
    assert!(objects.read(damaged, ReadLimits::default()).is_err());
    if !rehash {
        assert!(matches!(
            objects.verify_packs(&AtomicBool::new(false)),
            Err(ObjectReadError::Corrupt("index checksum"))
        ));
    }
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn damaged_trailer_pack_hash_fails_open(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    damage(&fixture, false, |_, index| {
        let at = index.len() - 2 * format.digest_len();
        index[at] ^= 1;
    });
    assert_eq!(
        corrupt_artifacts(open(&fixture)),
        "index/pack checksum disagreement"
    );
}

#[rstest]
#[case::sha1_stale(ObjectFormat::Sha1, false)]
#[case::sha1_rehashed(ObjectFormat::Sha1, true)]
#[case::sha256_stale(ObjectFormat::Sha256, false)]
#[case::sha256_rehashed(ObjectFormat::Sha256, true)]
fn damaged_table_length_fails_open(#[case] format: ObjectFormat, #[case] rehash: bool) {
    let fixture = Fixture::new(format, true, 8);
    // Four stray bytes before the trailer make the large-offset table a partial entry.
    damage(&fixture, rehash, |_, index| {
        let at = index.len() - 2 * format.digest_len();
        index.splice(at..at, [0; 4]);
    });
    assert_eq!(corrupt_artifacts(open(&fixture)), "index table length");
}

fn git_rejects(fixture: &Fixture) -> bool {
    let index = fixture.index_path.to_str().unwrap();
    !git_output(fixture.root.path(), &["verify-pack", index], b"")
        .status
        .success()
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
    assert!(open(&fixture).is_err());
}

/// A large-offset slot beyond the one-slot table is rejected by Git and by opening.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn large_offset_slot_outside_table_fails_like_git(#[case] format: ObjectFormat) {
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
    assert_eq!(corrupt_artifacts(open(&fixture)), "large offset index");
}
