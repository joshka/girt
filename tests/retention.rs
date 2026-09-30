//! Retention observations against independently created Git objects and histories.
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::retention::{
    ExpireCause, MaintenanceIsolation, PruneCause, RepackError, RepackLimits, RetentionOutcome,
    RetentionPolicy,
};
use girt::{ObjectId, ObjectKind, PackLimits, ReadLimits, Repository};

fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let mut child = command
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
        .env("GIT_AUTHOR_NAME", "A")
        .env("GIT_AUTHOR_EMAIL", "a@example.com")
        .env("GIT_AUTHOR_DATE", "@1700000000 +0000")
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
        .env("GIT_COMMITTER_DATE", "@1700000000 +0000")
        .args(["-c", "commit.gpgsign=false"])
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
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn fixture_format(format: &str) -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            "--initial-branch=main",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let first = git(root.path(), &["commit-tree", &tree], b"one\n");
    let second = git(root.path(), &["commit-tree", &tree, "-p", &first], b"two\n");
    git(root.path(), &["update-ref", "refs/heads/main", &first], b"");
    let log = root.path().join("logs/refs/heads/deleted");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    fs::write(
        log,
        format!(
            "{} {second} A <a@example.com> 1700000000 +0000\tupdate\n",
            ObjectId::null(if format == "sha1" {
                girt::ObjectFormat::Sha1
            } else {
                girt::ObjectFormat::Sha256
            })
        ),
    )
    .unwrap();
    let repo = Repository::open(root.path()).unwrap();
    (root, repo, first.parse().unwrap(), second.parse().unwrap())
}

fn fixture() -> (tempfile::TempDir, Repository, ObjectId, ObjectId) {
    fixture_format("sha1")
}

struct FixtureIsolation;

impl MaintenanceIsolation for FixtureIsolation {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<Self::Guard> {
        // The disposable fixture has no independent writers, readers or alternate dependents.
        Ok(())
    }
}

struct RefusingIsolation;

impl MaintenanceIsolation for RefusingIsolation {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<Self::Guard> {
        Err(std::io::ErrorKind::WouldBlock.into())
    }
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn composed_maintenance_expires_replaces_and_prunes(#[case] format: &str) {
    let (root, repo, first, _second) = fixture_format(format);
    git(
        root.path(),
        &["repack", "-ad", "--no-write-bitmap-index"],
        b"",
    );
    let orphan: ObjectId = git(root.path(), &["hash-object", "-w", "--stdin"], b"orphan\n")
        .parse()
        .unwrap();
    let orphan_hex = orphan.to_string();
    let orphan_path = root
        .path()
        .join("objects")
        .join(&orphan_hex[..2])
        .join(&orphan_hex[2..]);
    assert!(orphan_path.exists());
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        reflog_expire_before: Some(4_000_000_000),
        reflog_expire_unreachable_before: Some(4_000_000_000),
        ..Default::default()
    };
    let result = repo
        .run_maintenance(
            &mut FixtureIsolation,
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(result.expired.unwrap().changed.len(), 1);
    assert!(result.retired.unwrap().durable);
    assert!(result.pruned.unwrap().deleted.contains(&orphan));
    assert!(!orphan_path.exists());
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn composed_reftable_maintenance_preserves_live_git_objects(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--ref-format=reftable",
            &format!("--object-format={format}"),
            "--template=",
            ".",
        ],
        b"",
    );
    git(
        root.path(),
        &["config", "core.logAllRefUpdates", "true"],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let live: ObjectId = git(root.path(), &["commit-tree", &tree], b"live\n")
        .parse()
        .unwrap();
    let expired: ObjectId = git(root.path(), &["commit-tree", &tree], b"expired\n")
        .parse()
        .unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "live",
            "refs/heads/live",
            &live.to_string(),
        ],
        b"",
    );
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "old",
            "refs/heads/old",
            &expired.to_string(),
        ],
        b"",
    );
    let repository = Repository::open(root.path()).unwrap();
    let old_name = girt::refs::RefName::new(b"refs/heads/old").unwrap();
    repository
        .references()
        .unwrap()
        .delete_without_reflog(
            &old_name,
            girt::refs::Expected::Value(girt::refs::Target::Direct(expired)),
        )
        .unwrap();
    git(
        root.path(),
        &["repack", "-ad", "--no-write-bitmap-index"],
        b"",
    );
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        reflog_expire_unreachable_before: Some(4_000_000_000),
        ..Default::default()
    };
    let report = repository
        .run_maintenance(
            &mut FixtureIsolation,
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert_eq!(report.expired.unwrap().changed.len(), 1);
    assert!(report.retired.unwrap().durable);
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &live.to_string()], b""),
        "commit"
    );
}

#[cfg(not(unix))]
#[test]
fn composed_maintenance_refuses_before_mutation() {
    let (root, repo, _, _) = fixture();
    let log = root.path().join("logs/refs/heads/deleted");
    let before = fs::read(&log).unwrap();
    let error = repo
        .run_maintenance(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::MaintenanceCause::UnsupportedPlatform
    ));
    assert!(error.report.expired.is_none());
    assert_eq!(fs::read(log).unwrap(), before);
    assert_eq!(
        fs::read_dir(root.path().join("objects/pack"))
            .unwrap()
            .count(),
        0
    );
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn isolated_loose_prune_keeps_git_roots_and_reflog_history(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let orphan = git(root.path(), &["hash-object", "-w", "--stdin"], b"orphan\n");
    let orphan: ObjectId = orphan.parse().unwrap();
    let path = root
        .path()
        .join("objects")
        .join(&orphan.to_string()[..2])
        .join(&orphan.to_string()[2..]);
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let result = repo
        .prune_unreachable_loose(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(result.deleted, [orphan]);
    assert!(!path.exists());
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
}

#[test]
fn loose_prune_refuses_without_isolation() {
    let (root, repo, _, _) = fixture();
    let orphan = git(root.path(), &["hash-object", "-w", "--stdin"], b"orphan\n");
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let error = repo
        .prune_unreachable_loose(&mut RefusingIsolation, &policy, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.cause, PruneCause::Isolation(_)));
    assert!(error.report.deleted.is_empty());
    assert_eq!(git(root.path(), &["cat-file", "-t", &orphan], b""), "blob");
}

#[test]
fn loose_prune_keeps_recent_unreachable_object() {
    let (root, repo, _, _) = fixture();
    let orphan = git(root.path(), &["hash-object", "-w", "--stdin"], b"recent\n");
    let result = repo
        .prune_unreachable_loose(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(result.deleted.is_empty());
    assert_eq!(git(root.path(), &["cat-file", "-t", &orphan], b""), "blob");
}

#[test]
fn incomplete_scan_refuses_loose_deletion() {
    let (root, repo, _, _) = fixture();
    let orphan = git(root.path(), &["hash-object", "-w", "--stdin"], b"orphan\n");
    fs::write(root.path().join("refs/heads/broken"), b"invalid\n").unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let error = repo
        .prune_unreachable_loose(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.cause, PruneCause::Incomplete(_)));
    assert!(error.report.deleted.is_empty());
    assert_eq!(git(root.path(), &["cat-file", "-t", &orphan], b""), "blob");
}

#[test]
fn loose_prune_refreshes_shallow_state_before_scanning() {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "--template=", "."], b"");
    let tree = git(root.path(), &["mktree"], b"");
    let parent = git(root.path(), &["commit-tree", &tree], b"parent\n");
    let child = git(
        root.path(),
        &["commit-tree", &tree, "-p", &parent],
        b"child\n",
    );
    git(root.path(), &["update-ref", "refs/heads/main", &child], b"");
    fs::write(root.path().join("shallow"), format!("{child}\n")).unwrap();
    let stale = Repository::open(root.path()).unwrap();
    fs::remove_file(root.path().join("shallow")).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let report = stale
        .prune_unreachable_loose(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert!(report.deleted.is_empty());
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &parent], b""),
        "commit"
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn isolated_files_reflog_expiry_preserves_live_git_objects(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let path = root.path().join("logs/refs/heads/deleted");
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        reflog_expire_unreachable_before: Some(1700000001),
        ..Default::default()
    };
    let report = repo
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.changed.len(), 1);
    assert_eq!(report.changed[0].name().as_bytes(), b"refs/heads/deleted");
    assert_eq!(fs::read(path).unwrap(), b"");
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
}

#[cfg(not(unix))]
#[test]
fn files_reflog_expiry_refuses_without_directory_durability() {
    let (root, repo, _, _) = fixture();
    let path = root.path().join("logs/refs/heads/deleted");
    let before = fs::read(&path).unwrap();
    let error = repo
        .expire_reflogs(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(error.cause, ExpireCause::UnsupportedDurability));
    assert!(error.report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn reflog_expiry_refuses_without_isolation() {
    let (root, repo, _, _) = fixture();
    let path = root.path().join("logs/refs/heads/deleted");
    let before = fs::read(&path).unwrap();
    let policy = RetentionPolicy {
        reflog_expire_unreachable_before: Some(1700000001),
        ..Default::default()
    };
    let error = repo
        .expire_reflogs(&mut RefusingIsolation, &policy, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.cause, ExpireCause::Isolation(_)));
    assert!(error.report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn reftable_expiry_without_candidates_preserves_stack() {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--ref-format=reftable",
            "--template=",
            ".",
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let before = fs::read(root.path().join("reftable/tables.list")).unwrap();
    let result = repo.expire_reflogs(
        &mut FixtureIsolation,
        &RetentionPolicy::default(),
        &AtomicBool::new(false),
    );
    #[cfg(unix)]
    assert!(result.unwrap().changed.is_empty());
    #[cfg(not(unix))]
    assert!(matches!(
        result.unwrap_err().cause,
        ExpireCause::UnsupportedBackend
    ));
    assert_eq!(
        fs::read(root.path().join("reftable/tables.list")).unwrap(),
        before
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn isolated_reftable_expiry_removes_selected_history(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--ref-format=reftable",
            &format!("--object-format={format}"),
            "--template=",
            ".",
        ],
        b"",
    );
    git(
        root.path(),
        &["config", "core.logAllRefUpdates", "true"],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let tip: ObjectId = git(root.path(), &["commit-tree", &tree], b"topic\n")
        .parse()
        .unwrap();
    let live: ObjectId = git(root.path(), &["commit-tree", &tree], b"live\n")
        .parse()
        .unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "create",
            "refs/heads/live",
            &live.to_string(),
        ],
        b"",
    );
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "create",
            "refs/heads/topic",
            &tip.to_string(),
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let name = girt::refs::RefName::new(b"refs/heads/topic").unwrap();
    repo.references()
        .unwrap()
        .delete_without_reflog(
            &name,
            girt::refs::Expected::Value(girt::refs::Target::Direct(tip)),
        )
        .unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        reflog_expire_unreachable_before: Some(4_000_000_000),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(!plan.reflog_expiry_candidates.is_empty());
    let report = repo
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(report.changed.len(), 1);
    assert!(
        repo.references()
            .unwrap()
            .imported_reflog(&name, policy.reflog, &AtomicBool::new(false))
            .unwrap()
            .is_none()
    );
    let live_name = girt::refs::RefName::new(b"refs/heads/live").unwrap();
    assert!(
        repo.references()
            .unwrap()
            .imported_reflog(&live_name, policy.reflog, &AtomicBool::new(false))
            .unwrap()
            .is_some()
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &tip.to_string()], b""),
        "commit"
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn isolated_pack_retirement_preserves_git_readability(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    git(
        root.path(),
        &["repack", "-ad", "--no-write-bitmap-index"],
        b"",
    );
    let directory = root.path().join("objects/pack");
    let old_index = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "idx"))
        .unwrap();
    let canonical_index = old_index.canonicalize().unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let report = repo
        .retire_old_packs(
            &mut FixtureIsolation,
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.published.is_some());
    assert_eq!(report.removed.first(), Some(&canonical_index));
    assert_eq!(
        report.removed.last(),
        Some(&canonical_index.with_extension("pack"))
    );
    assert!(!old_index.exists());
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
}

#[cfg(unix)]
#[test]
fn kept_pack_is_not_retired() {
    let (root, repo, _, _) = fixture();
    git(
        root.path(),
        &["repack", "-ad", "--no-write-bitmap-index"],
        b"",
    );
    let directory = root.path().join("objects/pack");
    let old_pack = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "pack")
        })
        .unwrap();
    fs::write(old_pack.with_extension("keep"), b"keep\n").unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let report = repo
        .retire_old_packs(
            &mut FixtureIsolation,
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.removed.is_empty());
    assert!(old_pack.exists());
}

#[cfg(unix)]
#[test]
fn failed_replacement_keeps_old_pack() {
    let (root, repo, first, _) = fixture();
    git(
        root.path(),
        &["repack", "-ad", "--no-write-bitmap-index"],
        b"",
    );
    let old_pack = fs::read_dir(root.path().join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension == "pack")
        })
        .unwrap();
    let mut limits = RepackLimits::default();
    limits.write.max_objects = 0;
    let error = repo
        .retire_old_packs(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            limits,
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::RetireCause::Repack(_)
    ));
    assert!(error.report.published.is_none());
    assert!(error.report.removed.is_empty());
    assert!(old_pack.exists());
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
}

#[cfg(not(unix))]
#[test]
fn pack_retirement_refuses_without_directory_durability() {
    let (root, repo, _, _) = fixture();
    let directory = root.path().join("objects/pack");
    let before = fs::read_dir(&directory).unwrap().count();
    let error = repo
        .retire_old_packs(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::RetireCause::UnsupportedPlatform
    ));
    assert!(error.report.published.is_none());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), before);
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn published_repack_is_git_usable_and_preserves_readers(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let mut reader = repo.objects(PackLimits::default()).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let result = repo
        .repack_retained(&policy, RepackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    let basename = root
        .path()
        .join("objects/pack")
        .join(format!("pack-{}", result.written.checksum));
    assert!(basename.with_extension("pack").exists());
    assert!(basename.with_extension("idx").exists());
    assert_eq!(
        fs::metadata(basename.with_extension("pack")).unwrap().len(),
        result.written.pack_bytes
    );
    assert_eq!(
        fs::metadata(basename.with_extension("idx")).unwrap().len(),
        result.written.index_bytes
    );
    assert_eq!(
        fs::read_dir(root.path().join("objects/pack"))
            .unwrap()
            .count(),
        2
    );
    git(
        root.path(),
        &[
            "verify-pack",
            "-v",
            basename.with_extension("idx").to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
    reader
        .refresh(PackLimits::default(), girt::AlternateLimits::default())
        .unwrap();
    for id in [first, second] {
        let hex = id.to_string();
        fs::remove_file(root.path().join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
    }
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
    assert_eq!(
        reader
            .read(second, ReadLimits::default())
            .unwrap()
            .unwrap()
            .id(),
        second
    );
    let again = repo
        .repack_retained(&policy, RepackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(again.written.checksum, result.written.checksum);
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn unstored_canonical_empty_tree_is_retained_and_repacked(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            &format!("--object-format={format}"),
            ".",
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let empty = repo.object_format().hash_object(ObjectKind::Tree, b"");
    assert!(
        repo.objects(PackLimits::default())
            .unwrap()
            .read(empty, ReadLimits::default())
            .unwrap()
            .is_none()
    );
    let content =
        format!("tree {empty}\nauthor A <a@b> 0 +0000\ncommitter A <a@b> 0 +0000\n\nempty\n");
    let commit = git(
        root.path(),
        &["hash-object", "-t", "commit", "-w", "--stdin"],
        content.as_bytes(),
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &commit],
        b"",
    );
    let policy = RetentionPolicy::default();
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.required.contains(&empty));
    repo.repack_retained(&policy, RepackLimits::default(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &empty.to_string()], b""),
        "tree"
    );
}

#[test]
fn repack_refuses_incomplete_plan_and_prepublication_cancellation() {
    let (root, repo, _first, second) = fixture();
    let cancelled = AtomicBool::new(true);
    assert!(matches!(
        repo.repack_retained(
            &RetentionPolicy::default(),
            RepackLimits::default(),
            &cancelled
        ),
        Err(RepackError::Cancelled)
    ));
    let policy = RetentionPolicy {
        heads: vec![second],
        max_entries: 0,
        ..Default::default()
    };
    assert!(matches!(
        repo.repack_retained(&policy, RepackLimits::default(), &AtomicBool::new(false)),
        Err(RepackError::Incomplete(_))
    ));
    assert_eq!(
        fs::read_dir(root.path().join("objects/pack"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn repack_limit_fails_before_creating_artifacts() {
    let (root, repo, _, _) = fixture();
    let mut limits = RepackLimits::default();
    limits.write.max_objects = 0;
    assert!(matches!(
        repo.repack_retained(&RetentionPolicy::default(), limits, &AtomicBool::new(false)),
        Err(RepackError::Write(girt::PackWriteError::Limit(
            "object count"
        )))
    ));
    assert_eq!(
        fs::read_dir(root.path().join("objects/pack"))
            .unwrap()
            .count(),
        0
    );
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn deleted_reflog_and_git_closure_are_retained(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert_eq!(plan.outcome, RetentionOutcome::Complete);
    assert!(plan.roots.contains(&first));
    assert!(plan.roots.contains(&second));
    let expected: std::collections::BTreeSet<ObjectId> = git(
        root.path(),
        &[
            "rev-list",
            "--objects",
            &first.to_string(),
            &second.to_string(),
        ],
        b"",
    )
    .lines()
    .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
    .collect();
    assert_eq!(plan.reachable, expected);
    let git_gc_roots: std::collections::BTreeSet<ObjectId> = git(
        root.path(),
        &["rev-list", "--objects", "--all", "--reflog"],
        b"",
    )
    .lines()
    .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
    .collect();
    assert_eq!(plan.reachable, git_gc_roots);
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
}

#[test]
fn damaged_history_and_missing_object_cannot_complete() {
    let (root, repo, _, second) = fixture();
    let log = root.path().join("logs/refs/heads/deleted");
    fs::write(
        &log,
        format!(
            "{} {second} incomplete",
            ObjectId::null(repo.object_format())
        ),
    )
    .unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&second));
    fs::remove_file(log).unwrap();
    let missing: ObjectId = "1111111111111111111111111111111111111111".parse().unwrap();
    let policy = RetentionPolicy {
        heads: vec![missing],
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&missing));
}

#[test]
fn expiry_candidates_do_not_remove_live_or_reflog_observation() {
    let (_root, repo, first, second) = fixture();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        reflog_expire_unreachable_before: Some(1700000001),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.roots.contains(&second));
    assert!(plan.reachable.contains(&second));
    assert!(plan.required.contains(&first));
    assert!(!plan.required.contains(&second));
    let name = girt::refs::RefName::new(b"refs/heads/deleted").unwrap();
    let source = girt::retention::ReflogSource::new(repo.common_dir(), name);
    assert_eq!(plan.reflog_expiry_candidates[&source], [1]);
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn expiry_positions_distinguish_private_heads_and_dedupe_shared_logs(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let checkout = root.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &second.to_string(),
        ],
        b"",
    );
    let linked = Repository::open(&checkout).unwrap();
    let record = format!("{first} {second} A <a@example.com> 1700000000 +0000\tprivate\n");
    fs::create_dir_all(repo.git_dir().join("logs")).unwrap();
    fs::create_dir_all(linked.git_dir().join("logs")).unwrap();
    fs::write(repo.git_dir().join("logs/HEAD"), &record).unwrap();
    fs::write(linked.git_dir().join("logs/HEAD"), &record).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        reflog_expire_before: Some(1700000001),
        reflog_expire_unreachable_before: Some(1700000001),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    let head = girt::refs::RefName::new(b"HEAD").unwrap();
    let main = girt::retention::ReflogSource::new(repo.git_dir(), head.clone());
    let private = girt::retention::ReflogSource::new(linked.git_dir(), head);
    let deleted = girt::retention::ReflogSource::new(
        repo.common_dir(),
        girt::refs::RefName::new(b"refs/heads/deleted").unwrap(),
    );
    assert_eq!(plan.reflog_expiry_candidates[&main], [1]);
    assert_eq!(plan.reflog_expiry_candidates[&private], [1]);
    assert_eq!(plan.reflog_expiry_candidates[&deleted], [1]);
}

#[test]
fn detached_linked_head_and_private_index_are_roots() {
    let (root, repo, _first, second) = fixture();
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    let checkout = root.path().join("linked");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &second.to_string(),
        ],
        b"",
    );
    let blob = git(
        &checkout,
        &["hash-object", "-w", "--stdin"],
        b"staged data\n",
    );
    git(
        &checkout,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},staged.txt"),
        ],
        b"",
    );
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&second));
    assert!(plan.strong_roots.contains(&blob.parse().unwrap()));
}

#[test]
fn bounded_and_cancelled_scans_report_incomplete() {
    let (_root, repo, _, _) = fixture();
    let policy = RetentionPolicy {
        max_entries: 0,
        ..Default::default()
    };
    assert!(
        !repo
            .plan_retention(&policy, &AtomicBool::new(false))
            .is_complete()
    );
    let cancel = AtomicBool::new(true);
    assert!(
        !repo
            .plan_retention(&RetentionPolicy::default(), &cancel)
            .is_complete()
    );
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn reftable_history_survives_reference_deletion_without_log_edit(#[case] format: &str) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--ref-format=reftable",
            &format!("--object-format={format}"),
            "--template=",
            ".",
        ],
        b"",
    );
    git(
        root.path(),
        &["config", "core.logAllRefUpdates", "true"],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let tip: ObjectId = git(root.path(), &["commit-tree", &tree], b"topic\n")
        .parse()
        .unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "create",
            "refs/heads/topic",
            &tip.to_string(),
        ],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let name = girt::refs::RefName::new(b"refs/heads/topic").unwrap();
    repo.references()
        .unwrap()
        .delete_without_reflog(
            &name,
            girt::refs::Expected::Value(girt::refs::Target::Direct(tip)),
        )
        .unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.roots.contains(&tip));
    assert!(plan.reachable.contains(&tip));
}

#[test]
fn pseudoref_roots_and_recent_objects_hook_policy_are_explicit() {
    let (root, repo, _first, second) = fixture();
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    fs::write(root.path().join("ORIG_HEAD"), format!("{second}\n")).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&second));
    git(
        root.path(),
        &["config", "gc.recentObjectsHook", "false"],
        b"",
    );
    let configured = Repository::open(root.path()).unwrap();
    let plan = configured.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn aggregate_reflog_budget_keeps_recovered_candidates() {
    let (_root, repo, _first, second) = fixture();
    let policy = RetentionPolicy {
        max_reflog_bytes: 0,
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&second));
}

#[test]
fn kept_pack_is_reported_for_later_repacking() {
    let (root, repo, _first, _second) = fixture();
    git(root.path(), &["repack", "-ad"], b"");
    let pack = fs::read_dir(root.path().join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .unwrap();
    fs::write(pack.with_extension("keep"), b"").unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(
        plan.protected_packs
            .contains(&fs::canonicalize(pack).unwrap())
    );
}

#[test]
fn alternate_objects_are_read_but_not_owned() {
    let root = tempfile::tempdir().unwrap();
    let primary = root.path().join("primary");
    let borrowed = root.path().join("borrowed");
    git(
        root.path(),
        &["init", "--bare", "--template=", primary.to_str().unwrap()],
        b"",
    );
    git(
        root.path(),
        &["init", "--bare", "--template=", borrowed.to_str().unwrap()],
        b"",
    );
    let blob: ObjectId = git(&borrowed, &["hash-object", "-w", "--stdin"], b"alternate\n")
        .parse()
        .unwrap();
    fs::write(
        primary.join("objects/info/alternates"),
        format!("{}\n", borrowed.join("objects").display()),
    )
    .unwrap();
    let repo = Repository::open(&primary).unwrap();
    let policy = RetentionPolicy {
        heads: vec![blob],
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.required.contains(&blob));
    assert_eq!(
        plan.alternate_stores,
        [fs::canonicalize(borrowed.join("objects")).unwrap()]
    );
    #[cfg(unix)]
    {
        let borrowed_path = borrowed
            .join("objects")
            .join(&blob.to_string()[..2])
            .join(&blob.to_string()[2..]);
        let original = fs::read(&borrowed_path).unwrap();
        let report = repo
            .retire_old_packs(
                &mut FixtureIsolation,
                &policy,
                RepackLimits::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
        assert!(report.published.is_some());
        assert_eq!(fs::read(&borrowed_path).unwrap(), original);
        fs::remove_file(borrowed_path).unwrap();
        assert_eq!(
            git(&primary, &["cat-file", "-t", &blob.to_string()], b""),
            "blob"
        );
    }
}

#[test]
fn invalid_worktree_registration_blocks_completion() {
    let (root, repo, _first, _second) = fixture();
    fs::create_dir_all(root.path().join("worktrees")).unwrap();
    fs::write(root.path().join("worktrees/bad"), b"not a registration").unwrap();
    let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn unavailable_checkout_keeps_private_head() {
    let (root, repo, _first, second) = fixture();
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    let checkout = root.path().join("unavailable");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &second.to_string(),
        ],
        b"",
    );
    fs::remove_file(checkout.join(".git")).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&second));
}

#[test]
fn declared_shallow_boundary_does_not_require_missing_parent() {
    let (root, _repo, first, second) = fixture();
    git(root.path(), &["update-ref", "-d", "refs/heads/main"], b"");
    fs::write(root.path().join("shallow"), format!("{second}\n")).unwrap();
    let hex = first.to_string();
    fs::remove_file(root.path().join("objects").join(&hex[..2]).join(&hex[2..])).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.reachable.contains(&second));
    assert!(!plan.reachable.contains(&first));
}

#[test]
fn reference_inventory_budget_blocks_completion() {
    let (_root, repo, _first, _second) = fixture();
    let policy = RetentionPolicy {
        max_reference_bytes: 0,
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn symbolic_reference_through_head_retains_target() {
    let (root, repo, first, _second) = fixture();
    fs::write(root.path().join("refs/heads/alias"), b"ref: HEAD\n").unwrap();
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&first));
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn git_gc_preserves_planned_history_and_repository_use(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    git(root.path(), &["config", "gc.reflogExpire", "never"], b"");
    git(
        root.path(),
        &["config", "gc.reflogExpireUnreachable", "never"],
        b"",
    );
    git(root.path(), &["gc", "--prune=now"], b"");
    let retained: std::collections::BTreeSet<ObjectId> = git(
        root.path(),
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
        b"",
    )
    .lines()
    .map(|line| line.parse().unwrap())
    .collect();
    assert_eq!(plan.required, retained);
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
}

#[test]
fn recent_loose_object_and_pack_are_protected() {
    let (root, repo, _first, _second) = fixture();
    let blob: ObjectId = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"recent orphan\n",
    )
    .parse()
    .unwrap();
    git(root.path(), &["repack", "-ad"], b"");
    let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&blob));
    assert!(plan.required.contains(&blob));
    assert!(!plan.protected_packs.is_empty());
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn retained_registration_roots_survive_checkout_reuse(#[case] format: &str) {
    let (root, repo, first, head) = fixture_format(format);
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    let checkout = root.path().join("retained");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &head.to_string(),
        ],
        b"",
    );
    let private = Repository::open(&checkout).unwrap();
    let tree = git(root.path(), &["mktree"], b"");
    let reference = git(root.path(), &["commit-tree", &tree], b"private reference");
    let history = git(root.path(), &["commit-tree", &tree], b"private history");
    git(
        &checkout,
        &["update-ref", "refs/worktree/saved", &reference],
        b"",
    );
    fs::create_dir_all(private.git_dir().join("logs")).unwrap();
    fs::write(
        private.git_dir().join("logs/HEAD"),
        format!("{history} {head} A <a@example.com> 1700000000 +0000\tretained\n"),
    )
    .unwrap();
    let blob = git(
        &checkout,
        &["hash-object", "-w", "--stdin"],
        b"private index",
    );
    git(
        &checkout,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("100644,{blob},staged"),
        ],
        b"",
    );
    // Keep the old registration in Git's recognized namespace, protected from ordinary prune.
    fs::write(private.git_dir().join("locked"), b"retained metadata\n").unwrap();
    fs::remove_file(checkout.join(".git")).unwrap();
    let new_checkout = root.path().join("replacement");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            new_checkout.to_str().unwrap(),
            &first.to_string(),
        ],
        b"",
    );
    let replacement = Repository::open(&new_checkout).unwrap();
    fs::rename(new_checkout.join(".git"), checkout.join(".git")).unwrap();
    fs::write(
        replacement.git_dir().join("gitdir"),
        format!("{}\n", checkout.join(".git").display()),
    )
    .unwrap();
    assert_ne!(replacement.git_dir(), private.git_dir());
    git(root.path(), &["worktree", "prune", "--expire=now"], b"");
    let git_roots = git(root.path(), &["rev-list", "--all", "--reflog"], b"");
    assert!(git_roots.lines().any(|line| line == head.to_string()));
    assert!(git_roots.lines().any(|line| line == history));
    // Git's common-directory --all omits this private ref; its owning metadata still exposes it.
    assert_eq!(
        git(
            private.git_dir(),
            &["rev-parse", "refs/worktree/saved"],
            b""
        ),
        reference
    );
    assert!(git(private.git_dir(), &["ls-files", "--stage"], b"").contains(&blob));
    let policy = RetentionPolicy {
        recent_cutoff: SystemTime::now() + Duration::from_secs(60),
        ..Default::default()
    };
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&head));
    assert!(plan.strong_roots.contains(&reference.parse().unwrap()));
    assert!(plan.strong_roots.contains(&blob.parse().unwrap()));
    assert!(plan.roots.contains(&history.parse().unwrap()));
}

#[rstest::rstest]
#[case::missing_common("commondir", None)]
#[case::malformed_common("commondir", Some(b"\n".as_slice()))]
#[case::missing_backlink("gitdir", None)]
#[case::malformed_backlink("gitdir", Some(b"bad\npath\n".as_slice()))]
#[case::missing_head("HEAD", None)]
#[case::malformed_head("HEAD", Some(b"invalid\n".as_slice()))]
fn retained_registration_corruption_blocks_scan(
    #[case] name: &str,
    #[case] replacement: Option<&[u8]>,
) {
    let (root, repo, _, head) = fixture();
    let checkout = root.path().join("retained");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &head.to_string(),
        ],
        b"",
    );
    let private = Repository::open(&checkout).unwrap();
    fs::remove_file(checkout.join(".git")).unwrap();
    let path = private.git_dir().join(name);
    fs::remove_file(&path).unwrap();
    if let Some(bytes) = replacement {
        fs::write(path, bytes).unwrap();
    }
    let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[cfg(unix)]
#[rstest::rstest]
#[case::registration(None)]
#[case::head(Some("HEAD"))]
#[case::backlink(Some("gitdir"))]
#[case::common(Some("commondir"))]
fn retained_registration_symlink_blocks_scan(#[case] name: Option<&str>) {
    let (root, repo, _, head) = fixture();
    let checkout = root.path().join("retained");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &head.to_string(),
        ],
        b"",
    );
    let private = Repository::open(&checkout).unwrap();
    let path = name.map_or_else(
        || private.git_dir().to_path_buf(),
        |name| private.git_dir().join(name),
    );
    let saved = root.path().join("saved-metadata");
    fs::rename(&path, &saved).unwrap();
    std::os::unix::fs::symlink(&saved, path).unwrap();
    let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn retained_registration_unrelated_common_blocks_scan() {
    let (root, repo, _, head) = fixture();
    let (foreign, _, _, _) = fixture();
    let checkout = root.path().join("retained");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &head.to_string(),
        ],
        b"",
    );
    let private = Repository::open(&checkout).unwrap();
    fs::write(
        private.git_dir().join("commondir"),
        format!("{}\n", foreign.path().display()),
    )
    .unwrap();
    let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn retained_registration_unreadable_metadata_blocks_scan() {
    let (root, repo, _, head) = fixture();
    let checkout = root.path().join("retained");
    git(
        root.path(),
        &[
            "worktree",
            "add",
            "--detach",
            checkout.to_str().unwrap(),
            &head.to_string(),
        ],
        b"",
    );
    let private = Repository::open(&checkout).unwrap();
    let backlink = private.git_dir().join("gitdir");
    fs::remove_file(&backlink).unwrap();
    fs::create_dir(backlink).unwrap();
    let plan = repo.plan_retention(&RetentionPolicy::default(), &AtomicBool::new(false));
    assert!(!plan.is_complete());
}
