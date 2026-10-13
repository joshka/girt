//! Runtime Git-generated pack/index pairs and exact cat-file comparisons.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use girt::{
    ObjectFormat, ObjectId, ObjectKind, ObjectReadError, PackLimits, ReadLimits, Repository,
};
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
/// Observed with `git cat-file -p`: a corrupt loose copy of a packed object doesn't hide the
/// valid packed copy.
fn corrupt_loose_copy_does_not_hide_valid_pack(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let expected = objects
        .read(fixture.ordinary, ReadLimits::default())
        .unwrap();
    let hex = fixture.ordinary.to_string();
    let directory = fixture.repo.object_dir().join(&hex[..2]);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(&hex[2..]), b"broken zlib").unwrap();
    assert_eq!(
        objects
            .read(fixture.ordinary, ReadLimits::default())
            .unwrap(),
        expected
    );
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

// Change only the documented header version and trailer; Git independently rebuilds the index.
fn rewrite_pack_version(fixture: &Fixture, version: u32, index: u32) {
    use sha2::Digest;

    let path = fixture.index_path.with_extension("pack");
    let mut bytes = fs::read(&path).unwrap();
    let format = fixture.repo.object_format();
    bytes.truncate(bytes.len() - format.digest_len());
    bytes[4..8].copy_from_slice(&version.to_be_bytes());
    let checksum = match format {
        girt::ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes).to_vec(),
        girt::ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes).to_vec(),
    };
    bytes.extend_from_slice(&checksum);
    fs::remove_file(&path).unwrap();
    fs::write(&path, bytes).unwrap();
    fs::remove_file(&fixture.index_path).unwrap();
    let relative = path.strip_prefix(fixture.root.path()).unwrap();
    git(
        fixture.root.path(),
        &[
            "index-pack",
            &format!("--index-version={index}"),
            relative.to_str().unwrap(),
        ],
        b"",
    );
}

#[rstest]
#[case::sha1_v2_v1_ref(girt::ObjectFormat::Sha1, 2, 1, false)]
#[case::sha256_v2_v1_ref(girt::ObjectFormat::Sha256, 2, 1, false)]
#[case::sha1_v3_v1_ofs(girt::ObjectFormat::Sha1, 3, 1, true)]
#[case::sha256_v3_v1_ofs(girt::ObjectFormat::Sha256, 3, 1, true)]
#[case::sha1_v3_v2_ref(girt::ObjectFormat::Sha1, 3, 2, false)]
#[case::sha256_v3_v2_ref(girt::ObjectFormat::Sha256, 3, 2, false)]
fn reads_legacy_indexes_and_version_three_packs(
    #[case] format: girt::ObjectFormat,
    #[case] version: u32,
    #[case] index: u32,
    #[case] ofs: bool,
) {
    let fixture = Fixture::new(format, ofs, 16);
    rewrite_pack_version(&fixture, version, index);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let actual: Vec<_> = fixture
        .records
        .iter()
        .map(|(id, _, _)| {
            let object = objects.read(*id, ReadLimits::default()).unwrap().unwrap();
            (object.id(), object.kind(), object.into_data())
        })
        .collect();
    assert_eq!(actual, fixture.records);
    assert_eq!(
        git(
            fixture.root.path(),
            &["cat-file", "blob", &fixture.delta.to_string()],
            b""
        ),
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
    );
}

#[cfg(unix)]
#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn reads_symlinked_object_directory(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let destination = fixture.root.path().join("external-objects");
    fs::rename(fixture.repo.object_dir(), &destination).unwrap();
    std::os::unix::fs::symlink(&destination, fixture.repo.object_dir()).unwrap();
    let repo = girt::Repository::open(fixture.root.path()).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
}

#[rstest]
#[case::index_bytes(PackLimits { max_index_bytes: 1, ..PackLimits::default() }, "pack index bytes")]
#[case::handles(PackLimits { max_open_files: 1, ..PackLimits::default() }, "pack file handles")]
#[case::directory(PackLimits { max_directory_entries: 0, ..PackLimits::default() }, "pack directory entries")]
fn rejects_file_resource_limits(#[case] limits: PackLimits, #[case] expected: &str) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    assert!(
        matches!(fixture.repo.objects(limits), Err(ObjectReadError::Limit(reason)) if reason == expected)
    );
    // A failed opening leaves no reader state that can poison a later independent attempt.
    assert!(fixture.repo.objects(PackLimits::default()).is_ok());
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn exact_file_limits_and_compressed_work(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let index_bytes = fs::metadata(&fixture.index_path).unwrap().len() as usize;
    let pack_bytes = fs::metadata(fixture.index_path.with_extension("pack"))
        .unwrap()
        .len() as usize;
    let limits = PackLimits {
        max_bytes: index_bytes + pack_bytes,
        max_index_bytes: index_bytes,
        max_open_files: 2,
        max_packs: 1,
        ..PackLimits::default()
    };
    let objects = fixture.repo.objects(limits).unwrap();
    let read = ReadLimits {
        max_input_bytes: 0,
        ..ReadLimits::default()
    };
    assert!(matches!(
        objects.read(fixture.delta, read),
        Err(ObjectReadError::Limit("compressed entry bytes"))
    ));
    assert!(
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .is_some()
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn cancelled_open_and_read_are_retryable(#[case] format: girt::ObjectFormat) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new(format, true, 4);
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        fixture.repo.objects_controlled(
            PackLimits::default(),
            girt::AlternateLimits::default(),
            &cancelled
        ),
        Err(ObjectReadError::Cancelled)
    ));
    cancelled.store(false, Ordering::Relaxed);
    let objects = fixture
        .repo
        .objects_controlled(
            PackLimits::default(),
            girt::AlternateLimits::default(),
            &cancelled,
        )
        .unwrap();
    cancelled.store(true, Ordering::Relaxed);
    assert!(matches!(
        objects.read_controlled(fixture.delta, ReadLimits::default(), &cancelled),
        Err(ObjectReadError::Cancelled)
    ));
    cancelled.store(false, Ordering::Relaxed);
    assert!(
        objects
            .read_controlled(fixture.delta, ReadLimits::default(), &cancelled)
            .unwrap()
            .is_some()
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn cloned_readers_share_files_with_independent_cursors(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 32);
    let reader = fixture.repo.objects(PackLimits::default()).unwrap();
    let other = reader.clone();
    let expected = reader.read(fixture.delta, ReadLimits::default()).unwrap();
    let result = std::thread::scope(|scope| {
        let first = scope.spawn(|| reader.read(fixture.delta, ReadLimits::default()).unwrap());
        let second = scope.spawn(|| other.read(fixture.ordinary, ReadLimits::default()).unwrap());
        (first.join().unwrap(), second.join().unwrap())
    });
    assert_eq!(result.0, expected);
    assert_eq!(result.1.unwrap().id(), fixture.ordinary);
    drop(reader);
    assert_eq!(
        other.read(fixture.delta, ReadLimits::default()).unwrap(),
        expected
    );
}

#[rstest]
#[case::index("idx")]
#[case::pack("pack")]
fn truncated_pinned_artifacts_preserve_io_failure(#[case] extension: &str) {
    let fixture = Fixture::new(girt::ObjectFormat::Sha1, true, 4);
    let path = fs::canonicalize(fixture.index_path.with_extension(extension)).unwrap();
    // Git indexes may be read-only. Recreate this fixture artifact before pinning it.
    let bytes = fs::read(&path).unwrap();
    fs::remove_file(&path).unwrap();
    fs::write(&path, bytes).unwrap();
    let reader = fixture.repo.objects(PackLimits::default()).unwrap();
    // In-place writes violate the immutable-artifact contract; observable short reads must still
    // return an error rather than panic, deadlock or become object absence.
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0)
        .unwrap();
    assert!(
        matches!(reader.read(fixture.delta, ReadLimits::default()), Err(ObjectReadError::Path { path: found, source }) if found == path && source.kind() == std::io::ErrorKind::UnexpectedEof)
    );
}

// Recompute only the independent index checksum, leaving the pack unchanged.
fn rehash_index(format: girt::ObjectFormat, bytes: &mut [u8]) {
    use sha2::Digest;
    let end = bytes.len() - format.digest_len();
    let checksum = match format {
        girt::ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes[..end]).to_vec(),
        girt::ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes[..end]).to_vec(),
    };
    bytes[end..].copy_from_slice(&checksum);
}

fn pack_record(fixture: &Fixture, id: ObjectId) -> (usize, usize) {
    let report = git(
        fixture.root.path(),
        &["verify-pack", "-v", fixture.index_path.to_str().unwrap()],
        b"",
    );
    let report = std::str::from_utf8(&report).unwrap();
    let line = report
        .lines()
        .find(|line| line.starts_with(&id.to_string()))
        .unwrap();
    let fields: Vec<_> = line.split_whitespace().collect();
    (fields[4].parse().unwrap(), fields[3].parse().unwrap())
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn open_rejects_index_pack_trailer_mismatch(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut index = fs::read(&fixture.index_path).unwrap();
    let trailer = index.len() - 2 * format.digest_len();
    index[trailer] ^= 1;
    rehash_index(format, &mut index);
    fs::remove_file(&fixture.index_path).unwrap();
    fs::write(&fixture.index_path, index).unwrap();
    assert!(
        matches!(fixture.repo.objects(PackLimits::default()), Err(ObjectReadError::PackArtifacts { source, .. }) if matches!(*source, ObjectReadError::Corrupt("index/pack checksum disagreement")))
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn open_rejects_truncated_pack(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let (offset, _) = pack_record(&fixture, fixture.delta);
    let file = fs::OpenOptions::new()
        .write(true)
        .open(fixture.index_path.with_extension("pack"))
        .unwrap();
    file.set_len((offset + format.digest_len()) as u64).unwrap();
    assert!(
        matches!(fixture.repo.objects(PackLimits::default()), Err(ObjectReadError::PackArtifacts { source, .. }) if matches!(*source, ObjectReadError::Corrupt("offset outside pack entries")))
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn corrupt_entry_is_detected_on_read_not_open(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let (offset, length) = pack_record(&fixture, fixture.ordinary);
    let path = fixture.index_path.with_extension("pack");
    let mut bytes = fs::read(&path).unwrap();
    // Damage the zlib checksum, preserving entry boundaries and the pack trailer.
    bytes[offset + length - 1] ^= 1;
    fs::remove_file(&path).unwrap();
    fs::write(&path, bytes).unwrap();
    let good = fixture
        .records
        .iter()
        .find(|record| record.1 == ObjectKind::Commit)
        .unwrap();
    assert!(
        !pack_git::git_output(
            fixture.root.path(),
            &["cat-file", "blob", &fixture.ordinary.to_string()],
            b""
        )
        .status
        .success()
    );
    assert_eq!(
        git(
            fixture.root.path(),
            &["cat-file", "commit", &good.0.to_string()],
            b""
        ),
        good.2
    );
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        objects.verify_packs(&std::sync::atomic::AtomicBool::new(false)),
        Err(ObjectReadError::Corrupt(_))
    ));
    assert!(matches!(
        objects.read(fixture.ordinary, ReadLimits::default()),
        Err(ObjectReadError::Corrupt(_))
    ));
    assert_eq!(
        objects
            .read(good.0, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        good.2
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn read_checks_index_crc_even_when_object_identity_matches(#[case] format: girt::ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut index = fs::read(&fixture.index_path).unwrap();
    let width = format.digest_len();
    let count = fixture.records.len();
    let position = index[1032..1032 + count * width]
        .chunks_exact(width)
        .position(|bytes| bytes == fixture.ordinary.as_bytes())
        .unwrap();
    index[1032 + count * width + position * 4] ^= 1;
    rehash_index(format, &mut index);
    fs::remove_file(&fixture.index_path).unwrap();
    fs::write(&fixture.index_path, index).unwrap();
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        objects.read(fixture.delta, ReadLimits::default()),
        Err(ObjectReadError::Corrupt("entry CRC"))
    ));
    assert!(matches!(
        objects.read(fixture.ordinary, ReadLimits::default()),
        Err(ObjectReadError::Corrupt("entry CRC"))
    ));
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn explicit_verification_agrees_with_git_and_is_cancellable(#[case] format: girt::ObjectFormat) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = Fixture::new(format, true, 4);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let cancel = AtomicBool::new(true);
    assert!(matches!(
        objects.verify_packs(&cancel),
        Err(ObjectReadError::Cancelled)
    ));
    cancel.store(false, Ordering::Relaxed);
    objects.verify_packs(&cancel).unwrap();
    git(
        fixture.root.path(),
        &["verify-pack", fixture.index_path.to_str().unwrap()],
        b"",
    );
    assert_eq!(
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
}

#[rstest]
#[case::sha1(girt::ObjectFormat::Sha1)]
#[case::sha256(girt::ObjectFormat::Sha256)]
fn matching_trailers_do_not_replace_whole_pack_verification(#[case] format: girt::ObjectFormat) {
    use std::sync::atomic::AtomicBool;
    let fixture = Fixture::new(format, true, 4);
    let path = fixture.index_path.with_extension("pack");
    let mut pack = fs::read(&path).unwrap();
    let mut index = fs::read(&fixture.index_path).unwrap();
    *pack.last_mut().unwrap() ^= 1;
    let position = index.len() - format.digest_len() - 1;
    index[position] ^= 1;
    rehash_index(format, &mut index);
    fs::remove_file(&path).unwrap();
    fs::write(&path, &pack).unwrap();
    fs::remove_file(&fixture.index_path).unwrap();
    fs::write(&fixture.index_path, &index).unwrap();
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.ordinary
    );
    assert!(matches!(
        objects.verify_packs(&AtomicBool::new(false)),
        Err(ObjectReadError::Corrupt("pack checksum"))
    ));
    assert!(
        !pack_git::git_output(
            fixture.root.path(),
            &["verify-pack", fixture.index_path.to_str().unwrap()],
            b""
        )
        .status
        .success()
    );
    assert_eq!(fs::read(&path).unwrap(), pack);
    assert_eq!(fs::read(&fixture.index_path).unwrap(), index);
}

// Whole-history reads compared byte for byte with `git cat-file --batch`. Reads share decoded
// payloads with the delta base cache, so these read objects in several orders, as requested
// objects and as bases, and after a caller has taken and changed a payload.

/// Bytes deterministic for a seed, so successive versions differ in scattered places.
fn noise(seed: u64, length: usize) -> Vec<u8> {
    let mut state = seed;
    (0..length)
        .map(|_| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 56) as u8
        })
        .collect()
}

/// A history whose large file (over 1 MiB) and small text files change in every commit, repacked
/// by Git with deep delta chains.
fn history(format: ObjectFormat) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let path = root.path();
    git(
        path,
        &[
            "init",
            "--quiet",
            &format!("--object-format={format}"),
            "--template=",
            "--initial-branch=main",
            ".",
        ],
        b"",
    );
    let mut large = noise(1, 1536 * 1024);
    for version in 0..12u64 {
        let edit = noise(version + 100, 4096);
        let at = (version as usize * 97_331) % (large.len() - edit.len());
        large[at..at + edit.len()].copy_from_slice(&edit);
        large.extend_from_slice(&noise(version + 200, 3000));
        fs::write(path.join("large.bin"), &large).unwrap();
        for file in 0..6 {
            let text = (0..200)
                .map(|line| {
                    let changed = line % 37 == version as usize;
                    format!(
                        "file {file} line {line} {}\n",
                        if changed { version } else { 0 }
                    )
                })
                .collect::<String>();
            fs::write(path.join(format!("small-{file}.txt")), text).unwrap();
        }
        git(path, &["add", "--all"], b"");
        git(
            path,
            &["commit", "--quiet", "-m", &format!("version {version}")],
            b"",
        );
    }
    git(
        path,
        &[
            "repack",
            "-a",
            "-d",
            "-f",
            "-q",
            "--depth=50",
            "--window=50",
        ],
        b"",
    );
    root
}

/// Every reachable object in `git rev-list --objects --all` order, with Git's kind and bytes.
fn git_objects(path: &Path) -> Vec<(ObjectId, ObjectKind, Vec<u8>)> {
    let listing = git(path, &["rev-list", "--objects", "--all"], b"");
    let ids: Vec<ObjectId> = String::from_utf8(listing)
        .unwrap()
        .lines()
        .map(|line| line.split(' ').next().unwrap().parse().unwrap())
        .collect();
    let request: String = ids.iter().map(|id| format!("{id}\n")).collect();
    let batch = git(path, &["cat-file", "--batch"], request.as_bytes());
    let mut rest = batch.as_slice();
    let objects = ids
        .iter()
        .map(|id| {
            let newline = rest.iter().position(|&byte| byte == b'\n').unwrap();
            let header = std::str::from_utf8(&rest[..newline]).unwrap();
            let mut fields = header.split(' ');
            assert_eq!(fields.next(), Some(id.to_string().as_str()));
            let kind = match fields.next().unwrap() {
                "commit" => ObjectKind::Commit,
                "tree" => ObjectKind::Tree,
                "blob" => ObjectKind::Blob,
                "tag" => ObjectKind::Tag,
                other => panic!("unexpected kind {other}"),
            };
            let size: usize = fields.next().unwrap().parse().unwrap();
            let data = rest[newline + 1..newline + 1 + size].to_vec();
            assert_eq!(rest[newline + 1 + size], b'\n');
            rest = &rest[newline + 2 + size..];
            (*id, kind, data)
        })
        .collect();
    assert!(rest.is_empty());
    objects
}

/// Number of pack entries whose delta chain has at least `depth` links, from `git verify-pack`.
fn chains_at_least(path: &Path, depth: usize) -> usize {
    let pack_dir = path.join(".git/objects/pack");
    let index = fs::read_dir(&pack_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|file| file.extension().is_some_and(|extension| extension == "idx"))
        .unwrap();
    let report = git(path, &["verify-pack", "-v", index.to_str().unwrap()], b"");
    String::from_utf8(report)
        .unwrap()
        .lines()
        .filter(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            fields.len() == 7 && fields[5].parse::<usize>().is_ok_and(|chain| chain >= depth)
        })
        .count()
}

fn read(repo: &Repository, id: ObjectId) -> (ObjectKind, Vec<u8>) {
    let objects = repo.objects(PackLimits::default()).unwrap();
    let object = objects.read(id, ReadLimits::trusted()).unwrap().unwrap();
    (object.kind(), object.into_data())
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn full_history_reads_match_git_in_every_order(#[case] format: ObjectFormat) {
    let root = history(format);
    assert!(
        chains_at_least(root.path(), 5) > 0,
        "fixture must contain deep delta chains"
    );
    let expected = git_objects(root.path());
    assert!(
        expected.iter().any(|(_, _, data)| data.len() > 1024 * 1024),
        "fixture must contain objects larger than the initial reservation"
    );
    let repo = Repository::open(root.path()).unwrap();
    // One reader for every pass, so later passes hit entries cached by earlier ones.
    let objects = repo.objects(PackLimits::default()).unwrap();
    let forward: Vec<usize> = (0..expected.len()).collect();
    let reverse: Vec<usize> = forward.iter().rev().copied().collect();
    for order in [&forward, &reverse, &forward] {
        for &index in order {
            let (id, kind, data) = &expected[index];
            let object = objects.read(*id, ReadLimits::trusted()).unwrap().unwrap();
            assert_eq!(
                (object.kind(), object.data()),
                (*kind, data.as_slice()),
                "{id}"
            );
            assert_eq!(object.id(), *id);
            assert_eq!(object.into_data(), *data, "{id}");
        }
    }
    // A fresh reader agrees too, with nothing cached.
    for (id, kind, data) in &expected {
        assert_eq!(read(&repo, *id), (*kind, data.clone()));
    }
}

/// A caller owns the payload it takes: changing it cannot alter the cache that later reads, or
/// later delta reconstructions, use.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn taken_payloads_do_not_alias_cached_bases(#[case] format: ObjectFormat) {
    let root = history(format);
    let expected = git_objects(root.path());
    let by_id: HashMap<_, _> = expected
        .iter()
        .map(|(id, kind, data)| (*id, (*kind, data.clone())))
        .collect();
    let repo = Repository::open(root.path()).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    for round in 0..3 {
        for (id, _, _) in &expected {
            let object = objects.read(*id, ReadLimits::trusted()).unwrap().unwrap();
            let mut data = object.into_data();
            data.iter_mut().for_each(|byte| *byte = !*byte);
            data.push(round);
        }
        for (id, _, _) in expected.iter().rev() {
            let object = objects.read(*id, ReadLimits::trusted()).unwrap().unwrap();
            assert_eq!(
                (object.kind(), object.into_data()),
                by_id[id].clone(),
                "{id}"
            );
        }
    }
}

/// Readers on several threads share one cache and still each get Git's bytes.
#[test]
fn concurrent_full_reads_match_git() {
    let root = history(ObjectFormat::Sha1);
    let expected = git_objects(root.path());
    let repo = Repository::open(root.path()).unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    std::thread::scope(|scope| {
        for thread in 0..4 {
            let objects = &objects;
            let expected = &expected;
            scope.spawn(move || {
                for step in 0..expected.len() {
                    let (id, kind, data) = &expected[(step * (thread + 1)) % expected.len()];
                    let object = objects.read(*id, ReadLimits::trusted()).unwrap().unwrap();
                    assert_eq!((object.kind(), object.data()), (*kind, data.as_slice()));
                    let mut owned = object.into_data();
                    owned.fill(thread as u8);
                }
            });
        }
    });
}
