//! Independently generated Git packs exercise reader generations and interrupted publication.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::fs;
use std::sync::atomic::AtomicBool;

use girt::{AlternateLimits, ObjectFormat, ObjectReadError, PackLimits, ReadLimits};
use pack_git::{Fixture, git};
use rstest::rstest;

fn refresh(objects: &mut girt::Objects) -> Result<(), ObjectReadError> {
    objects.refresh(PackLimits::default(), AlternateLimits::default())
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn index_publication_after_miss_requires_explicit_refresh(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let pending = fixture.index_path.with_extension("pending");
    fs::rename(&fixture.index_path, &pending).unwrap();
    let mut objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let retained = objects.clone();
    assert_eq!(
        objects.read(fixture.delta, ReadLimits::default()).unwrap(),
        None
    );
    // Pack exists already; only the fully written index is atomically published.
    fs::rename(pending, &fixture.index_path).unwrap();
    assert_eq!(
        objects.read(fixture.delta, ReadLimits::default()).unwrap(),
        None
    );
    refresh(&mut objects).unwrap();
    let value = objects
        .read(fixture.delta, ReadLimits::default())
        .unwrap()
        .unwrap();
    assert_eq!(
        value.data(),
        git(
            fixture.root.path(),
            &["cat-file", "blob", &fixture.delta.to_string()],
            b""
        )
    );
    assert_eq!(
        retained.read(fixture.delta, ReadLimits::default()).unwrap(),
        None
    );
    assert_eq!(fixture.records.len(), 7);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn failed_refresh_preserves_pinned_reader_and_recovers(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let retained = objects.clone();
    let pack = fixture.index_path.with_extension("pack");
    let pending = pack.with_extension("pending");
    fs::rename(&pack, &pending).unwrap();
    assert!(
        matches!(refresh(&mut objects), Err(ObjectReadError::Path { source, .. }) if source.kind() == std::io::ErrorKind::NotFound)
    );
    assert_eq!(
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
    fs::rename(pending, pack).unwrap();
    refresh(&mut objects).unwrap();
    assert_eq!(
        objects.read(fixture.delta, ReadLimits::default()).unwrap(),
        retained.read(fixture.delta, ReadLimits::default()).unwrap()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn corrupt_replacement_is_error_until_repaired(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let original = fixture.index_path.with_extension("old");
    fs::rename(&fixture.index_path, &original).unwrap();
    fs::write(&fixture.index_path, b"interrupted index").unwrap();
    assert!(matches!(
        refresh(&mut objects),
        Err(ObjectReadError::PackArtifacts { .. })
    ));
    assert_eq!(
        objects
            .read(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.ordinary
    );
    fs::remove_file(&fixture.index_path).unwrap();
    fs::rename(original, &fixture.index_path).unwrap();
    refresh(&mut objects).unwrap();
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
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn external_git_gc_retains_old_packs_and_refreshes_current_view(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let retained = objects.clone();
    git(fixture.root.path(), &["gc", "--prune=now"], b"");
    refresh(&mut objects).unwrap();
    assert_eq!(
        objects.read(fixture.delta, ReadLimits::default()).unwrap(),
        retained.read(fixture.delta, ReadLimits::default()).unwrap()
    );
    assert_eq!(
        objects
            .read(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        git(
            fixture.root.path(),
            &["cat-file", "blob", &fixture.ordinary.to_string()],
            b""
        )
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn changed_alternates_leave_retained_topology_until_refresh(#[case] format: ObjectFormat) {
    let primary = Fixture::new(format, true, 4);
    let alternate = Fixture::new(format, true, 8);
    let id = alternate.delta;
    let mut objects = primary.repo.objects(PackLimits::default()).unwrap();
    let retained = objects.clone();
    let metadata = primary.repo.object_dir().join("info/alternates");
    fs::create_dir_all(metadata.parent().unwrap()).unwrap();
    fs::write(
        &metadata,
        format!("{}\n", alternate.repo.object_dir().display()),
    )
    .unwrap();
    refresh(&mut objects).unwrap();
    assert!(objects.read(id, ReadLimits::default()).unwrap().is_some());
    // A fresh loose identity exists only in the newly borrowed directory.
    let loose = alternate
        .repo
        .loose_objects()
        .write_blob(b"alternate topology change")
        .unwrap();
    assert!(
        objects
            .read(loose, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert_eq!(retained.read(loose, ReadLimits::default()).unwrap(), None);
    let borrowed = objects.clone();
    fs::remove_file(metadata).unwrap();
    refresh(&mut objects).unwrap();
    assert_eq!(objects.read(loose, ReadLimits::default()).unwrap(), None);
    assert!(
        borrowed
            .read(loose, ReadLimits::default())
            .unwrap()
            .is_some()
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn cancellation_and_limits_preserve_view(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut objects = fixture.repo.objects(PackLimits::default()).unwrap();
    assert!(matches!(
        objects.refresh_controlled(
            PackLimits::default(),
            AlternateLimits::default(),
            &AtomicBool::new(true)
        ),
        Err(ObjectReadError::Cancelled)
    ));
    assert!(matches!(
        objects.refresh(
            PackLimits {
                max_open_files: 0,
                ..PackLimits::default()
            },
            AlternateLimits::default()
        ),
        Err(ObjectReadError::Limit("pack file handles"))
    ));
    assert_eq!(
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
    refresh(&mut objects).unwrap();
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn concurrent_loose_publication_is_live_without_refresh(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let id = girt::ObjectId::for_blob(format, b"concurrent loose publication");
    assert_eq!(objects.read(id, ReadLimits::default()).unwrap(), None);
    let first = fixture.repo.loose_objects();
    let second = first.clone();
    std::thread::scope(|scope| {
        let a = scope.spawn(|| first.write_blob(b"concurrent loose publication").unwrap());
        let b = scope.spawn(|| second.write_blob(b"concurrent loose publication").unwrap());
        assert_eq!(a.join().unwrap(), id);
        assert_eq!(b.join().unwrap(), id);
    });
    assert_eq!(
        objects
            .read(id, ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        b"concurrent loose publication"
    );
    git(fixture.root.path(), &["prune", "--expire=now"], b"");
    assert_eq!(objects.read(id, ReadLimits::default()).unwrap(), None);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn removing_pairs_only_affects_refreshed_generation(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let retained = objects.clone();
    fs::remove_file(&fixture.index_path).unwrap();
    fs::remove_file(fixture.index_path.with_extension("pack")).unwrap();
    refresh(&mut objects).unwrap();
    assert_eq!(
        objects.read(fixture.delta, ReadLimits::default()).unwrap(),
        None
    );
    assert_eq!(
        retained
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn malformed_alternate_refresh_preserves_prior_view(#[case] format: ObjectFormat) {
    let fixture = Fixture::new(format, true, 4);
    let mut objects = fixture.repo.objects(PackLimits::default()).unwrap();
    let metadata = fixture.repo.object_dir().join("info/alternates");
    fs::create_dir_all(metadata.parent().unwrap()).unwrap();
    fs::write(&metadata, b"\"unfinished").unwrap();
    assert!(matches!(
        refresh(&mut objects),
        Err(ObjectReadError::Alternate { .. })
    ));
    assert_eq!(
        objects
            .read(fixture.delta, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        fixture.delta
    );
    fs::remove_file(metadata).unwrap();
    refresh(&mut objects).unwrap();
}
