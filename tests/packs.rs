//! Runtime Git-generated pack/index pairs and exact cat-file comparisons.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::fs;

use girt::{ObjectId, ObjectKind, ObjectReadError, PackLimits, ReadLimits};
use pack_git::{Fixture, git};
use rstest::rstest;

#[rstest]
#[case::ofs_blobs_sha1(girt::ObjectFormat::Sha1, true, ObjectKind::Blob)]
#[case::ofs_blobs_sha256(girt::ObjectFormat::Sha256, true, ObjectKind::Blob)]
#[case::ofs_trees_sha1(girt::ObjectFormat::Sha1, true, ObjectKind::Tree)]
#[case::ofs_trees_sha256(girt::ObjectFormat::Sha256, true, ObjectKind::Tree)]
#[case::ofs_commits_sha1(girt::ObjectFormat::Sha1, true, ObjectKind::Commit)]
#[case::ofs_commits_sha256(girt::ObjectFormat::Sha256, true, ObjectKind::Commit)]
#[case::ofs_tags_sha1(girt::ObjectFormat::Sha1, true, ObjectKind::Tag)]
#[case::ofs_tags_sha256(girt::ObjectFormat::Sha256, true, ObjectKind::Tag)]
#[case::ref_blobs_sha1(girt::ObjectFormat::Sha1, false, ObjectKind::Blob)]
#[case::ref_blobs_sha256(girt::ObjectFormat::Sha256, false, ObjectKind::Blob)]
#[case::ref_trees_sha1(girt::ObjectFormat::Sha1, false, ObjectKind::Tree)]
#[case::ref_trees_sha256(girt::ObjectFormat::Sha256, false, ObjectKind::Tree)]
#[case::ref_commits_sha1(girt::ObjectFormat::Sha1, false, ObjectKind::Commit)]
#[case::ref_commits_sha256(girt::ObjectFormat::Sha256, false, ObjectKind::Commit)]
#[case::ref_tags_sha1(girt::ObjectFormat::Sha1, false, ObjectKind::Tag)]
#[case::ref_tags_sha256(girt::ObjectFormat::Sha256, false, ObjectKind::Tag)]
fn agrees_with_git_for_exact_packed_objects(
    #[case] format: girt::ObjectFormat,
    #[case] ofs: bool,
    #[case] kind: ObjectKind,
) {
    let fixture = Fixture::new(format, ofs, 16);
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn combines_loose_and_packed_reads_and_reports_absence(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let loose = fixture.repo.loose_objects();
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
            .read(ObjectId::for_blob(format, b"absent"), ReadLimits::default())
            .unwrap(),
        None
    );
    assert!(
        matches!(loose.read_blob(fixture.delta, 100000), Err(girt::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn corrupt_loose_copy_shadows_valid_pack(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn snapshots_survive_repack_and_new_reader_reads_repacked_repository(
    #[case] format: girt::ObjectFormat,
) {
    let fixture = Fixture::new(format, true, 16);
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
#[case::bytes_sha1(girt::ObjectFormat::Sha1, PackLimits { max_bytes: 10, ..PackLimits::default() })]
#[case::bytes_sha256(girt::ObjectFormat::Sha256, PackLimits { max_bytes: 10, ..PackLimits::default() })]
#[case::count_sha1(girt::ObjectFormat::Sha1, PackLimits { max_packs: 0, ..PackLimits::default() })]
#[case::count_sha256(girt::ObjectFormat::Sha256, PackLimits { max_packs: 0, ..PackLimits::default() })]
fn bounds_snapshot_loading(#[case] format: girt::ObjectFormat, #[case] limits: PackLimits) {
    let fixture = Fixture::new(format, true, 4);
    assert!(matches!(
        fixture.repo.objects(limits),
        Err(ObjectReadError::Limit(_))
    ));
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn missing_pack_is_storage_failure_not_object_absence(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    fs::remove_file(fixture.index_path.with_extension("pack")).unwrap();
    assert!(matches!(
        fixture.repo.objects(PackLimits::default()),
        Err(ObjectReadError::Path { path, source }) if path == fixture.repo.object_dir().join("pack").join(fixture.index_path.file_name().unwrap()).with_extension("pack") && source.kind() == std::io::ErrorKind::NotFound
    ));
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn unindexed_pack_is_not_yet_published(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
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
fn verify_export(format: girt::ObjectFormat, records: &[(ObjectId, ObjectKind, Vec<u8>)]) {
    verify_compressed_export(format, records, girt::PackCompression::Ordinary, false);
}

fn verify_compressed_export(
    format: girt::ObjectFormat,
    records: &[(ObjectId, ObjectKind, Vec<u8>)],
    compression: girt::PackCompression,
    expect_delta: bool,
) {
    use girt::{PackObject, PackWriteLimits, Repository, write_pack_with_compression};
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            &format!("--object-format={format}"),
            "--template=",
            ".",
        ],
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
    let result = write_pack_with_compression(
        format,
        &input,
        &mut pack,
        &mut index,
        PackWriteLimits::default(),
        compression,
    )
    .unwrap();
    assert_eq!(result.deltas.entries > 0, expect_delta);
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
    let verified = git(root.path(), &["verify-pack", "-v", &index_path], b"");
    let report = String::from_utf8(verified).unwrap();
    let depths: Vec<usize> = report
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            (fields.len() == 7).then(|| fields[5].parse().unwrap())
        })
        .collect();
    assert_eq!(!depths.is_empty(), expect_delta);
    assert_eq!(depths.len(), result.deltas.entries as usize);
    assert_eq!(
        depths.into_iter().max().unwrap_or(0),
        result.deltas.max_depth
    );
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

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_and_reader_accept_empty_export(#[case] format: girt::ObjectFormat) {
    verify_export(format, &[]);
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_and_reader_accept_mixed_export_and_repeated_objects(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 2);
    let mut records = fixture.records.clone();
    records.push(records[0].clone());
    verify_export(format, &records);
}

#[rstest]
#[case::binary_sha1(girt::ObjectFormat::Sha1, (0..=255).collect())]
#[case::binary_sha256(girt::ObjectFormat::Sha256, (0..=255).collect())]
#[case::large_sha1(girt::ObjectFormat::Sha1, (0..4 * 1024 * 1024).map(|n| (n % 251) as u8).collect())]
#[case::large_sha256(girt::ObjectFormat::Sha256, (0..4 * 1024 * 1024).map(|n| (n % 251) as u8).collect())]
fn git_and_reader_accept_binary_and_large_exports(
    #[case] format: girt::ObjectFormat,
    #[case] data: Vec<u8>,
) {
    verify_export(
        format,
        &[(ObjectId::for_blob(format, &data), ObjectKind::Blob, data)],
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_indexes_and_reads_internal_deltas(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, false, 12);
    verify_compressed_export(
        format,
        &fixture.records,
        girt::PackCompression::Delta(girt::DeltaOptions::default()),
        true,
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn git_reads_shifted_binary_deltas(#[case] format: girt::ObjectFormat) {
    let mut state = 987654321u64;
    let base: Vec<u8> = (0..1048576)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let mut edited = base.clone();
    edited.splice(4000..4013, [0, 255, 1]);
    let records = vec![
        (ObjectId::for_blob(format, &base), ObjectKind::Blob, base),
        (
            ObjectId::for_blob(format, &edited),
            ObjectKind::Blob,
            edited,
        ),
        (ObjectId::for_blob(format, b""), ObjectKind::Blob, vec![]),
        (
            ObjectId::for_blob(format, b"x"),
            ObjectKind::Blob,
            b"x".to_vec(),
        ),
    ];
    verify_compressed_export(
        format,
        &records,
        girt::PackCompression::Delta(girt::DeltaOptions::default()),
        true,
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn corrupt_snapshot_identifies_both_artifacts_and_preserves_cause(
    #[case] format: girt::ObjectFormat,
) {
    let fixture = Fixture::new(format, true, 4);
    let expected_index = fs::canonicalize(&fixture.index_path).unwrap();
    fs::remove_file(&fixture.index_path).unwrap();
    fs::write(&fixture.index_path, b"broken").unwrap();
    let error = fixture.repo.objects(PackLimits::default()).unwrap_err();
    let ObjectReadError::PackArtifacts {
        index,
        pack,
        source,
    } = error
    else {
        panic!("expected artifact context: {error}");
    };
    assert_eq!(index, expected_index);
    assert_eq!(pack, expected_index.with_extension("pack"));
    assert!(matches!(*source, ObjectReadError::Corrupt(_)));
}
