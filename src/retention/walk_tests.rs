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
    let policy = RetentionPolicy {
        recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
        packs: PackLimits::trusted(),
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
            .read(fixture.ordinary, crate::ReadLimits::default())
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
            .read(fixture.delta, crate::ReadLimits::default())
            .is_err()
    );
}
