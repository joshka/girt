//! Pack and object enumeration compared with independent Git executable observations.
#[path = "support/pack_git.rs"]
#[allow(dead_code, reason = "shared helpers; this suite uses only `git`")]
mod pack_git;

use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use girt::{ListLimits, ObjectFormat, ObjectId, ObjectReadError, PackLimits, Repository};
use pack_git::git;
use rstest::rstest;

/// A bare repository initialized by Git.
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

/// Writes loose blobs with Git and returns their identities.
fn blobs(repo: &Repository, prefix: &str, count: usize) -> Vec<ObjectId> {
    (0..count)
        .map(|n| {
            let id = git(
                repo.git_dir(),
                &["hash-object", "-w", "--stdin"],
                format!("{prefix} {n}\n").as_bytes(),
            );
            String::from_utf8(id).unwrap().trim().parse().unwrap()
        })
        .collect()
}

/// Packs every loose blob named on stdin into one new pack and removes the loose copies.
fn pack(repo: &Repository, ids: &[ObjectId]) -> String {
    let input: String = ids.iter().map(|id| format!("{id}\n")).collect();
    let base = repo.object_dir().join("pack/pack");
    let name = git(
        repo.git_dir(),
        &["pack-objects", base.to_str().unwrap()],
        input.as_bytes(),
    );
    git(repo.git_dir(), &["prune-packed"], b"");
    String::from_utf8(name).unwrap().trim().to_owned()
}

/// `git cat-file --batch-all-objects` identities, which Git prints sorted and unique.
fn git_all(repo: &Repository) -> Vec<ObjectId> {
    let output = git(
        repo.git_dir(),
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
        b"",
    );
    String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| line.parse().unwrap())
        .collect()
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn lists_packs_loose_and_alternates_like_git(#[case] format: ObjectFormat) {
    let root = tempfile::tempdir().unwrap();
    let borrowed = init(&root.path().join("borrowed"), format);
    let repo = init(&root.path().join("repo"), format);
    fs::write(
        repo.object_dir().join("info/alternates"),
        b"../../borrowed/objects\n",
    )
    .unwrap();
    let packed = blobs(&repo, "packed", 5);
    let name = pack(&repo, &packed);
    fs::write(
        repo.object_dir().join(format!("pack/pack-{name}.keep")),
        b"",
    )
    .unwrap();
    let loose = blobs(&repo, "loose", 3);
    // Duplicated in the alternate: listed once overall.
    let shared = blobs(&borrowed, "packed", 1);
    let borrowed_loose = blobs(&borrowed, "borrowed", 2);

    let objects = repo.objects(PackLimits::default()).unwrap();
    assert_eq!(
        objects.object_ids(ListLimits::default()).unwrap(),
        git_all(&repo)
    );

    let stores: Vec<_> = objects.stores().collect();
    assert_eq!(stores.len(), 2);
    assert!(!stores[0].is_alternate());
    assert!(stores[1].is_alternate());

    let packs: Vec<_> = objects.packs().collect();
    assert_eq!(packs.len(), 1);
    let pack = packs[0];
    assert_eq!(pack.checksum().to_string(), name);
    assert_eq!(pack.object_count(), 5);
    assert!(pack.is_kept());
    assert!(!pack.is_promisor());
    assert_eq!(pack.pack_bytes(), fs::metadata(pack.path()).unwrap().len());
    assert_eq!(
        pack.index_bytes(),
        fs::metadata(pack.index_path()).unwrap().len()
    );
    let mut sorted = packed.clone();
    sorted.sort();
    assert_eq!(pack.ids().collect::<Vec<_>>(), sorted);
    assert_eq!(pack.ids().next_back(), sorted.last().copied());

    let mut primary: Vec<_> = stores[0]
        .loose_objects()
        .map(|object| object.unwrap().id())
        .collect();
    primary.sort();
    let mut expected = loose.clone();
    expected.sort();
    assert_eq!(primary, expected);

    let mut alternate: Vec<_> = stores[1]
        .loose_objects()
        .map(|object| object.unwrap().id())
        .collect();
    alternate.sort();
    let mut expected = [shared, borrowed_loose].concat();
    expected.sort();
    assert_eq!(alternate, expected);
}

/// Files that cannot name an object in this format are skipped, as Git skips them.
#[test]
fn loose_scan_skips_non_object_names() {
    let root = tempfile::tempdir().unwrap();
    let repo = init(root.path(), ObjectFormat::Sha1);
    let ids = blobs(&repo, "kept", 1);
    let dir = repo.object_dir();
    fs::create_dir_all(dir.join("ab")).unwrap();
    fs::write(dir.join("ab/tmp_obj_123"), b"").unwrap();
    fs::write(dir.join("ab/0123"), b"").unwrap();
    fs::create_dir_all(dir.join("zz")).unwrap();
    fs::write(dir.join("zz/00000000000000000000000000000000000000"), b"").unwrap();
    fs::write(dir.join("cd"), b"not a directory").unwrap();
    fs::create_dir_all(dir.join("ef/0000000000000000000000000000000000000a")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        "missing",
        dir.join("ef/0000000000000000000000000000000000000b"),
    )
    .unwrap();
    let objects = repo.objects(PackLimits::default()).unwrap();
    let store = objects.stores().next().unwrap();
    let found: Vec<_> = store.loose_objects().map(Result::unwrap).collect();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id(), ids[0]);
    assert!(found[0].path().is_file());
    assert_eq!(git_all(&repo), ids);
}

#[test]
fn loose_scan_stops_at_entry_limit_and_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let repo = init(root.path(), ObjectFormat::Sha1);
    blobs(&repo, "limited", 4);
    let objects = repo.objects(PackLimits::default()).unwrap();
    let store = objects.stores().next().unwrap();
    let never = AtomicBool::new(false);
    let limited: Vec<_> = store
        .loose_objects_controlled(ListLimits::default().with_max_entries(1), &never)
        .collect();
    assert!(matches!(
        limited.last(),
        Some(Err(ObjectReadError::Limit(_)))
    ));
    assert!(matches!(
        objects.object_ids(ListLimits::default().with_max_entries(3)),
        Err(ObjectReadError::Limit(_))
    ));
    let cancelled = AtomicBool::new(true);
    let mut scan = store.loose_objects_controlled(ListLimits::default(), &cancelled);
    assert!(matches!(scan.next(), Some(Err(ObjectReadError::Cancelled))));
    assert!(scan.next().is_none());
}

#[test]
fn empty_store_lists_nothing() {
    let root = tempfile::tempdir().unwrap();
    let repo = init(root.path(), ObjectFormat::Sha1);
    let objects = repo.objects(PackLimits::default()).unwrap();
    assert!(
        objects
            .object_ids(ListLimits::default())
            .unwrap()
            .is_empty()
    );
    assert_eq!(objects.packs().count(), 0);
}
