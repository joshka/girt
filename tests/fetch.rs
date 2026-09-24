//! Disposable Git upload-pack servers; no network, real remotes, or caller checkout mutation.
#[path = "support/pack_git.rs"]
mod pack_git;

use std::fs;
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use girt::fetch::{Advertisement, FetchError, FetchLimits, ReceivedFetch, receive_local};
use girt::refs::{Expected, RefName, Target};
use girt::{ObjectId, PackLimits, ReadLimits, Repository};
use pack_git::{Fixture, git};
use rstest::rstest;

fn destination() -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--object-format=sha1",
            "--template=",
            "--initial-branch=main",
            ".",
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    (root, repo)
}
fn select_all(advertisement: &Advertisement) -> Vec<ObjectId> {
    advertisement
        .refs
        .iter()
        .filter(|r| !r.peeled)
        .map(|r| r.id)
        .collect()
}
fn fetch(path: &Path) -> ReceivedFetch {
    let result = receive_local(
        path,
        select_all,
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    result.unwrap()
}
fn verify_contents(fixture: &Fixture, repo: &Repository) {
    let objects = repo.objects(PackLimits::default()).unwrap();
    for (id, kind, expected) in &fixture.records {
        let object = objects.read(*id, ReadLimits::default()).unwrap().unwrap();
        assert_eq!(object.kind(), *kind);
        assert_eq!(object.data(), expected);
        assert_eq!(
            git(
                repo.git_dir(),
                &["cat-file", kind.as_str(), &id.to_string()],
                b""
            ),
            *expected
        );
    }
}

#[rstest]
#[case::ofs_source(true)]
#[case::ref_source(false)]
fn imports_server_deltas_and_all_object_kinds(#[case] ofs: bool) {
    let fixture = Fixture::new(ofs, 16);
    let (root, repo) = destination();
    assert!(fixture.index_path.exists());
    let received = fetch(fixture.root.path());
    let installed = received.install(&repo, &AtomicBool::new(false)).unwrap();
    let index = repo
        .object_dir()
        .join(format!("pack/pack-{}.idx", installed.checksum.unwrap()));
    let report = git(
        root.path(),
        &["verify-pack", "-v", index.to_str().unwrap()],
        b"",
    );
    assert!(
        std::str::from_utf8(&report)
            .unwrap()
            .lines()
            .any(|line| line.split_whitespace().count() == 7)
    );
    assert_eq!(installed.objects, fixture.records.len());
    assert!(received.advertisement().refs.iter().any(|r| r.peeled));
    verify_contents(&fixture, &repo);
    assert!(
        repo.objects(PackLimits::default())
            .unwrap()
            .read(fixture.ordinary, ReadLimits::default())
            .unwrap()
            .is_some()
    );
    assert_eq!(
        repo.references()
            .unwrap()
            .resolve(&RefName::new("HEAD").unwrap(), 8)
            .unwrap()
            .id,
        None
    );
    assert!(!root.path().join("FETCH_HEAD").exists());
    assert!(!root.path().join("logs").exists());
    // Independent Git indexing must agree byte for byte with girt's generated index.
    let expected_index = fs::read(&index).unwrap();
    fs::remove_file(&index).unwrap();
    git(
        root.path(),
        &[
            "index-pack",
            "--index-version=2",
            index.with_extension("pack").to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(fs::read(&index).unwrap(), expected_index);
}

#[test]
fn empty_repository_transfers_nothing() {
    let (source, _) = destination();
    let (_root, repo) = destination();
    let received = fetch(source.path());
    let installed = received.install(&repo, &AtomicBool::new(false)).unwrap();
    assert_eq!(installed.objects, 0);
    assert!(installed.checksum.is_none());
}

#[test]
fn empty_selection_does_not_request_a_pack() {
    let fixture = Fixture::new(true, 4);
    let received = receive_local(
        fixture.root.path(),
        |_| vec![],
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    let received = received.unwrap();
    assert_eq!(received.object_count(), 0);
    assert_eq!(received.pack_bytes(), 0);
}

#[rstest]
#[case::branch("refs/heads/main", false)]
#[case::tag("refs/tags/packed", true)]
fn selects_explicit_branch_or_tag(#[case] name: &str, #[case] has_tag: bool) {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let received = receive_local(
        fixture.root.path(),
        |advertisement| {
            advertisement
                .refs
                .iter()
                .filter(|r| r.name.as_bytes() == name.as_bytes() && !r.peeled)
                .map(|r| r.id)
                .collect()
        },
        FetchLimits::default(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    let received = received.unwrap();
    received.install(&repo, &AtomicBool::new(false)).unwrap();
    let tag = fixture.records.last().unwrap().0;
    assert_eq!(
        repo.objects(PackLimits::default())
            .unwrap()
            .read(tag, ReadLimits::default())
            .unwrap()
            .is_some(),
        has_tag
    );
}

#[test]
fn repeated_and_incremental_fetches_allow_conditional_updates() {
    let fixture = Fixture::new(true, 4);
    let (root, repo) = destination();
    let first = fetch(fixture.root.path());
    let installed = first.install(&repo, &AtomicBool::new(false)).unwrap();
    let repeated = fetch(fixture.root.path());
    assert_eq!(
        repeated
            .install(&repo, &AtomicBool::new(false))
            .unwrap()
            .checksum,
        installed.checksum
    );
    let branch = RefName::new("refs/heads/main").unwrap();
    let old = fixture
        .repo
        .references()
        .unwrap()
        .resolve(&branch, 8)
        .unwrap()
        .id
        .unwrap();
    let refs = repo.references().unwrap();
    refs.update_without_reflog(&branch, Target::Direct(old), Expected::Absent)
        .unwrap();
    let tree = git(fixture.root.path(), &["rev-parse", "main^{tree}"], b"");
    let next = git(
        fixture.root.path(),
        &[
            "commit-tree",
            std::str::from_utf8(&tree).unwrap().trim(),
            "-p",
            &old.to_string(),
        ],
        b"Next snapshot\n",
    );
    let next: ObjectId = std::str::from_utf8(&next).unwrap().trim().parse().unwrap();
    git(
        fixture.root.path(),
        &["update-ref", "refs/heads/main", &next.to_string()],
        b"",
    );
    fetch(fixture.root.path())
        .install(&repo, &AtomicBool::new(false))
        .unwrap();
    let updated = refs.update_without_reflog(
        &branch,
        Target::Direct(next),
        Expected::Value(Target::Direct(old)),
    );
    updated.unwrap();
    assert_eq!(refs.resolve(&branch, 8).unwrap().id, Some(next));
    git(root.path(), &["fsck", "--strict", "--no-reflogs"], b"");
    assert!(!root.path().join("logs").exists());
    verify_contents(&fixture, &repo);
}

#[test]
fn concurrent_ref_change_fails_only_the_callers_conditional_update() {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let received = fetch(fixture.root.path());
    let branch = RefName::new("refs/heads/main").unwrap();
    let other = repo
        .loose_objects()
        .unwrap()
        .write_blob(b"concurrent writer")
        .unwrap();
    let refs = repo.references().unwrap();
    refs.update_without_reflog(&branch, Target::Direct(other), Expected::Absent)
        .unwrap();
    received.install(&repo, &AtomicBool::new(false)).unwrap();
    assert!(
        refs.update_without_reflog(
            &branch,
            Target::Direct(received.wants()[0]),
            Expected::Absent
        )
        .is_err()
    );
    assert_eq!(refs.read(&branch).unwrap(), Some(Target::Direct(other)));
    verify_contents(&fixture, &repo);
}

#[test]
fn existing_snapshots_and_concurrent_openers_see_complete_pairs() {
    let fixture = Fixture::new(true, 16);
    let (_root, repo) = destination();
    let old = repo.objects(PackLimits::default()).unwrap();
    let received = fetch(fixture.root.path());
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            while !done.load(Ordering::Acquire) {
                let objects = repo.objects(PackLimits::default()).unwrap();
                if let Some(object) = objects.read(fixture.delta, ReadLimits::default()).unwrap() {
                    assert_eq!(object.id(), fixture.delta);
                }
            }
        });
        received.install(&repo, &AtomicBool::new(false)).unwrap();
        done.store(true, Ordering::Release);
        reader.join().unwrap();
    });
    assert_eq!(
        old.read(fixture.delta, ReadLimits::default()).unwrap(),
        None
    );
    verify_contents(&fixture, &repo);
}

#[test]
fn concurrent_publishers_reuse_identical_artifacts() {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let received = fetch(fixture.root.path());
    let results = std::thread::scope(|scope| {
        let first = scope.spawn(|| received.install(&repo, &AtomicBool::new(false)).unwrap());
        let second = received.install(&repo, &AtomicBool::new(false)).unwrap();
        (first.join().unwrap(), second)
    });
    assert_eq!(results.0.checksum, results.1.checksum);
    verify_contents(&fixture, &repo);
}

#[test]
fn installation_failure_preserves_existing_objects() {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let loose = repo.loose_objects().unwrap();
    let id = loose.write_blob(b"keep me").unwrap();
    fs::remove_dir(repo.object_dir().join("pack")).unwrap();
    fs::write(repo.object_dir().join("pack"), b"blocking file").unwrap();
    let received = fetch(fixture.root.path());
    assert!(matches!(
        received.install(&repo, &AtomicBool::new(false)),
        Err(FetchError::Io(_))
    ));
    assert_eq!(loose.read_blob(id, 100).unwrap(), b"keep me");
    assert_eq!(
        fs::read(repo.object_dir().join("pack")).unwrap(),
        b"blocking file"
    );
}

#[test]
fn cancelled_install_has_no_side_effects() {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let received = fetch(fixture.root.path());
    assert!(matches!(
        received.install(&repo, &AtomicBool::new(true)),
        Err(FetchError::Cancelled)
    ));
    assert_eq!(
        fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn conflicting_artifacts_are_never_overwritten() {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let received = fetch(fixture.root.path());
    let installed = received.install(&repo, &AtomicBool::new(false)).unwrap();
    let path = repo
        .object_dir()
        .join(format!("pack/pack-{}.pack", installed.checksum.unwrap()));
    fs::write(&path, b"existing damage").unwrap();
    assert!(matches!(
        received.install(&repo, &AtomicBool::new(false)),
        Err(FetchError::Existing(_))
    ));
    assert_eq!(fs::read(path).unwrap(), b"existing damage");
}

#[test]
fn retries_after_index_publication_failure() {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let received = fetch(fixture.root.path());
    // Obtain the deterministic basename in a separate disposable destination.
    let (_other_root, other) = destination();
    let checksum = received
        .install(&other, &AtomicBool::new(false))
        .unwrap()
        .checksum
        .unwrap();
    let index = repo.object_dir().join(format!("pack/pack-{checksum}.idx"));
    fs::write(&index, b"conflicting index").unwrap();
    assert!(matches!(
        received.install(&repo, &AtomicBool::new(false)),
        Err(FetchError::Existing(_))
    ));
    assert_eq!(fs::read(&index).unwrap(), b"conflicting index");
    fs::remove_file(&index).unwrap();
    assert_eq!(
        git(
            repo.git_dir(),
            &["cat-file", "--batch-check"],
            format!("{}\n", fixture.delta).as_bytes()
        ),
        format!("{} missing\n", fixture.delta).into_bytes()
    );
    assert_eq!(
        repo.objects(PackLimits::default())
            .unwrap()
            .read(fixture.delta, ReadLimits::default())
            .unwrap(),
        None
    );
    received.install(&repo, &AtomicBool::new(false)).unwrap();
    verify_contents(&fixture, &repo);
    assert_eq!(
        fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        2
    );
}
