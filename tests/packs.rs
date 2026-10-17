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
        matches!(loose.read_blob(fixture.delta, 100000), Err(girt::loose::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
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
#[case::bytes_sha1(girt::ObjectFormat::Sha1, PackLimits::default().with_max_bytes(10))]
#[case::bytes_sha256(girt::ObjectFormat::Sha256, PackLimits::default().with_max_bytes(10))]
#[case::count_sha1(girt::ObjectFormat::Sha1, PackLimits::default().with_max_packs(0))]
#[case::count_sha256(girt::ObjectFormat::Sha256, PackLimits::default().with_max_packs(0))]
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
        _ => panic!("unsupported hash format in test fixture"),
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
#[case::index_bytes(PackLimits::default().with_max_index_bytes(1), "pack index bytes")]
#[case::handles(PackLimits::default().with_max_open_files(1), "pack file handles")]
#[case::directory(PackLimits::default().with_max_directory_entries(0), "pack directory entries")]
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
    let limits = PackLimits::default()
        .with_max_bytes(index_bytes + pack_bytes)
        .with_max_index_bytes(index_bytes)
        .with_max_open_files(2)
        .with_max_packs(1);
    let objects = fixture.repo.objects(limits).unwrap();
    let read = ReadLimits::default().with_max_input_bytes(0);
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

#[cfg(not(windows))]
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
        _ => panic!("unsupported hash format in test fixture"),
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
    // Finish fixture mutation before opening a reader that excludes writers on Windows.
    drop(file);
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
    let error = objects
        .read(fixture.ordinary, ReadLimits::default())
        .unwrap_err();
    assert!(error_chain(&error).contains("corrupt packed storage"));
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
    assert!(
        error_chain(
            &objects
                .read(fixture.delta, ReadLimits::default())
                .unwrap_err()
        )
        .contains("entry CRC")
    );
    assert!(
        error_chain(
            &objects
                .read(fixture.ordinary, ReadLimits::default())
                .unwrap_err()
        )
        .contains("entry CRC")
    );
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

/// Git writes both representations; only the selected entry/index fields are damaged afterwards.
struct DamagedCopies {
    fixture: Fixture,
    target: ObjectId,
    expected: Vec<u8>,
    damaged: std::path::PathBuf,
    good: std::path::PathBuf,
    offset: usize,
    unique: ObjectId,
}

impl DamagedCopies {
    fn new(format: ObjectFormat, damage: &str, graph: bool) -> Self {
        let mut fixture = Fixture::new(format, true, 4);
        // Keep an unreachable object only in the damaged pack. A rescued root must never
        // authorize discarding it as part of retiring that pack.
        let unique_data = b"unreachable unique object";
        let unique: ObjectId = std::str::from_utf8(&git(
            fixture.root.path(),
            &["hash-object", "-w", "--stdin"],
            unique_data,
        ))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
        fixture
            .records
            .push((unique, ObjectKind::Blob, unique_data.to_vec()));
        let tree = fixture
            .records
            .iter()
            .find(|record| record.1 == ObjectKind::Tree)
            .unwrap()
            .0;
        let swap: ObjectId = std::str::from_utf8(&git(
            fixture.root.path(),
            &["commit-tree", &tree.to_string()],
            b"unreachable different commit\n",
        ))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
        let swap_data = git(
            fixture.root.path(),
            &["cat-file", "commit", &swap.to_string()],
            b"",
        );
        fixture.records.push((swap, ObjectKind::Commit, swap_data));
        let input = fixture
            .records
            .iter()
            .map(|record| format!("{}\n", record.0))
            .collect::<String>();
        let original = git(
            fixture.root.path(),
            &[
                "pack-objects",
                "--stdout",
                "--no-reuse-object",
                "--no-reuse-delta",
                "--window=0",
            ],
            input.as_bytes(),
        );
        fs::remove_file(fixture.index_path.with_extension("pack")).unwrap();
        fs::remove_file(&fixture.index_path).unwrap();
        fs::write(fixture.index_path.with_extension("pack"), original).unwrap();
        git(
            fixture.root.path(),
            &[
                "index-pack",
                fixture.index_path.with_extension("pack").to_str().unwrap(),
            ],
            b"",
        );
        git(fixture.root.path(), &["prune-packed"], b"");
        let record = fixture
            .records
            .iter()
            .find(|record| {
                if graph {
                    record.1 == ObjectKind::Commit
                } else {
                    record.0 == fixture.ordinary
                }
            })
            .unwrap();
        let target = record.0;
        let expected = record.2.clone();
        let (offset, length) = pack_record(&fixture, target);
        let directory = fixture.repo.object_dir().join("pack");
        let good = directory.join("z-good.pack");
        let bytes = git(
            fixture.root.path(),
            &["pack-objects", "--stdout", "--window=0"],
            format!("{target}\n").as_bytes(),
        );
        fs::write(&good, bytes).unwrap();
        git(
            fixture.root.path(),
            &["index-pack", good.to_str().unwrap()],
            b"",
        );
        let damaged = directory.join("a-damaged.pack");
        fs::rename(fixture.index_path.with_extension("pack"), &damaged).unwrap();
        fs::rename(&fixture.index_path, damaged.with_extension("idx")).unwrap();
        let mut pack = fs::read(&damaged).unwrap();
        let mut index = fs::read(damaged.with_extension("idx")).unwrap();
        let count = fixture.records.len();
        let width = format.digest_len();
        let position = index[1032..1032 + count * width]
            .chunks_exact(width)
            .position(|bytes| bytes == target.as_bytes())
            .unwrap();
        let crcs = 1032 + count * width;
        let offsets = crcs + count * 4;
        let other = index[1032..1032 + count * width]
            .chunks_exact(width)
            .position(|name| name == swap.as_bytes())
            .unwrap();
        match damage {
            "crc" => index[crcs + position * 4] ^= 1,
            "zlib" => pack[offset + length - 1] ^= 1,
            "identity" => {
                // Swap complete representations while preserving sorted names, valid CRCs,
                // exact offset tiling and both artifact checksums.
                for byte in 0..4 {
                    index.swap(crcs + position * 4 + byte, crcs + other * 4 + byte);
                    index.swap(offsets + position * 4 + byte, offsets + other * 4 + byte);
                }
            }
            _ => panic!("unknown fixture damage"),
        }
        let offset = u32::from_be_bytes(
            index[offsets + position * 4..offsets + position * 4 + 4]
                .try_into()
                .unwrap(),
        ) as usize;
        rehash_index(format, &mut index);
        fs::write(&damaged, pack).unwrap();
        fs::remove_file(damaged.with_extension("idx")).unwrap();
        fs::write(damaged.with_extension("idx"), index).unwrap();
        Self {
            fixture,
            target,
            expected,
            damaged,
            good,
            offset,
            unique,
        }
    }

    fn git_content(&self, damage: &str) -> Vec<u8> {
        // Git cat-file does not hash intact mislabelled bytes. For identity-only damage,
        // compare with the good Git-generated representation in isolation.
        let index = self.damaged.with_extension("idx");
        let hidden = self.fixture.root.path().join("hidden.idx");
        if damage == "identity" {
            fs::rename(&index, &hidden).unwrap();
        }
        let content = git(
            self.fixture.root.path(),
            &["cat-file", "blob", &self.target.to_string()],
            b"",
        );
        if damage == "identity" {
            fs::rename(hidden, index).unwrap();
        }
        content
    }

    fn write_loose(&self) {
        // hash-object avoids writing an already packed object. Hide its index while Git
        // generates the loose copy, then restore the damaged pair before opening girt.
        let index = self.damaged.with_extension("idx");
        let hidden = self.fixture.root.path().join("hidden.idx");
        fs::rename(&index, &hidden).unwrap();
        git(
            self.fixture.root.path(),
            &["hash-object", "-w", "--stdin"],
            &self.expected,
        );
        fs::rename(hidden, index).unwrap();
        let hex = self.target.to_string();
        assert!(
            self.fixture
                .repo
                .object_dir()
                .join(&hex[..2])
                .join(&hex[2..])
                .exists()
        );
    }

    fn remove_good(&self) {
        fs::remove_file(&self.good).unwrap();
        fs::remove_file(self.good.with_extension("idx")).unwrap();
    }
}

#[rstest]
#[case::crc_sha1(ObjectFormat::Sha1, "crc")]
#[case::crc_sha256(ObjectFormat::Sha256, "crc")]
#[case::zlib_sha1(ObjectFormat::Sha1, "zlib")]
#[case::zlib_sha256(ObjectFormat::Sha256, "zlib")]
#[case::identity_sha1(ObjectFormat::Sha1, "identity")]
#[case::identity_sha256(ObjectFormat::Sha256, "identity")]
fn damaged_pack_reads_another_verified_pack(#[case] format: ObjectFormat, #[case] damage: &str) {
    let copies = DamagedCopies::new(format, damage, false);
    let from_git = copies.git_content(damage);
    let objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(copies.target, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        copies.expected
    );
    assert_eq!(from_git, copies.expected);
    assert_eq!(
        objects
            .read(copies.unique, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        b"unreachable unique object"
    );
    assert_eq!(
        objects
            .clone()
            .read(copies.target, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        copies.expected
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn damaged_pack_reads_verified_loose_fallback(#[case] format: ObjectFormat) {
    let copies = DamagedCopies::new(format, "zlib", false);
    copies.remove_good();
    copies.write_loose();
    let objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(copies.target, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        copies.expected
    );
    assert_eq!(
        git(
            copies.fixture.root.path(),
            &["cat-file", "blob", &copies.target.to_string()],
            b""
        ),
        copies.expected
    );
}

#[rstest]
#[case::crc(ObjectFormat::Sha1, "crc")]
#[case::zlib(ObjectFormat::Sha256, "zlib")]
#[case::identity(ObjectFormat::Sha1, "identity")]
fn damaged_only_copy_names_pack_and_offset_without_content(
    #[case] format: ObjectFormat,
    #[case] damage: &str,
) {
    let copies = DamagedCopies::new(format, damage, false);
    copies.remove_good();
    let mut objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    let error = objects
        .read(copies.target, ReadLimits::default())
        .unwrap_err();
    let chain = error_chain(&error);
    assert!(chain.contains(copies.damaged.to_str().unwrap()), "{chain}");
    assert!(
        chain.contains(&format!("offset {}", copies.offset)),
        "{chain}"
    );
    assert!(chain.contains("corrupt"), "{chain}");
    objects
        .refresh(PackLimits::default(), girt::AlternateLimits::default())
        .unwrap();
    assert_eq!(
        error_chain(
            &objects
                .read(copies.target, ReadLimits::default())
                .unwrap_err()
        ),
        chain
    );
}

fn error_chain(error: &dyn std::error::Error) -> String {
    let mut result = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        result.push_str(&format!(": {error}"));
        source = error.source();
    }
    result
}

#[cfg(unix)]
struct FallbackIsolation;

#[cfg(unix)]
impl girt::retention::MaintenanceIsolation for FallbackIsolation {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
#[rstest]
#[case::gc_crc(ObjectFormat::Sha1, "crc", false)]
#[case::maintenance_zlib(ObjectFormat::Sha256, "zlib", true)]
#[case::gc_identity(ObjectFormat::Sha256, "identity", false)]
#[case::maintenance_identity(ObjectFormat::Sha1, "identity", true)]
fn rescued_damage_never_authorizes_unique_object_deletion(
    #[case] format: ObjectFormat,
    #[case] damage: &str,
    #[case] maintenance: bool,
) {
    let copies = DamagedCopies::new(format, damage, true);
    let objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(copies.target, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        copies.expected
    );
    let inventory = pack_inventory(copies.fixture.repo.object_dir());
    let pack = fs::read(&copies.damaged).unwrap();
    let index = fs::read(copies.damaged.with_extension("idx")).unwrap();
    let policy = girt::retention::RetentionPolicy::default()
        .with_recent_cutoff(std::time::SystemTime::now() + std::time::Duration::from_secs(60));
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let plan = copies.fixture.repo.plan_retention(&policy, &cancel);
    let girt::retention::RetentionOutcome::Incomplete(girt::retention::RetentionError::Read(
        source,
    )) = &plan.outcome
    else {
        panic!("expected a preserved storage failure: {:?}", plan.outcome);
    };
    assert!(source.downcast_ref::<girt::ObjectReadError>().is_some());
    let mut diagnostic = source.to_string();
    let mut cause = source.source();
    while let Some(error) = cause {
        diagnostic.push_str(&error.to_string());
        cause = error.source();
    }
    assert!(diagnostic.contains("a-damaged.pack"));
    assert!(diagnostic.contains("offset "));
    assert!(diagnostic.contains("corrupt"));
    if maintenance {
        let error = copies
            .fixture
            .repo
            .run_maintenance(
                &mut FallbackIsolation,
                &policy,
                girt::retention::RepackLimits::default(),
                &cancel,
            )
            .unwrap_err();
        assert!(
            error
                .report
                .pruned
                .is_none_or(|report| report.deleted.is_empty())
        );
    } else {
        copies
            .fixture
            .repo
            .collect_garbage(&mut FallbackIsolation, &policy, &cancel)
            .unwrap_err();
    }
    assert_eq!(pack_inventory(copies.fixture.repo.object_dir()), inventory);
    assert_eq!(fs::read(&copies.damaged).unwrap(), pack);
    assert_eq!(
        fs::read(copies.damaged.with_extension("idx")).unwrap(),
        index
    );
    // This object is unique to the damaged pack, but its own representation remains intact.
    let unique = copies
        .fixture
        .records
        .iter()
        .find(|record| record.0 == copies.unique)
        .unwrap();
    assert_eq!(
        objects
            .read(unique.0, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        unique.2
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn damaged_pack_reads_verified_alternate(#[case] format: ObjectFormat) {
    let copies = DamagedCopies::new(format, "crc", false);
    let alternate = tempfile::tempdir().unwrap();
    fs::create_dir(alternate.path().join("pack")).unwrap();
    fs::rename(&copies.good, alternate.path().join("pack/good.pack")).unwrap();
    fs::rename(
        copies.good.with_extension("idx"),
        alternate.path().join("pack/good.idx"),
    )
    .unwrap();
    fs::write(
        copies.fixture.repo.object_dir().join("info/alternates"),
        format!("{}\n", alternate.path().display()),
    )
    .unwrap();
    let objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(copies.target, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        copies.expected
    );
}

#[test]
fn damaged_fallback_copy_is_checked_too() {
    let copies = DamagedCopies::new(ObjectFormat::Sha1, "crc", false);
    let mut index = fs::read(copies.good.with_extension("idx")).unwrap();
    index[1032 + 20] ^= 1;
    rehash_index(ObjectFormat::Sha1, &mut index);
    fs::remove_file(copies.good.with_extension("idx")).unwrap();
    fs::write(copies.good.with_extension("idx"), index).unwrap();
    let objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    let error = objects
        .read(copies.target, ReadLimits::default())
        .unwrap_err();
    assert!(error_chain(&error).contains(copies.damaged.to_str().unwrap()));
}

#[test]
fn read_limit_does_not_poison_a_pack_entry() {
    let fixture = Fixture::new(ObjectFormat::Sha1, true, 4);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        objects.read(
            fixture.ordinary,
            ReadLimits::default().with_max_object_bytes(1)
        ),
        Err(ObjectReadError::Limit(_))
    ));
    assert_eq!(
        objects
            .read(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.ordinary
    );
}

#[test]
fn corrupt_loose_fallback_still_reports_the_damaged_pack() {
    let copies = DamagedCopies::new(ObjectFormat::Sha1, "crc", false);
    copies.remove_good();
    copies.write_loose();
    let hex = copies.target.to_string();
    let loose = copies
        .fixture
        .repo
        .object_dir()
        .join(&hex[..2])
        .join(&hex[2..]);
    fs::remove_file(&loose).unwrap();
    fs::write(loose, b"damaged loose stream").unwrap();
    let objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    let error = objects
        .read(copies.target, ReadLimits::default())
        .unwrap_err();
    assert!(error_chain(&error).contains(copies.damaged.to_str().unwrap()));
}

#[cfg(unix)]
fn pack_inventory(objects: &Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(objects.join("pack"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    files.sort();
    files
}

#[test]
fn corrupt_loose_fallback_does_not_hide_a_verified_alternate() {
    let copies = DamagedCopies::new(ObjectFormat::Sha1, "crc", false);
    let alternate = tempfile::tempdir().unwrap();
    fs::create_dir(alternate.path().join("pack")).unwrap();
    fs::rename(&copies.good, alternate.path().join("pack/good.pack")).unwrap();
    fs::rename(
        copies.good.with_extension("idx"),
        alternate.path().join("pack/good.idx"),
    )
    .unwrap();
    let hex = copies.target.to_string();
    let directory = copies.fixture.repo.object_dir().join(&hex[..2]);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join(&hex[2..]), b"corrupt loose fallback").unwrap();
    fs::write(
        copies.fixture.repo.object_dir().join("info/alternates"),
        format!("{}\n", alternate.path().display()),
    )
    .unwrap();
    let objects = copies.fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects
            .read(copies.target, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        copies.expected
    );
}

/// Cold readers on the same thread peel correctly across mixed object reads and limit failures.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn packed_tag_peeling_survives_interleaved_read_limits(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 8);
    let tag = fixture
        .records
        .iter()
        .find(|(_, kind, _)| *kind == ObjectKind::Tag)
        .unwrap()
        .0;
    let target_bytes = git(
        fixture.root.path(),
        &["rev-parse", "refs/tags/packed^{}"],
        b"",
    );
    let target: ObjectId = std::str::from_utf8(&target_bytes)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let first = fixture.repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        first
            .peel(tag, girt::PeelLimits::trusted(), &cancel)
            .unwrap()
            .target,
        target
    );
    let second = fixture.repo.objects(PackLimits::default()).unwrap();
    assert!(
        second
            .read(tag, ReadLimits::default().with_max_object_bytes(1))
            .is_err()
    );
    let third = fixture.repo.objects(PackLimits::default()).unwrap();
    let blob = third
        .read(fixture.delta, ReadLimits::trusted())
        .unwrap()
        .unwrap();
    assert_eq!(
        blob.data(),
        git(
            fixture.root.path(),
            &["cat-file", "blob", &fixture.delta.to_string()],
            b""
        )
    );
    assert_eq!(
        second
            .peel(tag, girt::PeelLimits::trusted(), &cancel)
            .unwrap()
            .target,
        target
    );
    assert_eq!(
        first
            .peel(tag, girt::PeelLimits::trusted(), &cancel)
            .unwrap()
            .target,
        target
    );
}
