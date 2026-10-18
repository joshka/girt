//! Storage-access bounds for the retention walk, observed on a Git-generated, deltified pack.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use crate::pack::counters;
use crate::retention::RetentionPolicy;
use crate::{PackLimits, Repository};

pub(super) fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let mut child = Command::new("git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

/// History whose trees and blobs Git stores mostly as deltas, in a single pack.
fn deltified_history(root: &Path) -> usize {
    git(root, &["init", "-q", "--initial-branch=main", "."], b"");
    // Wide directories make each round's trees near copies of the last, so Git deltifies them.
    for round in 0..24 {
        for file in 0..60 {
            if round > 0 && file % 12 != round % 12 {
                continue;
            }
            let directory = root.join(format!("dir{}", file % 3));
            std::fs::create_dir_all(&directory).unwrap();
            let content: String = (0..40)
                .map(|line| format!("file {file} line {line} {}\n", (line == 0) as usize * round))
                .collect();
            std::fs::write(directory.join(format!("f{file}")), content).unwrap();
        }
        git(root, &["add", "-A"], b"");
        git(
            root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                &format!("r{round}"),
            ],
            b"",
        );
    }
    git(
        root,
        &[
            "-c",
            "pack.threads=1",
            "repack",
            "-a",
            "-d",
            "-f",
            "-q",
            "--window=50",
            "--depth=50",
        ],
        b"",
    );
    git(root, &["prune-packed"], b"");
    // Without reflogs the strong and weak root sets are equal, so the plan walks once.
    std::fs::remove_dir_all(root.join(".git/logs")).unwrap();
    let count = git(root, &["count-objects", "-v"], b"");
    let line = count
        .lines()
        .find(|line| line.starts_with("in-pack: "))
        .unwrap();
    line["in-pack: ".len()..].parse().unwrap()
}

/// The walk inflates each pack entry at most once (bases come from the delta base cache) and
/// reads a typical entry's header and stream with a single positioned read.
#[test]
fn retention_walk_reads_each_entry_once() {
    let dir = tempfile::tempdir().unwrap();
    let entries = deltified_history(dir.path());
    let listing = git(
        dir.path(),
        &["verify-pack", "-v", &{
            let pack = std::fs::read_dir(dir.path().join(".git/objects/pack"))
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
                .unwrap();
            pack.to_string_lossy().into_owned()
        }],
        b"",
    );
    let deltas = |kind: &str| {
        listing
            .lines()
            .map(|line| line.split_whitespace().collect::<Vec<_>>())
            .filter(|fields| fields.len() == 7 && fields[1] == kind)
            .count()
    };
    assert!(
        deltas("tree") > 24 && deltas("blob") > 24,
        "fixture must deltify trees and blobs:\n{listing}"
    );
    let repository = Repository::open(dir.path()).unwrap();
    // The counters are per thread, so the walk reads on this thread alone.
    let policy = RetentionPolicy {
        recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
        packs: PackLimits::trusted(),
        max_threads: 1,
        ..RetentionPolicy::trusted()
    };
    counters::reset();
    let plan = repository.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert_eq!(plan.reachable.len(), entries);
    let expected: std::collections::BTreeSet<crate::ObjectId> =
        git(dir.path(), &["rev-list", "--objects", "--all"], b"")
            .lines()
            .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
            .collect();
    assert_eq!(plan.reachable, expected);
    let repeated: Vec<_> = counters::INFLATIONS.with(|map| {
        map.borrow()
            .iter()
            .filter(|(_, count)| **count > 1)
            .map(|(offset, count)| (*offset, *count))
            .collect()
    });
    assert!(
        repeated.is_empty(),
        "entries inflated more than once: {repeated:?}"
    );
    // Operation-scoped windows amortize entry-header reads; streams and pack opening add a few.
    let reads = counters::OS_READS.with(|reads| reads.get());
    assert!(
        reads < entries / 4,
        "{reads} system reads for {entries} entries"
    );
}

/// Read-ahead workers share the delta base cache, so the walk reads bases in another order on
/// several threads. Per-read limits charge cached bases, so the outcome doesn't change.
#[test]
fn delta_depth_limits_give_the_same_outcome_on_any_thread_count() {
    let dir = tempfile::tempdir().unwrap();
    deltified_history(dir.path());
    let repository = Repository::open(dir.path()).unwrap();
    for depth in [0, 1, 2, 3, 64] {
        let plan = |max_threads| {
            let policy = RetentionPolicy {
                recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
                max_threads,
                read: crate::ReadLimits {
                    max_delta_depth: depth,
                    ..crate::ReadLimits::default()
                },
                ..RetentionPolicy::trusted()
            };
            repository.plan_retention(&policy, &AtomicBool::new(false))
        };
        let alone = plan(1);
        for _ in 0..4 {
            let parallel = plan(4);
            assert_eq!(
                format!("{:?}", alone.outcome),
                format!("{:?}", parallel.outcome),
                "max_delta_depth {depth}"
            );
            assert_eq!(alone.reachable, parallel.reachable);
        }
    }
}

/// Independently generated pack with a reachable blob and an old loose pruning candidate.
#[cfg(unix)]
pub(super) fn header_fixture() -> (
    tempfile::TempDir,
    Repository,
    RetentionPolicy,
    std::path::PathBuf,
) {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q", "--template=", "."], b"");
    let id = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"packed survivor",
    );
    git(root.path(), &["update-ref", "refs/tags/blob", &id], b"");
    git(root.path(), &["repack", "-adq"], b"");
    let orphan = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"loose orphan",
    );
    let orphan_path = root
        .path()
        .join(".git/objects")
        .join(&orphan[..2])
        .join(&orphan[2..]);
    std::fs::File::open(orphan_path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH)
        .unwrap();
    git(root.path(), &["fsck", "--full"], b"");
    let repo = Repository::open(root.path()).unwrap();
    let pack = std::fs::read_dir(repo.object_dir().join("pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: Some(std::time::UNIX_EPOCH + Duration::from_secs(1)),
        ..RetentionPolicy::trusted()
    };
    (root, repo, policy, pack)
}

#[cfg(unix)]
pub(super) fn change_pack(path: &Path, truncate: bool) {
    use std::io::{Seek, SeekFrom};
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let mut file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    if truncate {
        file.set_len(12).unwrap();
    } else {
        // Change a warmed entry header without changing the file length.
        let original = std::fs::read(path).unwrap()[12];
        file.seek(SeekFrom::Start(12)).unwrap();
        file.write_all(&[original ^ 1]).unwrap();
    }
    // Deterministic even on filesystems with coarse timestamp precision.
    file.set_modified(std::time::UNIX_EPOCH + Duration::from_secs(42))
        .unwrap();
}

#[cfg(unix)]
use crate::pack_git;

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1(crate::ObjectFormat::Sha1)]
#[case::sha256(crate::ObjectFormat::Sha256)]
fn damage_rescued_after_complete_plan_refuses_prune(#[case] format: crate::ObjectFormat) {
    let fixture = pack_git::Fixture::new(format, true, 4);
    let expected = fixture
        .records
        .iter()
        .find(|record| record.0 == fixture.ordinary)
        .unwrap();
    // Git skips writing already packed data; temporarily hide the index to generate a loose
    // rescue copy before taking the retention snapshot.
    let hidden = fixture.root.path().join("hidden.idx");
    std::fs::rename(&fixture.index_path, &hidden).unwrap();
    pack_git::git(
        fixture.root.path(),
        &["hash-object", "-w", "--stdin"],
        &expected.2,
    );
    std::fs::rename(hidden, &fixture.index_path).unwrap();
    let orphan = pack_git::git(
        fixture.root.path(),
        &["hash-object", "-w", "--stdin"],
        b"deletable orphan",
    );
    let mut index = std::fs::read(&fixture.index_path).unwrap();
    let width = format.digest_len();
    let count = fixture.records.len();
    let position = index[1032..1032 + count * width]
        .chunks_exact(width)
        .position(|name| name == fixture.ordinary.as_bytes())
        .unwrap();
    index[1032 + count * width + position * 4] ^= 1;
    let end = index.len() - width;
    let checksum = format.checksum(&index[..end]);
    index[end..].copy_from_slice(checksum.as_bytes());
    std::fs::remove_file(&fixture.index_path).unwrap();
    std::fs::write(&fixture.index_path, index).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
        ..Default::default()
    };
    let cancel = AtomicBool::new(false);
    let session = super::inputs::Session::new(&fixture.repo, &policy, &cancel).unwrap();
    assert!(session.plan.is_complete());
    let store = session.plan.store.as_ref().unwrap();
    assert_eq!(
        store
            .read_with(fixture.ordinary, crate::ReadLimits::default())
            .unwrap()
            .unwrap()
            .data(),
        expected.2
    );
    let error = fixture
        .repo
        .prune_planned(&policy, &cancel, &session)
        .unwrap_err();
    assert!(
        matches!(error.cause, super::PruneCause::PackChanged(ref source)
        if matches!(source.as_ref(), crate::ObjectReadError::Path { source, .. } if source.kind() == std::io::ErrorKind::InvalidData))
    );
    assert!(error.report.deleted.is_empty());
    assert_eq!(
        pack_git::git(
            fixture.root.path(),
            &[
                "cat-file",
                "blob",
                std::str::from_utf8(&orphan).unwrap().trim()
            ],
            b""
        ),
        b"deletable orphan"
    );
    assert!(
        store
            .read_with(fixture.delta, crate::ReadLimits::default())
            .is_err()
    );
}

/// One commit, referenced by `refs/heads/main`, whose root tree has `entries`.
///
/// Trees are written with girt because Git refuses entries whose mode contradicts the target's
/// kind; `entries` can name the commit itself through `None`.
fn kind_fixture(
    root: &Path,
    entries: &[(crate::EntryMode, &str, Option<crate::ObjectId>)],
) -> (Repository, crate::ObjectId) {
    use crate::{Commit, CommitFields, Signature, Tree, TreeEntry};
    git(root, &["init", "-q"], b"");
    let repository = Repository::open(root).unwrap();
    let loose = repository.loose_objects();
    let format = repository.object_format();
    let person = Signature {
        name: b"A".to_vec(),
        email: b"a@example.com".to_vec(),
        seconds: 1_700_000_000,
        offset_minutes: 0,
    };
    let commit_for = |tree| {
        Commit::new(CommitFields {
            tree,
            parents: vec![],
            author: person.clone(),
            committer: person.clone(),
            extra_headers: vec![],
            message: b"kinds\n".to_vec(),
        })
        .unwrap()
    };
    // A commit cannot name a tree that names the commit, so self-references use a parent commit
    // whose tree is empty; the tip's tree then names that parent.
    let parent = commit_for(
        loose
            .write_tree(&Tree::new(format, vec![]).unwrap())
            .unwrap(),
    );
    let parent_id = loose.write_commit(&parent).unwrap();
    let tree = Tree::new(
        format,
        entries
            .iter()
            .map(|&(mode, name, id)| TreeEntry {
                mode,
                name: name.as_bytes().to_vec(),
                id: id.unwrap_or(parent_id),
            })
            .collect(),
    )
    .unwrap();
    let mut tip = commit_for(loose.write_tree(&tree).unwrap());
    if entries.iter().any(|entry| entry.2.is_none()) {
        tip = Commit::new(CommitFields {
            tree: tree.id(),
            parents: vec![parent_id],
            author: person.clone(),
            committer: person.clone(),
            extra_headers: vec![],
            message: b"kinds\n".to_vec(),
        })
        .unwrap();
    }
    let tip = loose.write_commit(&tip).unwrap();
    git(
        root,
        &["update-ref", "refs/heads/main", &tip.to_string()],
        b"",
    );
    (repository, parent_id)
}

/// Plans on the calling thread alone and with read-ahead workers, which must agree.
fn plan_outcome(repository: &Repository) -> crate::retention::RetentionPlan {
    let plan = |max_threads| {
        let policy = RetentionPolicy {
            recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
            max_threads,
            ..RetentionPolicy::trusted()
        };
        repository.plan_retention(&policy, &AtomicBool::new(false))
    };
    let alone = plan(1);
    let parallel = plan(4);
    assert_eq!(
        format!("{:?}", alone.outcome),
        format!("{:?}", parallel.outcome)
    );
    assert_eq!(alone.reachable, parallel.reachable);
    assert_eq!(alone.required, parallel.required);
    parallel
}

/// Every edge checks its kind, including edges to an identity the walk already queued or read.
#[test]
fn retention_walk_checks_kinds_of_repeated_edges() {
    use crate::EntryMode::{Blob, Executable, Tree};
    use crate::retention::{RetentionError, RetentionOutcome};
    let blob = |root: &Path| -> crate::ObjectId {
        git(root, &["init", "-q"], b"");
        git(root, &["hash-object", "-w", "--stdin"], b"content\n")
            .parse()
            .unwrap()
    };

    // Repeated names of one blob, with either file mode, are one reachable object.
    let dir = tempfile::tempdir().unwrap();
    let id = blob(dir.path());
    let (repository, _) = kind_fixture(
        dir.path(),
        &[
            (Blob, "a", Some(id)),
            (Executable, "b", Some(id)),
            (Blob, "c", Some(id)),
        ],
    );
    let plan = plan_outcome(&repository);
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.reachable.contains(&id));
    assert_eq!(plan.reachable.len(), 3, "one commit, one tree and one blob");

    // A queued identity named again with another kind.
    let dir = tempfile::tempdir().unwrap();
    let id = blob(dir.path());
    let (repository, _) = kind_fixture(dir.path(), &[(Blob, "a", Some(id)), (Tree, "b", Some(id))]);
    assert!(matches!(
        plan_outcome(&repository).outcome,
        RetentionOutcome::Incomplete(RetentionError::ConflictingObjectKind(conflict)) if conflict == id
    ));

    // An edge asserting a tree whose target is read as a blob.
    let dir = tempfile::tempdir().unwrap();
    let id = blob(dir.path());
    let (repository, _) = kind_fixture(dir.path(), &[(Tree, "a", Some(id))]);
    assert!(matches!(
        plan_outcome(&repository).outcome,
        RetentionOutcome::Incomplete(RetentionError::WrongObjectKind(wrong)) if wrong == id
    ));

    // An edge naming, as a blob, a commit the walk has already read.
    let dir = tempfile::tempdir().unwrap();
    let (repository, parent) = kind_fixture(dir.path(), &[(Blob, "a", None)]);
    git(
        dir.path(),
        &["update-ref", "refs/heads/parent", &parent.to_string()],
        b"",
    );
    assert!(matches!(
        plan_outcome(&repository).outcome,
        RetentionOutcome::Incomplete(RetentionError::WrongObjectKind(wrong)) if wrong == parent
    ));
}

/// Read-ahead holds parsed edges until the walk reaches them, so a worker keeps no more than the
/// walk could accept before refusing that object.
#[test]
fn read_ahead_collects_no_edges_past_the_walk_limits() {
    use crate::retention::RetentionError;
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "--bare", "."], b"");
    let blob = git(dir.path(), &["hash-object", "-w", "--stdin"], b"x\n");
    let listing: String = (0..1000)
        .map(|n| format!("100644 blob {blob}\tf{n}\n"))
        .collect();
    let tree: crate::ObjectId = git(dir.path(), &["mktree"], listing.as_bytes())
        .parse()
        .unwrap();
    let repository = Repository::open(dir.path()).unwrap();
    let store = repository.objects_with(PackLimits::default()).unwrap();
    let cancel = AtomicBool::new(false);
    let visit = |policy: &RetentionPolicy| {
        super::visit(&repository, &store, policy, &cancel, tree).unwrap()
    };
    let all = visit(&RetentionPolicy::trusted());
    assert_eq!(all.edges.len(), 1000);
    let few = visit(&RetentionPolicy::trusted().with_max_edges(3));
    assert_eq!(
        few.edges.len(),
        4,
        "one past the limit, so the walk refuses"
    );
    let none = visit(&RetentionPolicy::trusted().with_max_bytes(10));
    assert!(none.edges.is_empty());
    for policy in [
        RetentionPolicy::trusted().with_max_edges(3),
        RetentionPolicy::trusted().with_max_bytes(10),
    ] {
        let plan = |max_threads| {
            repository
                .plan_retention(
                    &policy
                        .clone()
                        .with_heads(vec![tree])
                        .with_max_threads(max_threads),
                    &cancel,
                )
                .outcome
        };
        let alone = format!("{:?}", plan(1));
        assert!(
            alone.contains(&format!("{:?}", RetentionError::ReachableEdgeLimit))
                || alone.contains(&format!("{:?}", RetentionError::ReachableByteLimit)),
            "{alone}"
        );
        assert_eq!(alone, format!("{:?}", plan(4)));
    }
}

/// Classifying a missing object checks cancellation before building the promised set, and a
/// missing commit is never a boundary.
#[test]
fn promised_boundaries_check_cancellation_and_refuse_commits() {
    use super::{Boundaries, RetentionError};
    use crate::{InitKind, ObjectFormat, ObjectId, ObjectKind};

    let directory = tempfile::tempdir().unwrap();
    let repository = Repository::init(
        ObjectFormat::Sha1,
        directory.path().join("r.git"),
        InitKind::Bare,
    )
    .unwrap();
    let store = repository.objects().unwrap();
    let policy = RetentionPolicy::default();
    let id = ObjectId::for_blob(ObjectFormat::Sha1, b"absent\n");
    let mut boundaries = Boundaries {
        enabled: true,
        promised: None,
        found: Default::default(),
    };
    let cancelled = boundaries.admit(
        &store,
        &policy,
        &AtomicBool::new(true),
        id,
        Some(ObjectKind::Blob),
    );
    assert!(matches!(cancelled, Err(RetentionError::Cancelled)));
    assert!(boundaries.promised.is_none());
    let commit = boundaries.admit(
        &store,
        &policy,
        &AtomicBool::new(false),
        id,
        Some(ObjectKind::Commit),
    );
    assert!(matches!(commit, Err(RetentionError::MissingObject(missing)) if missing == id));
    assert!(boundaries.promised.is_none() && boundaries.found.is_empty());
}
