//! MIDX observations use only isolated Git CLI output, never implementation source.
#[path = "support/pack_git.rs"]
#[allow(dead_code)]
mod pack_git;

use girt::multi_pack_index::{Error, Limits, MultiPackIndex};
use girt::{ObjectFormat, Repository};
use rstest::rstest;

#[rstest]
#[case::sha1_v1(ObjectFormat::Sha1, "1")]
#[case::sha1_v2(ObjectFormat::Sha1, "2")]
#[case::sha256_v1(ObjectFormat::Sha256, "1")]
#[case::sha256_v2(ObjectFormat::Sha256, "2")]
fn git_written_index_resolves_and_reads_every_object(
    #[case] format: ObjectFormat,
    #[case] version: &str,
) {
    let fixture = pack_git::Fixture::new(format, true, 8);
    pack_git::git(
        fixture.root.path(),
        &[
            "-c",
            &format!("midx.version={version}"),
            "multi-pack-index",
            "write",
        ],
        b"",
    );
    pack_git::git(fixture.root.path(), &["multi-pack-index", "verify"], b"");
    let bytes = std::fs::read(fixture.root.path().join("objects/pack/multi-pack-index")).unwrap();
    assert_eq!(bytes[4].to_string(), version);
    let index = MultiPackIndex::parse(&bytes, format).unwrap();
    assert_eq!(index.len(), fixture.records.len());
    assert_eq!(index.pack_names().len(), 1);
    assert_eq!(index.base_count(), 0);
    check_records(&fixture, &index);
}

fn check_records(fixture: &pack_git::Fixture, index: &MultiPackIndex<'_>) {
    let objects = fixture.repo.objects().unwrap();
    index.verify(&objects).unwrap();
    for (id, kind, data) in &fixture.records {
        assert_eq!(index.find(*id).unwrap().pack, 0);
        let object = objects.read(*id).unwrap().unwrap();
        assert_eq!(object.kind(), *kind);
        assert_eq!(object.data(), data);
    }
}

#[rstest]
#[case::corrupt(false)]
#[case::disabled(true)]
fn unusable_optional_index_preserves_packed_reads(#[case] disabled: bool) {
    let fixture = pack_git::Fixture::new(ObjectFormat::Sha1, true, 8);
    std::fs::write(
        fixture.root.path().join("objects/pack/multi-pack-index"),
        b"MIDXbroken",
    )
    .unwrap();
    pack_git::git(
        fixture.root.path(),
        &[
            "config",
            "core.multiPackIndex",
            if disabled { "false" } else { "true" },
        ],
        b"",
    );
    let repo = Repository::open(fixture.root.path()).unwrap();
    let object = repo
        .objects()
        .unwrap()
        .read(fixture.ordinary)
        .unwrap()
        .unwrap();
    assert_eq!(object.kind(), girt::ObjectKind::Blob);
}

#[test]
fn byte_bound_precedes_header_and_checksum_work() {
    let mut limits = Limits::default();
    limits.max_bytes = 1;
    assert!(matches!(
        MultiPackIndex::parse_with(b"MIDX", ObjectFormat::Sha1, limits),
        Err(Error::Limit("bytes"))
    ));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn incremental_chain_and_unindexed_new_pack_remain_readable(#[case] format: ObjectFormat) {
    let fixture = pack_git::Fixture::new(format, true, 8);
    pack_git::git(
        fixture.root.path(),
        &["multi-pack-index", "write", "--incremental"],
        b"",
    );
    let first = add_pack(fixture.root.path(), b"original second pack");
    pack_git::git(
        fixture.root.path(),
        &["multi-pack-index", "write", "--incremental"],
        b"",
    );
    let outside = add_pack(fixture.root.path(), b"original uncovered pack");
    let objects = fixture.repo.objects().unwrap();
    assert_eq!(objects.read_blob(first).unwrap(), b"original second pack");
    assert_eq!(
        objects.read_blob(outside).unwrap(),
        b"original uncovered pack"
    );
    assert!(objects.read(fixture.ordinary).unwrap().is_some());
}

fn add_pack(directory: &std::path::Path, data: &[u8]) -> girt::ObjectId {
    let output = pack_git::git(directory, &["hash-object", "-w", "--stdin"], data);
    let name = std::str::from_utf8(&output).unwrap().trim();
    pack_git::git(directory, &["pack-objects", "objects/pack/pack"], &output);
    pack_git::git(directory, &["prune-packed"], b"");
    name.parse().unwrap()
}

#[test]
fn checksum_valid_stale_offset_falls_back_to_pack_index() {
    let fixture = pack_git::Fixture::new(ObjectFormat::Sha1, true, 8);
    pack_git::git(fixture.root.path(), &["multi-pack-index", "write"], b"");
    let path = fixture.root.path().join("objects/pack/multi-pack-index");
    let mut bytes = std::fs::read(&path).unwrap();
    invalidate_offsets(&mut bytes);
    std::fs::write(path, bytes).unwrap();
    let objects = fixture.repo.objects().unwrap();
    assert!(objects.read(fixture.ordinary).unwrap().is_some());
}

fn invalidate_offsets(bytes: &mut [u8]) {
    let count = usize::from(bytes[6]);
    for i in 0..count {
        let row = 12 + i * 12;
        if &bytes[row..row + 4] == b"OOFF" {
            let start = u64::from_be_bytes(bytes[row + 4..row + 12].try_into().unwrap()) as usize;
            let end = u64::from_be_bytes(bytes[row + 16..row + 24].try_into().unwrap()) as usize;
            for offset in bytes[start..end].as_chunks_mut::<8>().0 {
                offset[4..].copy_from_slice(&13_u32.to_be_bytes());
            }
        }
    }
    let end = bytes.len() - 20;
    use sha1::Digest;
    let hash = sha1::Sha1::digest(&bytes[..end]);
    bytes[end..].copy_from_slice(&hash);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn bitmap_midx_chunks_are_accepted(#[case] format: ObjectFormat) {
    let fixture = pack_git::Fixture::new(format, true, 8);
    pack_git::git(
        fixture.root.path(),
        &["multi-pack-index", "write", "--bitmap"],
        b"",
    );
    let bytes = std::fs::read(fixture.root.path().join("objects/pack/multi-pack-index")).unwrap();
    let index = MultiPackIndex::parse(&bytes, format).unwrap();
    check_records(&fixture, &index);
}

#[test]
fn explicit_verification_distinguishes_unavailable_packs() {
    let fixture = pack_git::Fixture::new(ObjectFormat::Sha1, true, 8);
    pack_git::git(fixture.root.path(), &["multi-pack-index", "write"], b"");
    let bytes = std::fs::read(fixture.root.path().join("objects/pack/multi-pack-index")).unwrap();
    let index = MultiPackIndex::parse(&bytes, ObjectFormat::Sha1).unwrap();
    let other = tempfile::tempdir().unwrap();
    let repo = Repository::init(
        ObjectFormat::Sha1,
        other.path().join("other.git"),
        girt::InitKind::Bare,
    )
    .unwrap();
    assert!(matches!(
        index.verify(&repo.objects().unwrap()),
        Err(Error::MissingPack)
    ));
}
