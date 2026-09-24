//! Disposable Git upload-pack servers; no network, real remotes, or caller checkout mutation.
#[path = "support/forward_delta.rs"]
mod forward_delta;
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
    let installed = received
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
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
    let installed = received
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
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
    received
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
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
    let installed = first
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let repeated = fetch(fixture.root.path());
    assert_eq!(
        repeated
            .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
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
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
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
    received
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
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
        received
            .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
            .unwrap();
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
        let first = scope.spawn(|| {
            received
                .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
                .unwrap()
        });
        let second = received
            .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
            .unwrap();
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
        received.install(&repo, girt::PackLimits::default(), &AtomicBool::new(false)),
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
        received.install(&repo, girt::PackLimits::default(), &AtomicBool::new(true)),
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
    let installed = received
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let path = repo
        .object_dir()
        .join(format!("pack/pack-{}.pack", installed.checksum.unwrap()));
    fs::write(&path, b"existing damage").unwrap();
    assert!(matches!(
        received.install(&repo, girt::PackLimits::default(), &AtomicBool::new(false)),
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
        .install(&other, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap()
        .checksum
        .unwrap();
    let index = repo.object_dir().join(format!("pack/pack-{checksum}.idx"));
    fs::write(&index, b"conflicting index").unwrap();
    assert!(matches!(
        received.install(&repo, girt::PackLimits::default(), &AtomicBool::new(false)),
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
    received
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    verify_contents(&fixture, &repo);
    assert_eq!(
        fs::read_dir(repo.object_dir().join("pack"))
            .unwrap()
            .count(),
        2
    );
}

fn known(repo: &Repository, roots: &[ObjectId]) -> girt::fetch::KnownHistory {
    girt::fetch::KnownHistory::new(
        &repo.objects(PackLimits::default()).unwrap(),
        roots,
        FetchLimits::default(),
        &AtomicBool::new(false),
    )
    .unwrap()
}
fn negotiated(path: &Path, known: &girt::fetch::KnownHistory) -> ReceivedFetch {
    girt::fetch::receive_local_with_known(
        path,
        select_all,
        known,
        FetchLimits::default(),
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
    )
    .unwrap()
}
fn child(fixture: &Fixture, parents: &[ObjectId], message: &[u8]) -> ObjectId {
    let tree = git(fixture.root.path(), &["rev-parse", "main^{tree}"], b"");
    let mut args = vec![
        "commit-tree".to_owned(),
        String::from_utf8(tree).unwrap().trim().to_owned(),
    ];
    for parent in parents {
        args.extend(["-p".to_owned(), parent.to_string()]);
    }
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    String::from_utf8(git(fixture.root.path(), &args, message))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}
fn set_main(fixture: &Fixture, id: ObjectId) {
    git(
        fixture.root.path(),
        &["update-ref", "refs/heads/main", &id.to_string()],
        b"",
    );
}
fn main_id(fixture: &Fixture) -> ObjectId {
    String::from_utf8(git(fixture.root.path(), &["rev-parse", "main"], b""))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn negotiated_initial_noop_and_incremental_preserve_connectivity() {
    let fixture = Fixture::new(true, 16);
    let (_root, repo) = destination();
    let initial = negotiated(fixture.root.path(), &girt::fetch::KnownHistory::default());
    initial
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let known = known(&repo, initial.wants());
    let noop = negotiated(fixture.root.path(), &known);
    assert_eq!(noop.object_count(), 0);
    assert_eq!(noop.pack_bytes(), 0);
    assert_eq!(noop.wants(), initial.wants());
    noop.install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let next = child(&fixture, &[main_id(&fixture)], b"incremental\n");
    set_main(&fixture, next);
    let full = fetch(fixture.root.path());
    let incremental = negotiated(fixture.root.path(), &known);
    assert_eq!(incremental.object_count(), 1);
    assert!(incremental.pack_bytes() < full.pack_bytes());
    incremental
        .install(&repo, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    verify_contents(&fixture, &repo);
    git(
        repo.git_dir(),
        &["fsck", "--strict", &next.to_string()],
        b"",
    );
    eprintln!(
        "fetch comparison initial={}/{} noop={}/{} incremental={}/{} full={}/{}",
        initial.object_count(),
        initial.pack_bytes(),
        noop.object_count(),
        noop.pack_bytes(),
        incremental.object_count(),
        incremental.pack_bytes(),
        full.object_count(),
        full.pack_bytes()
    );
}

#[rstest]
#[case::divergent(false)]
#[case::merge(true)]
fn negotiated_shared_history_and_merge(#[case] merge: bool) {
    let fixture = Fixture::new(true, 4);
    let old = main_id(&fixture);
    let left = child(&fixture, &[old], b"left\n");
    let right = child(&fixture, &[old], b"right\n");
    let parents = merge_parents(merge, left, right);
    let next = child(&fixture, &parents, b"next\n");
    // The left tip is local knowledge; only its common ancestor is shared in the divergent case.
    let known = known(&fixture.repo, &[left]);
    set_main(&fixture, next);
    let full = fetch(fixture.root.path());
    let received = negotiated(fixture.root.path(), &known);
    assert!(received.object_count() < full.object_count());
    assert!(received.pack_bytes() < full.pack_bytes());
    received
        .install(
            &fixture.repo,
            girt::PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    git(
        fixture.repo.git_dir(),
        &["fsck", "--strict", &next.to_string()],
        b"",
    );
}

#[test]
fn disconnected_haves_fall_back_to_complete_transfer() {
    let fixture = Fixture::new(true, 4);
    let (_root, local) = destination();
    let loose = local.loose_objects().unwrap();
    let tree = loose.write_tree(&girt::Tree::new(vec![]).unwrap()).unwrap();
    let template = girt::Commit::parse(&fixture.records[fixture.records.len() - 2].2).unwrap();
    let mut fields = template.fields().clone();
    fields.tree = tree;
    fields.message = b"Disconnected local history".to_vec();
    let commit = loose
        .write_commit(&girt::Commit::new(fields).unwrap())
        .unwrap();
    let known = known(&local, &[commit]);
    let full = fetch(fixture.root.path());
    let received = negotiated(fixture.root.path(), &known);
    assert_eq!(received.object_count(), full.object_count());
    received
        .install(&local, girt::PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    verify_contents(&fixture, &local);
}

#[rstest]
#[case::missing(false)]
#[case::corrupt(true)]
fn installation_rechecks_known_objects_before_publication(#[case] corrupt: bool) {
    let fixture = Fixture::new(true, 4);
    let (_root, local) = destination();
    let loose = local.loose_objects().unwrap();
    copy_loose(&fixture, &loose);
    let blob = fixture.ordinary;
    let known = known(&local, &[main_id(&fixture)]);
    let next = child(
        &fixture,
        &[main_id(&fixture)],
        b"incremental missing local
",
    );
    set_main(&fixture, next);
    let received = negotiated(fixture.root.path(), &known);
    let id = blob.to_string();
    let path = local.object_dir().join(&id[..2]).join(&id[2..]);
    damage(&path, corrupt);
    assert!(
        received
            .install(&local, girt::PackLimits::default(), &AtomicBool::new(false))
            .is_err()
    );
    assert_eq!(
        fs::read_dir(local.object_dir().join("pack"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        local
            .references()
            .unwrap()
            .resolve(&RefName::new("HEAD").unwrap(), 8)
            .unwrap()
            .id,
        None
    );
}

fn merge_parents(merge: bool, left: ObjectId, right: ObjectId) -> Vec<ObjectId> {
    if merge {
        vec![left, right]
    } else {
        vec![right]
    }
}
fn damage(path: &Path, corrupt: bool) {
    if corrupt {
        fs::write(path, b"corrupt").unwrap();
    } else {
        fs::remove_file(path).unwrap();
    }
}
fn copy_loose(fixture: &Fixture, loose: &girt::LooseObjects) {
    for (_, kind, bytes) in &fixture.records {
        match kind {
            girt::ObjectKind::Blob => loose.write_blob(bytes).unwrap(),
            girt::ObjectKind::Tree => loose
                .write_tree(&girt::Tree::parse(bytes).unwrap())
                .unwrap(),
            girt::ObjectKind::Commit => loose
                .write_commit(&girt::Commit::parse(bytes).unwrap())
                .unwrap(),
            girt::ObjectKind::Tag => loose.write_tag(&girt::Tag::parse(bytes).unwrap()).unwrap(),
        };
    }
}

#[test]
fn zero_have_budget_uses_full_transfer_without_losing_known_wants() {
    let fixture = Fixture::new(true, 4);
    let tag = fixture.records.last().unwrap().0;
    let known = known(&fixture.repo, &[tag]);
    let next = child(&fixture, &[main_id(&fixture)], b"no have budget\n");
    set_main(&fixture, next);
    let received = girt::fetch::receive_local_with_known(
        fixture.root.path(),
        select_all,
        &known,
        FetchLimits {
            max_haves: 0,
            ..FetchLimits::default()
        },
        girt::transport::TransportControl::new(&AtomicBool::new(false)),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    // Complete branch history plus its new commit; the already-known tag is still selected.
    assert_eq!(received.object_count(), fixture.records.len());
    assert!(received.wants().contains(&tag));
    received
        .install(
            &fixture.repo,
            girt::PackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
}

#[test]
fn installation_honors_destination_snapshot_limits_and_allows_retry() {
    let fixture = Fixture::new(true, 4);
    let (_root, repo) = destination();
    let initial = fetch(fixture.root.path());
    initial
        .install(&repo, PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let known = known(&repo, initial.wants());
    let received = negotiated(fixture.root.path(), &known);
    let too_small = PackLimits {
        max_packs: 0,
        ..PackLimits::default()
    };
    assert!(
        received
            .install(&repo, too_small, &AtomicBool::new(false))
            .is_err()
    );
    received
        .install(&repo, PackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    verify_contents(&fixture, &repo);
}

#[test]
fn reverse_forward_delta_chain_is_git_compatible_and_work_bounded() {
    let (wire, tip, pack) = forward_delta::response(1, 64);
    let (_root, repo) = destination();
    git(
        repo.git_dir(),
        &["index-pack", "--strict", "--stdin"],
        &pack,
    );
    assert_eq!(
        git(repo.git_dir(), &["cat-file", "blob", &tip.to_string()], b""),
        64u64.to_be_bytes()
    );
    // Sixty-five passes visit all sixty-five entries, including already resolved entries.
    let limits = FetchLimits {
        max_resolution_steps: 65 * 65,
        ..FetchLimits::default()
    };
    let receive = |limits| {
        girt::fetch::receive(
            &mut wire.as_slice(),
            &mut std::io::sink(),
            |_| vec![tip],
            limits,
            &AtomicBool::new(false),
            |_| ControlFlow::Continue(()),
        )
    };
    assert_eq!(receive(limits).unwrap().object_count(), 65);
    assert!(matches!(
        receive(FetchLimits {
            max_resolution_steps: 65 * 65 - 1,
            ..limits
        }),
        Err(FetchError::Limit("delta resolution steps"))
    ));
}
