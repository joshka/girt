//! Original alternate-store graphs and independent Git executable observations.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::fs;
use std::path::{Path, PathBuf};

use girt::{
    AlternateLimits, ObjectFormat, ObjectId, ObjectReadError, PackLimits, ReadLimits, Repository,
};
use pack_git::{Fixture, git};
use rstest::rstest;

fn init(path: &Path, format: ObjectFormat) -> Repository {
    fs::create_dir_all(path).unwrap();
    git(
        path,
        &[
            "init",
            "--bare",
            "--template=",
            &format!("--object-format={format}"),
        ],
        b"",
    );
    Repository::open(path).unwrap()
}

fn alternate(repo: &Repository, bytes: &[u8]) {
    fs::write(repo.object_dir().join("info/alternates"), bytes).unwrap();
}

fn read(repo: &Repository, id: ObjectId) -> Option<Vec<u8>> {
    repo.objects(PackLimits::default())
        .unwrap()
        .read(id, ReadLimits::default())
        .unwrap()
        .map(|o| o.into_data())
}

fn snapshot(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut result = Vec::new();
    for entry in fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            result.extend(snapshot(&entry.path()));
        } else {
            result.push((entry.path(), fs::read(entry.path()).unwrap()));
        }
    }
    result.sort();
    result
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn reads_nested_shared_cycles_after_move_without_borrowed_writes(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let a = init(&root.path().join("a"), format);
    let b = init(&root.path().join("b"), format);
    let c = init(&root.path().join("c"), format);
    alternate(
        &a,
        b"../../missing/objects\n../../b/objects\n../../c/objects\n",
    );
    alternate(&b, b"../../c/objects\n");
    alternate(&c, b"../../a/objects\n");
    let id = c.loose_objects().write_blob(b"borrowed graph").unwrap();
    let before = snapshot(c.git_dir());
    assert_eq!(read(&a, id).unwrap(), b"borrowed graph");
    assert_eq!(
        git(a.git_dir(), &["cat-file", "blob", &id.to_string()], b""),
        b"borrowed graph"
    );
    assert!(read(&a, ObjectId::for_blob(format, b"absent")).is_none());
    let written = a.loose_objects().write_blob(b"primary only").unwrap();
    assert!(c.loose_objects().read_blob(written, 100).is_err());
    assert_eq!(snapshot(c.git_dir()), before);
    let moved = tempfile::tempdir().unwrap();
    let destination = moved.path().join("graph");
    fs::rename(root.path(), &destination).unwrap();
    let reopened = Repository::open(destination.join("a")).unwrap();
    assert_eq!(read(&reopened, id).unwrap(), b"borrowed graph");
    assert_eq!(
        git(
            reopened.git_dir(),
            &["cat-file", "blob", &id.to_string()],
            b""
        ),
        b"borrowed graph"
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn reads_borrowed_packs_with_aggregate_limits(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), format);
    alternate(&a, fixture.repo.object_dir().as_os_str().as_encoded_bytes());
    let before = snapshot(fixture.root.path());
    assert_eq!(read(&a, fixture.delta), read(&fixture.repo, fixture.delta));
    assert_eq!(
        git(
            a.git_dir(),
            &["cat-file", "blob", &fixture.delta.to_string()],
            b""
        ),
        read(&a, fixture.delta).unwrap()
    );
    assert!(matches!(
        a.objects(PackLimits {
            max_packs: 0,
            ..PackLimits::default()
        }),
        Err(ObjectReadError::Limit("pack count"))
    ));
    assert!(matches!(
        a.objects(PackLimits {
            max_bytes: 1,
            ..PackLimits::default()
        }),
        Err(ObjectReadError::Limit("pack snapshot bytes"))
    ));
    assert_eq!(snapshot(fixture.root.path()), before);
    // Access the remaining fixture fields to retain the shared generator's warning-free contract.
    assert!(!fixture.records.is_empty());
    assert!(fixture.index_path.exists());
    assert!(read(&a, fixture.ordinary).is_some());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn topology_is_fixed_until_reopen_and_loose_objects_remain_live(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let a = init(&root.path().join("a"), format);
    let b = init(&root.path().join("b"), format);
    let c = init(&root.path().join("c"), format);
    alternate(&a, b"../../b/objects\n");
    let old = a.objects(PackLimits::default()).unwrap();
    // A joined writer is a deterministic edit after snapshot construction.
    let metadata = a.object_dir().join("info/alternates");
    std::thread::spawn(move || fs::write(metadata, b"../../c/objects\n").unwrap())
        .join()
        .unwrap();
    let old_id = b.loose_objects().write_blob(b"live old store").unwrap();
    let new_id = c.loose_objects().write_blob(b"new topology").unwrap();
    assert!(old.read(old_id, ReadLimits::default()).unwrap().is_some());
    assert!(old.read(new_id, ReadLimits::default()).unwrap().is_none());
    assert!(read(&a, new_id).is_some());
    assert!(read(&a, old_id).is_none());
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn corrupt_primary_duplicate_does_not_fall_through(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let a = init(&root.path().join("a"), format);
    let b = init(&root.path().join("b"), format);
    alternate(&a, b"../../b/objects\n");
    let id = b.loose_objects().write_blob(b"duplicate").unwrap();
    a.loose_objects().write_blob(b"duplicate").unwrap();
    let hex = id.to_string();
    fs::write(a.object_dir().join(&hex[..2]).join(&hex[2..]), b"corrupt").unwrap();
    let objects = a.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        objects.read(id, ReadLimits::default()),
        Err(ObjectReadError::Loose(_))
    ));
}

#[rstest]
#[case::stores(AlternateLimits { max_stores: 1, ..AlternateLimits::default() }, "alternate store count")]
#[case::entries(AlternateLimits { max_entries: 0, ..AlternateLimits::default() }, "alternate path count")]
#[case::bytes(AlternateLimits { max_bytes: 1, ..AlternateLimits::default() }, "alternate metadata bytes")]
#[case::paths(AlternateLimits { max_path_bytes: 0, ..AlternateLimits::default() }, "alternate resolved path bytes")]
fn bounds_the_complete_graph(#[case] limits: AlternateLimits, #[case] reason: &str) {
    let root = tempfile::tempdir().unwrap();
    let a = init(&root.path().join("a"), ObjectFormat::Sha1);
    init(&root.path().join("b"), ObjectFormat::Sha1);
    alternate(&a, b"../../b/objects\n");
    assert!(
        matches!(a.objects_with_alternates(PackLimits::default(), limits), Err(ObjectReadError::Limit(found)) if found == reason)
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn aliases_are_visited_once_and_primary_survives_missing_alternate(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), format);
    alternate(&a, b".\n../objects\n../../absent/objects\n");
    let id = a.loose_objects().write_blob(b"primary").unwrap();
    let limits = AlternateLimits {
        max_stores: 1,
        ..AlternateLimits::default()
    };
    let objects = a
        .objects_with_alternates(PackLimits::default(), limits)
        .unwrap();
    assert_eq!(
        objects
            .read(id, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        b"primary"
    );
    assert!(
        objects
            .read(
                ObjectId::for_blob(format, b"missing"),
                ReadLimits::default()
            )
            .unwrap()
            .is_none()
    );
}

#[rstest]
#[case::noop("noop", "arbitrary")]
#[case::precious("preciousObjects", "true")]
#[case::partial("partialClone", "origin")]
fn recognizes_known_extensions_without_fetching(
    #[case] name: &str,
    #[case] value: &str,
    #[values(ObjectFormat::Sha1, ObjectFormat::Sha256)] format: ObjectFormat,
) {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), format);
    let id = a
        .loose_objects()
        .write_blob(b"complete local object")
        .unwrap();
    git(
        a.git_dir(),
        &["config", "core.repositoryformatversion", "1"],
        b"",
    );
    git(
        a.git_dir(),
        &["config", &format!("extensions.{name}"), value],
        b"",
    );
    fs::write(
        a.object_dir().join("info/http-alternates"),
        b"https://invalid.example/objects\n",
    )
    .unwrap();
    let reopened = Repository::open(root.path()).unwrap();
    assert_eq!(read(&reopened, id).unwrap(), b"complete local object");
    assert_eq!(
        git(a.git_dir(), &["cat-file", "blob", &id.to_string()], b""),
        b"complete local object"
    );
    assert!(
        read(
            &reopened,
            ObjectId::for_blob(format, b"unavailable promised object")
        )
        .is_none()
    );
}

#[rstest]
#[case::nul(b"bad\0path\n")]
#[case::quote(b"\"unfinished\n")]
#[case::escape(b"\"bad\\q\"\n")]
fn malformed_records_are_explicit(#[case] bytes: &[u8]) {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), ObjectFormat::Sha1);
    alternate(&a, bytes);
    assert!(matches!(
        a.objects(PackLimits::default()),
        Err(ObjectReadError::Alternate { .. })
    ));
}

#[test]
fn metadata_io_error_retains_its_cause() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), ObjectFormat::Sha1);
    fs::create_dir(a.object_dir().join("info/alternates")).unwrap();
    let error = a.objects(PackLimits::default()).unwrap_err();
    assert!(
        matches!(&error, ObjectReadError::Path { path, .. } if path.ends_with("info/alternates"))
    );
    assert!(std::error::Error::source(&error).is_some());
}

#[cfg(target_os = "linux")]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn follows_symlink_aliases_and_non_utf8_paths(#[case] format: ObjectFormat) {
    use std::os::unix::ffi::OsStrExt;
    let root = tempfile::tempdir().unwrap();
    let a = init(&root.path().join("a"), format);
    let name = std::ffi::OsStr::from_bytes(b"borrowed-\xff");
    let b = init(&root.path().join(name), format);
    std::os::unix::fs::symlink(b.object_dir(), root.path().join("alias")).unwrap();
    alternate(&a, b"\"../../borrowed-\\377/objects\"\n../../alias\n");
    let id = b.loose_objects().write_blob(b"byte path").unwrap();
    let limits = AlternateLimits {
        max_stores: 2,
        ..AlternateLimits::default()
    };
    let objects = a
        .objects_with_alternates(PackLimits::default(), limits)
        .unwrap();
    assert_eq!(
        objects
            .read(id, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        b"byte path"
    );
    assert_eq!(
        git(a.git_dir(), &["cat-file", "blob", &id.to_string()], b""),
        b"byte path"
    );
}

#[cfg(unix)]
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn follows_symlink_alias_once(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let a = init(&root.path().join("a"), format);
    let b = init(&root.path().join("borrowed space"), format);
    std::os::unix::fs::symlink(b.object_dir(), root.path().join("alias")).unwrap();
    alternate(&a, b"\"../../borrowed space/objects\"\n../../alias\n");
    let id = b.loose_objects().write_blob(b"symlink").unwrap();
    let limits = AlternateLimits {
        max_stores: 2,
        ..AlternateLimits::default()
    };
    let objects = a
        .objects_with_alternates(PackLimits::default(), limits)
        .unwrap();
    assert_eq!(
        objects
            .read(id, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        b"symlink"
    );
    assert_eq!(
        git(a.git_dir(), &["cat-file", "blob", &id.to_string()], b""),
        b"symlink"
    );
}

#[cfg(unix)]
#[test]
fn denied_metadata_retains_permission_cause() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), ObjectFormat::Sha1);
    alternate(&a, b".\n");
    let path = a.object_dir().join("info/alternates");
    fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
    let result = a.objects(PackLimits::default());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        matches!(result, Err(ObjectReadError::Path { source, .. }) if source.kind() == std::io::ErrorKind::PermissionDenied)
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn depth_first_precedence_is_stable(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let a = init(&root.path().join("a"), format);
    let b = init(&root.path().join("b"), format);
    let c = init(&root.path().join("c"), format);
    let d = init(&root.path().join("d"), format);
    alternate(&a, b"../../b/objects\n../../c/objects\n");
    alternate(&b, b"../../d/objects\n");
    let id = d.loose_objects().write_blob(b"depth first").unwrap();
    c.loose_objects().write_blob(b"depth first").unwrap();
    let hex = id.to_string();
    fs::write(c.object_dir().join(&hex[..2]).join(&hex[2..]), b"corrupt").unwrap();
    assert_eq!(read(&a, id).unwrap(), b"depth first");
    alternate(&a, b"../../c/objects\n../../b/objects\n");
    let objects = a.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        objects.read(id, ReadLimits::default()),
        Err(ObjectReadError::Loose(_))
    ));
}

#[rstest]
#[case::precious("preciousObjects", "invalid")]
#[case::partial("partialClone", "")]
fn rejects_invalid_known_extension_values(#[case] name: &str, #[case] value: &str) {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), ObjectFormat::Sha1);
    let suffix = if value.is_empty() {
        String::new()
    } else {
        format!("={value}")
    };
    let config =
        format!("[core]\nbare=true\nrepositoryformatversion=1\n[extensions]\n{name}{suffix}\n");
    fs::write(a.git_dir().join("config"), config).unwrap();
    assert!(matches!(
        Repository::open(root.path()),
        Err(girt::OpenError::Malformed { .. })
    ));
}

#[test]
fn bounded_shared_graph_workload() {
    let root = tempfile::tempdir().unwrap();
    let a = init(root.path(), ObjectFormat::Sha1);
    let mut records = Vec::new();
    for number in 0..512 {
        let path = root.path().join(format!("store-{number}/info"));
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("alternates"), b"../objects\n").unwrap();
        records.extend_from_slice(format!("../store-{number}\n").as_bytes());
    }
    alternate(&a, &records);
    let started = std::time::Instant::now();
    let reader = a.objects(PackLimits::default()).unwrap();
    let elapsed = started.elapsed();
    assert!(
        reader
            .read(
                ObjectId::for_blob(ObjectFormat::Sha1, b"missing"),
                ReadLimits::default()
            )
            .unwrap()
            .is_none()
    );
    println!(
        "513 canonical stores, 1024 edges, {} metadata bytes; open {elapsed:?}",
        records.len() + 512 * b"../objects\n".len()
    );
    let limits = AlternateLimits {
        max_stores: 512,
        ..AlternateLimits::default()
    };
    assert!(matches!(
        a.objects_with_alternates(PackLimits::default(), limits),
        Err(ObjectReadError::Limit("alternate store count"))
    ));
}
