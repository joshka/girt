//! Runtime Git-generated pack/index pairs and exact cat-file comparisons.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::fs;

use girt::{ObjectId, ObjectKind, ObjectReadError, PackLimits, ReadLimits};
use pack_git::{Fixture, git};
use rstest::rstest;

#[rstest]
#[case::ofs_blobs(true, ObjectKind::Blob)]
#[case::ofs_trees(true, ObjectKind::Tree)]
#[case::ofs_commits(true, ObjectKind::Commit)]
#[case::ofs_tags(true, ObjectKind::Tag)]
#[case::ref_blobs(false, ObjectKind::Blob)]
#[case::ref_trees(false, ObjectKind::Tree)]
#[case::ref_commits(false, ObjectKind::Commit)]
#[case::ref_tags(false, ObjectKind::Tag)]
fn agrees_with_git_for_exact_packed_objects(#[case] ofs: bool, #[case] kind: ObjectKind) {
    let fixture = Fixture::new(ofs, 16);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let expected: Vec<_> = fixture
        .records
        .iter()
        .filter(|(_, k, _)| *k == kind)
        .cloned()
        .collect();
    let actual: Vec<_> = expected
        .iter()
        .map(|(id, _, _)| {
            let object = objects.read(*id, ReadLimits::default()).unwrap().unwrap();
            (object.id(), object.kind(), object.into_data())
        })
        .collect();
    let from_git: Vec<_> = expected
        .iter()
        .map(|(id, kind, _)| {
            (
                *id,
                *kind,
                git(
                    fixture.root.path(),
                    &["cat-file", kind.as_str(), &id.to_string()],
                    b"",
                ),
            )
        })
        .collect();
    assert_eq!(actual, expected);
    assert_eq!(actual, from_git);
}

#[test]
fn combines_loose_and_packed_reads_and_reports_absence() {
    let fixture = Fixture::new(true, 4);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let loose = fixture.repo.loose_objects().unwrap();
    let id = loose.write_blob(b"new loose blob").unwrap();
    assert_eq!(
        objects
            .read(id, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        b"new loose blob"
    );
    assert_eq!(
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
    assert_eq!(
        objects
            .read(ObjectId::for_blob(b"absent"), ReadLimits::default())
            .unwrap(),
        None
    );
    assert!(
        matches!(loose.read_blob(fixture.delta, 100000), Err(girt::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
}

#[test]
fn corrupt_loose_copy_shadows_valid_pack() {
    let fixture = Fixture::new(true, 4);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let hex = fixture.ordinary.to_string();
    let directory = fixture.repo.object_dir().join(&hex[..2]);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(&hex[2..]), b"broken zlib").unwrap();
    assert!(matches!(
        objects.read(fixture.ordinary, ReadLimits::default()),
        Err(ObjectReadError::Loose(_))
    ));
}

#[test]
fn snapshots_survive_repack_and_new_reader_reads_repacked_repository() {
    let fixture = Fixture::new(true, 16);
    let before = fixture.repo.objects(PackLimits::default()).unwrap();
    git(fixture.root.path(), &["repack", "-ad"], b"");
    let after = fixture.repo.objects(PackLimits::default()).unwrap();
    assert!(!fixture.index_path.exists());
    assert_eq!(
        before.read(fixture.delta, ReadLimits::default()).unwrap(),
        after.read(fixture.delta, ReadLimits::default()).unwrap()
    );
}

#[rstest]
#[case::bytes(PackLimits { max_bytes: 10, ..PackLimits::default() })]
#[case::count(PackLimits { max_packs: 0, ..PackLimits::default() })]
fn bounds_snapshot_loading(#[case] limits: PackLimits) {
    let fixture = Fixture::new(true, 4);
    assert!(matches!(
        fixture.repo.objects(limits),
        Err(ObjectReadError::Limit(_))
    ));
}

#[test]
fn missing_pack_is_storage_failure_not_object_absence() {
    let fixture = Fixture::new(true, 4);
    fs::remove_file(fixture.index_path.with_extension("pack")).unwrap();
    assert!(matches!(
        fixture.repo.objects(PackLimits::default()),
        Err(ObjectReadError::Io(_))
    ));
}

#[test]
fn unindexed_pack_is_not_yet_published() {
    let fixture = Fixture::new(true, 4);
    std::fs::remove_file(&fixture.index_path).unwrap();
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(fixture.ordinary, ReadLimits::default())
            .unwrap(),
        None
    );
}

/// Installs only into a fresh, private fixture before any reader exists. This is not a live
/// repository publication protocol. Git regenerates an independent index beside the export.
fn verify_export(records: &[(ObjectId, ObjectKind, Vec<u8>)]) {
    use girt::{PackObject, PackWriteLimits, Repository, write_pack};
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &["init", "--bare", "--object-format=sha1", "--template=", "."],
        b"",
    );
    let input: Vec<_> = records
        .iter()
        .map(|(id, kind, data)| PackObject {
            id: *id,
            kind: *kind,
            data,
        })
        .collect();
    let (mut pack, mut index) = (vec![], vec![]);
    let result = write_pack(&input, &mut pack, &mut index, PackWriteLimits::default()).unwrap();
    let basename = format!("objects/pack/pack-{}", result.checksum);
    let pack_path = format!("{basename}.pack");
    let index_path = format!("{basename}.idx");
    fs::write(root.path().join(&pack_path), &pack).unwrap();
    fs::write(root.path().join(&index_path), &index).unwrap();
    git(
        root.path(),
        &[
            "index-pack",
            "--index-version=2",
            "-o",
            "independent.idx",
            &pack_path,
        ],
        b"",
    );
    assert_eq!(
        fs::read(root.path().join("independent.idx")).unwrap(),
        index
    );
    git(root.path(), &["verify-pack", "-v", &index_path], b"");
    let repo = Repository::open(root.path()).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    for (id, kind, expected) in records {
        let restored = objects.read(*id, ReadLimits::default()).unwrap().unwrap();
        assert_eq!(restored.kind(), *kind);
        assert_eq!(restored.data(), expected);
        assert_eq!(
            git(
                root.path(),
                &["cat-file", kind.as_str(), &id.to_string()],
                b""
            ),
            *expected
        );
    }
    assert_eq!(result.pack_bytes, pack.len() as u64);
    assert_eq!(result.index_bytes, index.len() as u64);
}

#[test]
fn git_and_reader_accept_empty_export() {
    verify_export(&[]);
}

#[test]
fn git_and_reader_accept_mixed_export_and_repeated_objects() {
    let fixture = Fixture::new(true, 2);
    let mut records = fixture.records.clone();
    records.push(records[0].clone());
    verify_export(&records);
}

#[rstest]
#[case::binary((0..=255).collect())]
#[case::large((0..4 * 1024 * 1024).map(|n| (n % 251) as u8).collect())]
fn git_and_reader_accept_binary_and_large_exports(#[case] data: Vec<u8>) {
    verify_export(&[(ObjectId::for_blob(&data), ObjectKind::Blob, data)]);
}
