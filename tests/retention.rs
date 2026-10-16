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
    git_at(root, args, input, 1_700_000_000)
}

fn git_at(root: &Path, args: &[&str], input: &[u8], seconds: u64) -> String {
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
        .env("GIT_COMMITTER_DATE", format!("@{seconds} +0000"))
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
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: Some(4_000_000_000),
            disconnected_before: Some(4_000_000_000),
        });
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
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: None,
            disconnected_before: Some(4_000_000_000),
        });
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: None,
            disconnected_before: Some(1700000001),
        });
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
    let policy = RetentionPolicy::default().with_reflog_expiry(
        girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: None,
            disconnected_before: Some(1700000001),
        },
    );
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
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: None,
            disconnected_before: Some(4_000_000_000),
        });
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
    // A loose descendant makes the replacement differ from the old pack.
    let tree = git(root.path(), &["mktree"], b"");
    let parent = second.to_string();
    let child = git(
        root.path(),
        &["commit-tree", &tree, "-p", &parent],
        b"loose\n",
    );
    git(
        root.path(),
        &["update-ref", "refs/heads/loose", &child],
        b"",
    );
    let directory = root.path().join("objects/pack");
    let old_index = fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|extension| extension == "idx"))
        .unwrap();
    let canonical_index = old_index.canonicalize().unwrap();
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
fn pack_files(root: &Path, extension: &str) -> Vec<std::path::PathBuf> {
    fs::read_dir(root.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == extension))
        .collect()
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn retirement_reuses_git_deltas_and_compresses_loose_objects(#[case] format: &str) {
    let (root, repo, _, _) = fixture_format(format);
    let base: String = (0..2000).map(|n| format!("line {n}\n")).collect();
    let mut parent = git(root.path(), &["rev-parse", "main"], b"");
    let commit_version = |version: usize, parent: &str| {
        let blob = git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            format!("{base}version {version}\n").as_bytes(),
        );
        let tree = git(
            root.path(),
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        );
        git(
            root.path(),
            &["commit-tree", &tree, "-p", parent],
            format!("v{version}\n").as_bytes(),
        )
    };
    for version in 0..8 {
        parent = commit_version(version, &parent);
    }
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &parent],
        b"",
    );
    git(
        root.path(),
        &["repack", "-adf", "--no-write-bitmap-index"],
        b"",
    );
    let old_pack = pack_files(root.path(), "pack").pop().unwrap();
    let old_size = fs::metadata(&old_pack).unwrap().len();
    for version in 8..12 {
        parent = commit_version(version, &parent);
    }
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &parent],
        b"",
    );
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let report = repo
        .retire_old_packs(
            &mut FixtureIsolation,
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let written = report.published.unwrap().written;
    // Seven reused Git blob deltas plus three new ones among the four loose versions.
    assert!(written.deltas.entries >= 10, "{:?}", written.deltas);
    assert!(!old_pack.exists());
    let packs = pack_files(root.path(), "pack");
    assert_eq!(packs.len(), 1);
    // Loose objects delta only against each other, so at most one more whole blob is added.
    assert!(
        written.pack_bytes < old_size * 2,
        "{written:?} vs {old_size}"
    );
    let verify = git(
        root.path(),
        &[
            "verify-pack",
            "-v",
            packs[0].with_extension("idx").to_str().unwrap(),
        ],
        b"",
    );
    assert!(verify.contains("chain length = "), "{verify}");
    git(root.path(), &["fsck", "--full", "--strict"], b"");
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
            &RetentionPolicy::default()
                .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60)),
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    assert_eq!(again.written.pack_bytes, result.written.pack_bytes);
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
    // Git stores no object for the canonical empty tree, but reads it in every repository.
    let hex = empty.to_string();
    assert!(
        !root
            .path()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..])
            .exists()
    );
    assert!(
        repo.objects(PackLimits::default())
            .unwrap()
            .read(empty, ReadLimits::default())
            .unwrap()
            .is_some()
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
    let policy = RetentionPolicy::default()
        .with_heads(vec![second])
        .with_max_entries(0);
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&second));
    fs::remove_file(log).unwrap();
    let missing: ObjectId = "1111111111111111111111111111111111111111".parse().unwrap();
    let policy = RetentionPolicy::default()
        .with_heads(vec![missing])
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
    assert!(plan.roots.contains(&missing));
}

#[test]
fn expiry_candidates_do_not_remove_live_or_reflog_observation() {
    let (_root, repo, first, second) = fixture();
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: None,
            disconnected_before: Some(1700000001),
        });
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
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: Some(1700000001),
            disconnected_before: Some(1700000001),
        });
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&second));
    assert!(plan.strong_roots.contains(&blob.parse().unwrap()));
}

#[test]
fn bounded_and_cancelled_scans_report_incomplete() {
    let (_root, repo, _, _) = fixture();
    let policy = RetentionPolicy::default().with_max_entries(0);
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy = RetentionPolicy::default().with_max_reflog_bytes(0);
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy = RetentionPolicy::default()
        .with_heads(vec![blob])
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.reachable.contains(&second));
    assert!(!plan.reachable.contains(&first));
}

#[test]
fn reference_inventory_budget_blocks_completion() {
    let (_root, repo, _first, _second) = fixture();
    let policy = RetentionPolicy::default().with_max_reference_bytes(0);
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(!plan.is_complete());
}

#[test]
fn symbolic_reference_through_head_retains_target() {
    let (root, repo, first, _second) = fixture();
    fs::write(root.path().join("refs/heads/alias"), b"ref: HEAD\n").unwrap();
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let plan = repo.plan_retention(&policy, &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.strong_roots.contains(&first));
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn git_gc_preserves_planned_history_and_repository_use(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
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

/// Commits `count` revisions of one growing text file on `main`, so Git can find deltas.
fn grow_history(root: &Path, count: usize, parent: Option<String>) -> String {
    let mut parent = parent;
    let mut text = String::new();
    for revision in 0..count {
        for line in 0..40 {
            text.push_str(&format!(
                "revision {revision} line {line} of a slowly growing file\n"
            ));
        }
        let blob = git(root, &["hash-object", "-w", "--stdin"], text.as_bytes());
        let tree = git(
            root,
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        );
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(parent) = &parent {
            args.extend(["-p", parent]);
        }
        parent = Some(git(
            root,
            &args,
            format!("revision {revision}\n").as_bytes(),
        ));
    }
    let tip = parent.unwrap();
    git(root, &["update-ref", "refs/heads/main", &tip], b"");
    tip
}

fn pack_paths(root: &Path, extension: &str) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(root.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect();
    paths.sort();
    paths
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn repack_copies_git_deltas_byte_for_byte(#[case] format: &str) {
    let (root, _, _, _) = fixture_format(format);
    git(
        root.path(),
        &["reflog", "expire", "--expire=all", "--all"],
        b"",
    );
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    grow_history(root.path(), 30, None);
    git(
        root.path(),
        &["repack", "-adf", "--no-write-bitmap-index"],
        b"",
    );
    git(root.path(), &["prune", "--expire=now"], b"");
    let [git_pack] = pack_paths(root.path(), "pack").try_into().unwrap();
    let original_bytes = fs::read(&git_pack).unwrap();
    let original_index = git_pack.with_extension("idx");
    let original_deltas = git(
        root.path(),
        &["verify-pack", "-v", original_index.to_str().unwrap()],
        b"",
    )
    .lines()
    .filter(|line| line.split_whitespace().count() == 7)
    .count();
    let repo = Repository::open(root.path()).unwrap();
    let published = repo
        .repack_retained(
            &RetentionPolicy::default()
                .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60)),
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    // Every retained object is copied in Git's order with Git's deltas, reproducing its pack.
    assert!(published.written.deltas.entries > 0);
    assert_eq!(
        git_pack.file_stem().unwrap().to_str().unwrap(),
        format!("pack-{}", published.written.checksum)
    );
    assert_eq!(pack_paths(root.path(), "pack").len(), 1);
    let output = root.path().join(format!(
        "objects/pack/pack-{}.pack",
        published.written.checksum
    ));
    assert_eq!(fs::read(&output).unwrap(), original_bytes);
    let verbose = git(
        root.path(),
        &[
            "verify-pack",
            "-v",
            output.with_extension("idx").to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(
        verbose
            .lines()
            .filter(|line| line.split_whitespace().count() == 7)
            .count(),
        original_deltas
    );
    assert_eq!(published.written.deltas.entries as usize, original_deltas);
    git(root.path(), &["fsck", "--strict"], b"");
}

fn gc_policy() -> RetentionPolicy {
    RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: Some(1_800_000_000),
            disconnected_before: Some(1_800_000_000),
        })
}

#[cfg(unix)]
fn loose_path(root: &Path, id: ObjectId) -> std::path::PathBuf {
    let hex = id.to_string();
    root.join("objects").join(&hex[..2]).join(&hex[2..])
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1_ref("sha1", "ref")]
#[case::sha256_ref("sha256", "ref")]
#[case::sha1_pack("sha1", "pack")]
#[case::sha256_pack("sha256", "pack")]
#[case::sha1_log("sha1", "log")]
#[case::sha256_log("sha256", "log")]
#[case::sha1_index("sha1", "index")]
#[case::sha256_index("sha256", "index")]
#[case::sha1_shallow("sha1", "shallow")]
#[case::sha256_shallow("sha256", "shallow")]
#[case::sha1_alternates("sha1", "alternates")]
#[case::sha256_alternates("sha256", "alternates")]
#[case::sha1_loose("sha1", "loose")]
#[case::sha256_loose("sha256", "loose")]
fn input_changed_after_planning_refuses_before_mutation(
    #[case] format: &str,
    #[case] change: &str,
) {
    let (root, repo, first, second) = fixture_format(format);
    let orphan: ObjectId = git(root.path(), &["hash-object", "-w", "--stdin"], b"orphan")
        .parse()
        .unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "--create-reflog",
            "refs/heads/logged",
            &first.to_string(),
        ],
        b"",
    );
    let log = root.path().join("logs/refs/heads/deleted");
    let original = fs::read(&log).unwrap();
    let error = repo
        .collect_garbage_with_checkpoint(
            &mut FixtureIsolation,
            &gc_policy(),
            &AtomicBool::new(false),
            || mutate_gc_input(root.path(), first, second, orphan, change),
        )
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::MaintenanceCause::Changed
    ));
    assert!(error.report.expired.is_none());
    assert!(error.report.pruned.is_none());
    assert_eq!(fs::read(log).unwrap(), original);
    git(root.path(), &["cat-file", "-e", &orphan.to_string()], b"");
}

#[cfg(unix)]
fn mutate_gc_input(root: &Path, first: ObjectId, second: ObjectId, orphan: ObjectId, change: &str) {
    match change {
        "ref" => {
            git(
                root,
                &["update-ref", "refs/tags/saved", &orphan.to_string()],
                b"",
            );
        }
        "pack" => {
            git(root, &["repack", "-ad"], b"");
        }
        "log" => {
            git(
                root,
                &["update-ref", "refs/heads/logged", &second.to_string()],
                b"",
            );
        }
        "index" => {
            git(root, &["read-tree", &first.to_string()], b"");
        }
        "shallow" => {
            fs::write(root.join("shallow"), format!("{second}\n")).unwrap();
        }
        "alternates" => {
            fs::write(
                root.join("objects/info/alternates"),
                b"# changed topology metadata\n",
            )
            .unwrap();
        }
        "loose" => {
            git(root, &["hash-object", "-w", "--stdin"], b"new generation");
        }
        _ => unreachable!(),
    }
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1_tree("sha1", false)]
#[case::sha256_tree("sha256", false)]
#[case::sha1_tag("sha1", true)]
#[case::sha256_tag("sha256", true)]
fn loose_blob_reached_only_through_nested_tree_or_tag_survives(
    #[case] format: &str,
    #[case] tag: bool,
) {
    let (root, repo, _, _) = fixture_format(format);
    let blob: ObjectId = git(root.path(), &["hash-object", "-w", "--stdin"], b"deep blob")
        .parse()
        .unwrap();
    publish_blob_root(root.path(), blob, tag);
    repo.collect_garbage(&mut FixtureIsolation, &gc_policy(), &AtomicBool::new(false))
        .unwrap();
    git(root.path(), &["cat-file", "-e", &blob.to_string()], b"");
    git(root.path(), &["fsck", "--strict"], b"");
}

fn publish_blob_root(root: &Path, blob: ObjectId, tag: bool) {
    if tag {
        git(
            root,
            &["tag", "-a", "saved", "-m", "blob tag", &blob.to_string()],
            b"",
        );
    } else {
        let mut tree = git(
            root,
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        );
        for _ in 0..3 {
            tree = git(
                root,
                &["mktree"],
                format!("040000 tree {tree}\tnested\n").as_bytes(),
            );
        }
        let commit = git(root, &["commit-tree", &tree], b"nested\n");
        git(root, &["update-ref", "refs/heads/deep", &commit], b"");
    }
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn missing_reachable_blob_refuses_both_phases(#[case] format: &str) {
    let (root, repo, _, _) = fixture_format(format);
    let blob: ObjectId = git(root.path(), &["hash-object", "-w", "--stdin"], b"missing")
        .parse()
        .unwrap();
    publish_blob_root(root.path(), blob, false);
    fs::remove_file(loose_path(root.path(), blob)).unwrap();
    let log = root.path().join("logs/refs/heads/deleted");
    let original = fs::read(&log).unwrap();
    let error = repo
        .collect_garbage(&mut FixtureIsolation, &gc_policy(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::MaintenanceCause::Incomplete(_)
    ));
    assert!(error.report.expired.is_none());
    assert!(error.report.pruned.is_none());
    assert_eq!(fs::read(log).unwrap(), original);
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn max_bytes_counts_decoded_graph_objects_only(#[case] format: &str) {
    let (root, repo, _, _) = fixture_format(format);
    let blob: ObjectId = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        &vec![b'x'; 65536],
    )
    .parse()
    .unwrap();
    publish_blob_root(root.path(), blob, false);
    let plan = repo.plan_retention(&(gc_policy()).with_max_bytes(4096), &AtomicBool::new(false));
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.required.contains(&blob));
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn composite_gc_refuses_limits_before_mutation(#[case] format: &str) {
    let (root, repo, _, _) = fixture_format(format);
    let log = root.path().join("logs/refs/heads/deleted");
    let original = fs::read(&log).unwrap();
    let error = repo
        .collect_garbage(
            &mut FixtureIsolation,
            &(gc_policy()).with_max_entries(1),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::MaintenanceCause::Incomplete(_)
    ));
    assert_eq!(fs::read(log).unwrap(), original);
    assert!(error.report.expired.is_none());
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn expire_and_prune_match_git_gc_retained_set_and_reflogs(#[case] format: &str) {
    let (a, repo, _, second) = fixture_format(format);
    let (b, _, _, _) = fixture_format(format);
    let old_orphan = gc_fidelity_cutoffs(a.path());
    assert_eq!(old_orphan, gc_fidelity_cutoffs(b.path()));
    let live_log = a.path().join("logs/refs/heads/live");
    assert!(
        fs::read_to_string(&live_log)
            .unwrap()
            .contains("1500000000")
    );
    // Keep an unreachable endpoint solely through a recent reflog record.
    let recent = format!("{second} {second} A <a@example.com> 1900000000 +0000\trecent\n");
    fs::OpenOptions::new()
        .append(true)
        .open(a.path().join("logs/refs/heads/deleted"))
        .unwrap()
        .write_all(recent.as_bytes())
        .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(b.path().join("logs/refs/heads/deleted"))
        .unwrap()
        .write_all(recent.as_bytes())
        .unwrap();
    repo.collect_garbage(
        &mut FixtureIsolation,
        &(gc_policy())
            .with_recent_cutoff(SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000))
            .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
                live_before: Some(1_600_000_000),
                disconnected_before: Some(1_800_000_000),
            }),
        &AtomicBool::new(false),
    )
    .unwrap();
    git(
        b.path(),
        &[
            "-c",
            "gc.reflogExpire=@1600000000",
            "-c",
            "gc.reflogExpireUnreachable=@1800000000",
            "gc",
            "--prune=@1800000000",
        ],
        b"",
    );
    for root in [a.path(), b.path()] {
        let remaining = git(
            root,
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
            b"",
        );
        assert!(!remaining.lines().any(|id| id == old_orphan.to_string()));
        let retained_log = fs::read_to_string(root.join("logs/refs/heads/live")).unwrap();
        assert!(!retained_log.contains("1500000000"));
        assert!(retained_log.contains("1700000000"));
    }
    assert_eq!(
        git(
            a.path(),
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)"
            ],
            b""
        ),
        git(
            b.path(),
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)"
            ],
            b""
        )
    );
    assert_eq!(
        fs::read(a.path().join("logs/refs/heads/deleted")).unwrap(),
        fs::read(b.path().join("logs/refs/heads/deleted")).unwrap()
    );
    assert_eq!(
        fs::read(a.path().join("logs/refs/heads/live")).unwrap(),
        fs::read(b.path().join("logs/refs/heads/live")).unwrap()
    );
    git(a.path(), &["fsck", "--strict"], b"");
}

#[cfg(unix)]
fn gc_fidelity_cutoffs(root: &Path) -> ObjectId {
    let old_orphan = git(root, &["hash-object", "-w", "--stdin"], b"old orphan")
        .parse()
        .unwrap();
    let recent: ObjectId = git(root, &["hash-object", "-w", "--stdin"], b"recent orphan")
        .parse()
        .unwrap();
    fs::File::open(loose_path(root, recent))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_900_000_000))
        .unwrap();
    let first = git(root, &["rev-parse", "refs/heads/main"], b"");
    let tree = git(root, &["mktree"], b"");
    let live = git(root, &["commit-tree", &tree, "-p", &first], b"live branch");
    git_at(
        root,
        &[
            "update-ref",
            "--create-reflog",
            "-m",
            "live old",
            "refs/heads/live",
            &first,
        ],
        b"",
        1_500_000_000,
    );
    git_at(
        root,
        &[
            "update-ref",
            "-m",
            "live retained",
            "refs/heads/live",
            &live,
        ],
        b"",
        1_700_000_000,
    );
    old_orphan
}

#[cfg(unix)]
#[test]
fn composite_gc_holds_one_isolation_guard() {
    struct CountIsolation(usize);
    impl MaintenanceIsolation for CountIsolation {
        type Guard = ();
        fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
            self.0 += 1;
            Ok(())
        }
    }
    let (_root, repo, _, _) = fixture();
    let mut isolation = CountIsolation(0);
    repo.collect_garbage(&mut isolation, &gc_policy(), &AtomicBool::new(false))
        .unwrap();
    assert_eq!(isolation.0, 1);
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn recent_pack_does_not_classify_unreachable_history_as_live(#[case] format: &str) {
    let (root, repo, first, second) = fixture_format(format);
    git(
        root.path(),
        &["update-ref", "refs/heads/temporary", &second.to_string()],
        b"",
    );
    git(root.path(), &["repack", "-ad"], b"");
    git(
        root.path(),
        &["update-ref", "-d", "refs/heads/temporary"],
        b"",
    );
    let plan = repo.plan_retention(
        &RetentionPolicy::default().with_reflog_expiry(
            girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
                live_before: Some(1_600_000_000),
                disconnected_before: Some(1_800_000_000),
            },
        ),
        &AtomicBool::new(false),
    );
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    assert!(plan.required.contains(&first));
    assert!(!plan.strong_roots.contains(&second));
    assert!(!plan.required.contains(&second));
    assert!(!plan.reflog_expiry_candidates.is_empty());
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1_recent("sha1", false, false)]
#[case::sha256_recent("sha256", false, false)]
#[case::sha1_keep("sha1", true, false)]
#[case::sha256_keep("sha256", true, false)]
#[case::sha1_retire("sha1", false, true)]
#[case::sha256_retire("sha256", false, true)]
fn protected_pack_keeps_old_loose_descendants(
    #[case] format: &str,
    #[case] keep: bool,
    #[case] repack: bool,
) {
    let (root, repo, _, _) = fixture_format(format);
    let blob = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"protected descendant",
    );
    let tree = git(
        root.path(),
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    );
    let commit = git(
        root.path(),
        &["commit-tree", &tree],
        b"unreachable packed commit\n",
    );
    let checksum = git(
        root.path(),
        &["pack-objects", "objects/pack/pack"],
        format!("{commit}\n").as_bytes(),
    );
    fs::remove_file(loose_path(root.path(), commit.parse().unwrap())).unwrap();
    protect_and_backdate(root.path(), &checksum, keep);
    collect_with_optional_repack(
        &repo,
        &(gc_policy()).with_recent_cutoff(SystemTime::now() - Duration::from_secs(86400)),
        repack,
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-p", &tree], b""),
        format!("100644 blob {blob}\tfile")
    );
    git(root.path(), &["cat-file", "-e", &blob], b"");
    git(root.path(), &["fsck", "--strict"], b"");
}

#[cfg(unix)]
fn protect_and_backdate(root: &Path, checksum: &str, keep: bool) {
    for fanout in fs::read_dir(root.join("objects")).unwrap() {
        let fanout = fanout.unwrap();
        if fanout.file_name().to_string_lossy().len() == 2 {
            for file in fs::read_dir(fanout.path()).unwrap() {
                fs::File::open(file.unwrap().path())
                    .unwrap()
                    .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                    .unwrap();
            }
        }
    }
    if keep {
        fs::write(root.join(format!("objects/pack/pack-{checksum}.keep")), b"").unwrap();
        fs::File::open(root.join(format!("objects/pack/pack-{checksum}.pack")))
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
            .unwrap();
    }
}

#[cfg(unix)]
fn collect_with_optional_repack(repo: &Repository, policy: &RetentionPolicy, repack: bool) {
    if repack {
        repo.run_maintenance(
            &mut FixtureIsolation,
            policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    } else {
        repo.collect_garbage(&mut FixtureIsolation, policy, &AtomicBool::new(false))
            .unwrap();
    }
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn composite_fingerprints_configuration_symlink_target(#[case] format: &str) {
    let (root, repo, _, second) = fixture_format(format);
    let config = root.path().join("config");
    let target = root.path().join("external-config");
    fs::rename(&config, &target).unwrap();
    std::os::unix::fs::symlink(&target, &config).unwrap();
    let error = repo
        .collect_garbage_with_checkpoint(
            &mut FixtureIsolation,
            &gc_policy(),
            &AtomicBool::new(false),
            || {
                fs::OpenOptions::new()
                    .append(true)
                    .open(&target)
                    .unwrap()
                    .write_all(b"\n[gc]\npruneExpire = never\n")
                    .unwrap();
            },
        )
        .unwrap_err();
    assert!(
        matches!(error.cause, girt::retention::MaintenanceCause::Changed),
        "{:?}",
        error.cause
    );
    assert!(error.report.expired.is_none());
    assert!(error.report.pruned.is_none());
    git(root.path(), &["cat-file", "-e", &second.to_string()], b"");
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn recent_unindexed_pack_refuses_before_loose_deletion(#[case] format: &str) {
    let (root, repo, _, _) = fixture_format(format);
    let blob: ObjectId = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"unindexed protection",
    )
    .parse()
    .unwrap();
    let checksum = git(
        root.path(),
        &["pack-objects", "objects/pack/pack"],
        format!("{blob}\n").as_bytes(),
    );
    fs::remove_file(
        root.path()
            .join(format!("objects/pack/pack-{checksum}.idx")),
    )
    .unwrap();
    fs::File::open(loose_path(root.path(), blob))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000))
        .unwrap();
    let error = repo
        .collect_garbage(
            &mut FixtureIsolation,
            &(gc_policy()).with_recent_cutoff(SystemTime::now() - Duration::from_secs(86400)),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::MaintenanceCause::Incomplete(_)
    ));
    assert!(error.report.expired.is_none());
    assert!(error.report.pruned.is_none());
    git(root.path(), &["cat-file", "-e", &blob.to_string()], b"");
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1_loose(girt::ObjectFormat::Sha1, "sha1", "loose")]
#[case::sha256_loose(girt::ObjectFormat::Sha256, "sha256", "loose")]
#[case::sha1_pack(girt::ObjectFormat::Sha1, "sha1", "pack")]
#[case::sha256_pack(girt::ObjectFormat::Sha256, "sha256", "pack")]
#[case::sha1_alternate(girt::ObjectFormat::Sha1, "sha1", "alternate")]
#[case::sha256_alternate(girt::ObjectFormat::Sha256, "sha256", "alternate")]
fn packed_blob_cannot_hide_a_readable_loose_commit_before_pruning(
    #[case] format: girt::ObjectFormat,
    #[case] name: &str,
    #[case] copy: &str,
) {
    let (root, repo, _, _) = fixture_format(name);
    let blob = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"required descendant",
    );
    let tree = git(
        root.path(),
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    );
    let commit: ObjectId = git(root.path(), &["commit-tree", &tree], b"readable commit")
        .parse()
        .unwrap();
    let tag = git(root.path(), &["hash-object", "-t", "tag", "--literally", "-w", "--stdin"],
        format!("object {commit}\ntype blob\ntag disguised\ntagger A <a@example.com> 1700000000 +0000\n\nkind assertion\n").as_bytes());
    git(
        root.path(),
        &["update-ref", "refs/tags/disguised", &tag],
        b"",
    );
    move_graph_copy(root.path(), name, commit, copy);
    let shadow: ObjectId = git(root.path(), &["hash-object", "-w", "--stdin"], b"shadow")
        .parse()
        .unwrap();
    let checksum = git(
        root.path(),
        &["pack-objects", "objects/pack/pack"],
        format!("{shadow}\n").as_bytes(),
    );
    let pack = root
        .path()
        .join(format!("objects/pack/pack-{checksum}.pack"));
    let shadow_pack = root.path().join("objects/pack/000-shadow.pack");
    fs::rename(
        pack.with_extension("idx"),
        shadow_pack.with_extension("idx"),
    )
    .unwrap();
    fs::rename(pack, &shadow_pack).unwrap();
    let pack = shadow_pack;
    mislabel_single_entry_index(&pack.with_extension("idx"), format, commit);
    let log = root.path().join("logs/refs/heads/deleted");
    let original_log = fs::read(&log).unwrap();
    let error = repo
        .collect_garbage(&mut FixtureIsolation, &gc_policy(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::MaintenanceCause::Incomplete(_)
    ));
    assert!(error.report.expired.is_none());
    assert!(error.report.pruned.is_none());
    assert_eq!(fs::read(log).unwrap(), original_log);
    fs::remove_file(pack.with_extension("idx")).unwrap();
    fs::remove_file(pack).unwrap();
    git(
        root.path(),
        &["update-ref", "-d", "refs/tags/disguised"],
        b"",
    );
    fs::remove_file(loose_path(root.path(), tag.parse().unwrap())).unwrap();
    git(root.path(), &["cat-file", "-e", &blob], b"");
    git(root.path(), &["cat-file", "-e", &tree], b"");
    git(root.path(), &["fsck", "--strict"], b"");
}

#[cfg(unix)]
fn move_graph_copy(root: &Path, format: &str, commit: ObjectId, copy: &str) {
    match copy {
        "loose" => {}
        "pack" => {
            git(
                root,
                &["pack-objects", "objects/pack/pack"],
                format!("{commit}\n").as_bytes(),
            );
            fs::remove_file(loose_path(root, commit)).unwrap();
        }
        "alternate" => {
            git(
                root,
                &[
                    "init",
                    "--bare",
                    "--template=",
                    &format!("--object-format={format}"),
                    "borrowed",
                ],
                b"",
            );
            let alternate = root.join("borrowed");
            let destination = loose_path(&alternate, commit);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::rename(loose_path(root, commit), destination).unwrap();
            fs::write(
                root.join("objects/info/alternates"),
                format!("{}\n", alternate.join("objects").display()),
            )
            .unwrap();
        }
        _ => unreachable!(),
    }
}

#[cfg(unix)]
fn mislabel_single_entry_index(path: &Path, format: girt::ObjectFormat, id: ObjectId) {
    use sha1::Digest;
    let mut bytes = fs::read(path).unwrap();
    let width = format.digest_len();
    // Original v2 index mutation: one identity, cumulative fanout and independent checksum.
    bytes[1032..1032 + width].copy_from_slice(id.as_bytes());
    for bucket in 0..256 {
        let count = u32::from(bucket >= usize::from(id.as_bytes()[0]));
        bytes[8 + bucket * 4..12 + bucket * 4].copy_from_slice(&count.to_be_bytes());
    }
    let end = bytes.len() - width;
    let checksum = match format {
        girt::ObjectFormat::Sha1 => sha1::Sha1::digest(&bytes[..end]).to_vec(),
        girt::ObjectFormat::Sha256 => sha2::Sha256::digest(&bytes[..end]).to_vec(),
        _ => panic!("unsupported hash format in test fixture"),
    };
    bytes[end..].copy_from_slice(&checksum);
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(path, bytes).unwrap();
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn corrupted_blob_shaped_root_refuses_before_descendant_deletion(#[case] format: &str) {
    use std::os::unix::fs::PermissionsExt;
    let (root, repo, _, _) = fixture_format(format);
    let blob = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"required descendant",
    );
    let tree = git(
        root.path(),
        &["mktree"],
        format!("100644 blob {blob}\tfile\n").as_bytes(),
    );
    let commit: ObjectId = git(root.path(), &["commit-tree", &tree], b"readable commit")
        .parse()
        .unwrap();
    git(
        root.path(),
        &["update-ref", "refs/heads/main", &commit.to_string()],
        b"",
    );
    let checksum = git(
        root.path(),
        &["pack-objects", "objects/pack/pack"],
        format!("{commit}\n").as_bytes(),
    );
    fs::remove_file(loose_path(root.path(), commit)).unwrap();
    let pack = root
        .path()
        .join(format!("objects/pack/pack-{checksum}.pack"));
    let original = fs::read(&pack).unwrap();
    let mut corrupt = original.clone();
    corrupt[12] = (corrupt[12] & !0x70) | 0x30;
    fs::set_permissions(&pack, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&pack, corrupt).unwrap();
    let error = repo
        .collect_garbage(&mut FixtureIsolation, &gc_policy(), &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(
        error.cause,
        girt::retention::MaintenanceCause::Incomplete(_)
    ));
    assert!(error.report.expired.is_none());
    assert!(error.report.pruned.is_none());
    fs::write(pack, original).unwrap();
    git(root.path(), &["cat-file", "-e", &blob], b"");
    git(root.path(), &["cat-file", "-e", &tree], b"");
    git(root.path(), &["fsck", "--strict"], b"");
}

/// Opening skips the index checksum, so a plan that authorizes deletion must check it itself.
#[cfg(unix)]
#[rstest::rstest]
#[case::prune(false)]
#[case::maintenance(true)]
fn damaged_index_checksum_refuses_deletion(#[case] maintenance: bool) {
    let (root, _, _, _) = fixture();
    git(root.path(), &["repack", "-ad"], b"");
    let orphan = git(root.path(), &["hash-object", "-w", "--stdin"], b"orphan\n");
    let index = pack_files(root.path(), "idx").pop().unwrap();
    let mut bytes = fs::read(&index).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::remove_file(&index).unwrap();
    fs::write(&index, &bytes).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let cancel = AtomicBool::new(false);
    if maintenance {
        let error = repo
            .run_maintenance(
                &mut FixtureIsolation,
                &policy,
                RepackLimits::default(),
                &cancel,
            )
            .unwrap_err();
        assert!(
            error
                .report
                .pruned
                .is_none_or(|pruned| pruned.deleted.is_empty())
        );
    } else {
        let error = repo
            .prune_unreachable_loose(&mut FixtureIsolation, &policy, &cancel)
            .unwrap_err();
        assert!(matches!(error.cause, PruneCause::Incomplete(_)));
        assert!(error.report.deleted.is_empty());
    }
    assert_eq!(git(root.path(), &["cat-file", "-t", &orphan], b""), "blob");
    assert_eq!(pack_files(root.path(), "idx"), std::slice::from_ref(&index));
    assert_eq!(fs::read(&index).unwrap(), bytes);
}

// Git generates the references and reflog; removing its only pack creates history that a
// no-op must not need to read. No retained bytes are rewritten on this path.
fn missing_packed_history(format: &str) -> (tempfile::TempDir, Repository) {
    missing_packed_history_backend(format, "files")
}

fn missing_packed_history_backend(format: &str, backend: &str) -> (tempfile::TempDir, Repository) {
    let root = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "init",
            "--bare",
            "--template=",
            &format!("--object-format={format}"),
            &format!("--ref-format={backend}"),
            ".",
        ],
        b"",
    );
    let tree = git(root.path(), &["mktree"], b"");
    let commit = git(root.path(), &["commit-tree", &tree], b"packed history\n");
    git(
        root.path(),
        &[
            "update-ref",
            "--create-reflog",
            "-m",
            "generated",
            "refs/heads/main",
            &commit,
        ],
        b"",
    );
    git(root.path(), &["repack", "-ad"], b"");
    fs::remove_dir_all(root.path().join("objects/pack")).unwrap();
    fs::create_dir(root.path().join("objects/pack")).unwrap();
    let repo = Repository::open(root.path()).unwrap();
    (root, repo)
}

#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn prune_without_old_loose_candidates_does_not_audit_history(#[case] format: &str) {
    let (root, repo) = missing_packed_history(format);
    let before = fs::read(root.path().join("logs/refs/heads/main")).unwrap();
    git(root.path(), &["prune", "--expire=now"], b"");
    let report = repo
        .prune_unreachable_loose(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            &AtomicBool::new(false),
        )
        .unwrap();
    assert!(report.deleted.is_empty());
    assert_eq!(
        fs::read(root.path().join("logs/refs/heads/main")).unwrap(),
        before
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn expiry_at_cutoff_does_not_audit_history(#[case] format: &str) {
    let (root, repo) = missing_packed_history(format);
    let path = root.path().join("logs/refs/heads/main");
    let before = fs::read(&path).unwrap();
    git(
        root.path(),
        &[
            "reflog",
            "expire",
            "--all",
            "--expire=@1700000000",
            "--expire-unreachable=@1700000000",
        ],
        b"",
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    let policy = RetentionPolicy::default().with_reflog_expiry(
        girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: Some(1700000000),
            disconnected_before: Some(1700000000),
        },
    );
    let report = repo
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert!(report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn eligible_expiry_with_missing_history_refuses_without_rewrite() {
    let (root, repo) = missing_packed_history("sha1");
    let path = root.path().join("logs/refs/heads/main");
    let before = fs::read(&path).unwrap();
    let policy = RetentionPolicy::default().with_reflog_expiry(
        girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: None,
            disconnected_before: Some(1700000001),
        },
    );
    let error = repo
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.cause, ExpireCause::Incomplete(_)));
    assert!(error.report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn old_loose_candidate_with_missing_history_refuses_without_deletion() {
    let (root, repo) = missing_packed_history("sha1");
    let id = git(root.path(), &["hash-object", "-w", "--stdin"], b"only copy");
    let policy =
        RetentionPolicy::default().with_recent_cutoff(SystemTime::now() + Duration::from_secs(60));
    let error = repo
        .prune_unreachable_loose(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.cause, PruneCause::Incomplete(_)));
    assert!(error.report.deleted.is_empty());
    assert_eq!(
        git(root.path(), &["cat-file", "blob", &id], b""),
        "only copy"
    );
}

#[rstest::rstest]
#[case::entry_limit(0, false)]
#[case::cancelled(usize::MAX, true)]
fn prune_empty_inventory_still_enforces_scan_bounds(
    #[case] entries: usize,
    #[case] cancelled: bool,
) {
    let (root, repo) = missing_packed_history("sha1");
    let before = fs::read(root.path().join("logs/refs/heads/main")).unwrap();
    let policy = RetentionPolicy::default().with_max_entries(entries);
    let error = repo
        .prune_unreachable_loose(&mut FixtureIsolation, &policy, &AtomicBool::new(cancelled))
        .unwrap_err();
    assert!(error.report.deleted.is_empty());
    assert_eq!(
        fs::read(root.path().join("logs/refs/heads/main")).unwrap(),
        before
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::entry_limit(0, u64::MAX)]
#[case::byte_limit(usize::MAX, 0)]
fn expiry_noop_still_enforces_observation_budgets(#[case] entries: usize, #[case] bytes: u64) {
    let (root, repo) = missing_packed_history("sha1");
    let path = root.path().join("logs/refs/heads/main");
    let before = fs::read(&path).unwrap();
    let policy = RetentionPolicy::default()
        .with_max_entries(entries)
        .with_max_reflog_bytes(bytes);
    let error = repo
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error.cause, ExpireCause::Incomplete(_)));
    assert!(error.report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn recent_loose_candidate_is_preserved_without_a_history_walk() {
    let (root, repo) = missing_packed_history("sha1");
    let id = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"recent only copy",
    );
    let policy = RetentionPolicy::default().with_recent_cutoff(SystemTime::UNIX_EPOCH);
    // Git 2.55 still walks damaged roots when recent loose objects exist. Use a healthy
    // independently generated history for its retention oracle, then test girt's no-op contract.
    let (oracle, _, _, _) = fixture();
    let oracle_id = git(
        oracle.path(),
        &["hash-object", "-w", "--stdin"],
        b"recent only copy",
    );
    git(oracle.path(), &["prune", "--expire=never"], b"");
    assert_eq!(oracle_id, id);
    assert_eq!(
        git(oracle.path(), &["cat-file", "blob", &id], b""),
        "recent only copy"
    );
    let report = repo
        .prune_unreachable_loose(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert!(report.deleted.is_empty());
    assert_eq!(
        git(root.path(), &["cat-file", "blob", &id], b""),
        "recent only copy"
    );
}

#[test]
fn malformed_loose_inventory_cannot_establish_noop() {
    let (root, repo) = missing_packed_history("sha1");
    fs::create_dir(root.path().join("objects/zz")).unwrap();
    let error = repo
        .prune_unreachable_loose(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(error.cause, PruneCause::Incomplete(_)));
    assert!(error.report.deleted.is_empty());
}

#[cfg(unix)]
#[rstest::rstest]
#[case::unterminated(b"incomplete record".as_slice())]
#[case::invalid_fields_at_eof(b"invalid record\n".as_slice())]
fn malformed_reflog_cannot_establish_noop(#[case] bytes: &[u8]) {
    let (root, repo) = missing_packed_history("sha1");
    let path = root.path().join("logs/refs/heads/main");
    fs::write(&path, bytes).unwrap();
    let error = repo
        .expire_reflogs(
            &mut FixtureIsolation,
            &RetentionPolicy::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();
    assert!(matches!(error.cause, ExpireCause::Incomplete(_)));
    assert!(error.report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn reftable_noop_expiry_preserves_missing_history_stack(#[case] format: &str) {
    let (root, repo) = missing_packed_history_backend(format, "reftable");
    let path = root.path().join("reftable/tables.list");
    let before = fs::read(&path).unwrap();
    let policy = RetentionPolicy::default().with_reflog_expiry(
        girt::retention::ReflogExpiryPolicy::ClassifiedCutoffs {
            live_before: Some(1700000000),
            disconnected_before: Some(1700000000),
        },
    );
    let report = repo
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert!(report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum CutoffOperation {
    Prune,
    GarbageCollection,
    Maintenance,
}

#[cfg(unix)]
fn prune_at_cutoff(
    repo: &Repository,
    policy: &RetentionPolicy,
    operation: CutoffOperation,
) -> Vec<ObjectId> {
    let cancel = AtomicBool::new(false);
    match operation {
        CutoffOperation::Prune => {
            repo.prune_unreachable_loose(&mut FixtureIsolation, policy, &cancel)
                .unwrap()
                .deleted
        }
        CutoffOperation::GarbageCollection => {
            repo.collect_garbage(&mut FixtureIsolation, policy, &cancel)
                .unwrap()
                .pruned
                .unwrap()
                .deleted
        }
        CutoffOperation::Maintenance => {
            repo.run_maintenance(
                &mut FixtureIsolation,
                policy,
                RepackLimits::default(),
                &cancel,
            )
            .unwrap()
            .pruned
            .unwrap()
            .deleted
        }
    }
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn default_policy_preserves_epoch_zero_loose_objects(
    #[case] format: &str,
    #[values(
        CutoffOperation::Prune,
        CutoffOperation::GarbageCollection,
        CutoffOperation::Maintenance
    )]
    operation: CutoffOperation,
) {
    let (root, repo, _, _) = fixture_format(format);
    let id: ObjectId = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"epoch zero",
    )
    .parse()
    .unwrap();
    fs::File::open(loose_path(root.path(), id))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH)
        .unwrap();
    assert_eq!(
        fs::metadata(loose_path(root.path(), id))
            .unwrap()
            .modified()
            .unwrap(),
        SystemTime::UNIX_EPOCH
    );
    assert!(prune_at_cutoff(&repo, &RetentionPolicy::default(), operation).is_empty());
    assert_eq!(
        git(root.path(), &["cat-file", "blob", &id.to_string()], b""),
        "epoch zero"
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn subsecond_grace_deliberately_keeps_more_than_git(
    #[case] format: &str,
    #[values(
        CutoffOperation::Prune,
        CutoffOperation::GarbageCollection,
        CutoffOperation::Maintenance
    )]
    operation: CutoffOperation,
) {
    let (root, repo, _, _) = fixture_format(format);
    let (oracle, _, _, _) = fixture_format(format);
    let id: ObjectId = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"half second",
    )
    .parse()
    .unwrap();
    assert_eq!(
        git(
            oracle.path(),
            &["hash-object", "-w", "--stdin"],
            b"half second"
        ),
        id.to_string()
    );
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let modified = cutoff + Duration::from_millis(500);
    fs::File::open(loose_path(root.path(), id))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    fs::File::open(loose_path(oracle.path(), id))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(
        fs::metadata(loose_path(root.path(), id))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert_eq!(
        fs::metadata(loose_path(oracle.path(), id))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    // Git's second-resolution expiry deletes this object; girt preserves its exact grace period.
    git(
        oracle.path(),
        &["prune", "--expire=2023-11-14T22:13:20Z"],
        b"",
    );
    assert!(!loose_path(oracle.path(), id).exists());
    assert!(
        prune_at_cutoff(
            &repo,
            &RetentionPolicy::default().with_recent_cutoff(cutoff),
            operation
        )
        .is_empty()
    );
    assert_eq!(
        git(root.path(), &["cat-file", "blob", &id.to_string()], b""),
        "half second"
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::before(-1, true)]
#[case::equal(0, true)]
#[case::after(1, false)]
fn loose_prune_cutoff_agrees_with_git(
    #[values("sha1", "sha256")] format: &str,
    #[values(
        CutoffOperation::Prune,
        CutoffOperation::GarbageCollection,
        CutoffOperation::Maintenance
    )]
    operation: CutoffOperation,
    #[case] offset: i64,
    #[case] deleted: bool,
) {
    let (root, repo, _, _) = fixture_format(format);
    let (oracle, _, _, _) = fixture_format(format);
    let id: ObjectId = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"cutoff boundary",
    )
    .parse()
    .unwrap();
    assert_eq!(
        git(
            oracle.path(),
            &["hash-object", "-w", "--stdin"],
            b"cutoff boundary"
        ),
        id.to_string()
    );
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let modified = SystemTime::UNIX_EPOCH + Duration::from_secs((1_700_000_000 + offset) as u64);
    fs::File::open(loose_path(root.path(), id))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    fs::File::open(loose_path(oracle.path(), id))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(
        fs::metadata(loose_path(root.path(), id))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    assert_eq!(
        fs::metadata(loose_path(oracle.path(), id))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    git(
        oracle.path(),
        &["prune", "--expire=2023-11-14T22:13:20Z"],
        b"",
    );
    assert_eq!(!loose_path(oracle.path(), id).exists(), deleted);
    let policy = RetentionPolicy::default().with_recent_cutoff(cutoff);
    let removed = prune_at_cutoff(&repo, &policy, operation);
    assert_eq!(removed.contains(&id), deleted);
    assert_eq!(!loose_path(root.path(), id).exists(), deleted);
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn loose_object_one_nanosecond_newer_than_cutoff_survives(
    #[case] format: &str,
    #[values(
        CutoffOperation::Prune,
        CutoffOperation::GarbageCollection,
        CutoffOperation::Maintenance
    )]
    operation: CutoffOperation,
) {
    let (root, repo, _, _) = fixture_format(format);
    let id: ObjectId = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"subsecond grace",
    )
    .parse()
    .unwrap();
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let modified = cutoff + Duration::from_nanos(1);
    fs::File::open(loose_path(root.path(), id))
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(
        fs::metadata(loose_path(root.path(), id))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    let policy = RetentionPolicy::default().with_recent_cutoff(cutoff);
    assert!(!prune_at_cutoff(&repo, &policy, operation).contains(&id));
    assert_eq!(
        git(root.path(), &["cat-file", "blob", &id.to_string()], b""),
        "subsecond grace"
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn newer_loose_root_protects_its_old_complete_closure(
    #[case] format: &str,
    #[values(
        CutoffOperation::Prune,
        CutoffOperation::GarbageCollection,
        CutoffOperation::Maintenance
    )]
    operation: CutoffOperation,
) {
    let (root, repo, first, second) = fixture_format(format);
    let tree: ObjectId = git(
        root.path(),
        &["rev-parse", &format!("{first}^{{tree}}")],
        b"",
    )
    .parse()
    .unwrap();
    git(root.path(), &["update-ref", "-d", "refs/heads/main"], b"");
    fs::remove_file(root.path().join("logs/refs/heads/deleted")).unwrap();
    let cutoff = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    fs::File::open(loose_path(root.path(), first))
        .unwrap()
        .set_modified(cutoff)
        .unwrap();
    fs::File::open(loose_path(root.path(), tree))
        .unwrap()
        .set_modified(cutoff - Duration::from_secs(1))
        .unwrap();
    fs::File::open(loose_path(root.path(), second))
        .unwrap()
        .set_modified(cutoff + Duration::from_nanos(1))
        .unwrap();
    let policy = RetentionPolicy::default().with_recent_cutoff(cutoff);
    assert!(prune_at_cutoff(&repo, &policy, operation).is_empty());
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &first.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &second.to_string()], b""),
        "commit"
    );
    assert_eq!(
        git(root.path(), &["cat-file", "-t", &tree.to_string()], b""),
        "tree"
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::never("sha1", "never", "never", b"", "refs/heads/main")]
#[case::expire_all("sha1", "now", "never", b"", "refs/heads/main")]
#[case::unreachable_only("sha1", "never", "now", b"", "refs/heads/main")]
#[case::sha256("sha256", "now", "never", b"", "refs/heads/main")]
#[case::partial_general(
    "sha1",
    "now",
    "now",
    b"[gc \"refs/heads/*\"]\nreflogExpire=never\n",
    "refs/heads/main"
)]
#[case::partial_unreachable(
    "sha1",
    "now",
    "now",
    b"[gc \"refs/*\"]\nreflogExpireUnreachable=never\n",
    "refs/heads/main"
)]
#[case::first_match("sha1", "now", "now", b"[gc \"refs/*\"]\nreflogExpire=never\n[gc \"refs/heads/*\"]\nreflogExpire=now\nreflogExpireUnreachable=now\n", "refs/heads/nested/topic")]
#[case::duplicate(
    "sha1",
    "now",
    "now",
    b"[gc \"refs/*\"]\nreflogExpire=now\nreflogExpire=never\n",
    "refs/heads/main"
)]
#[case::stash("sha1", "now", "now", b"", "refs/stash")]
#[case::false_value("sha1", "false", "false", b"", "refs/heads/main")]
fn configured_expiry_keeps_the_same_records_as_git(
    #[case] format: &str,
    #[case] general: &str,
    #[case] unreachable: &str,
    #[case] extra: &[u8],
    #[case] reference: &str,
) {
    let (root, _, first, second) = fixture_format(format);
    git(root.path(), &["config", "gc.reflogExpire", general], b"");
    git(
        root.path(),
        &["config", "gc.reflogExpireUnreachable", unreachable],
        b"",
    );
    fs::OpenOptions::new()
        .append(true)
        .open(root.path().join("config"))
        .unwrap()
        .write_all(extra)
        .unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "forward",
            "--create-reflog",
            reference,
            &second.to_string(),
        ],
        b"",
    );
    git(
        root.path(),
        &["update-ref", "-m", "back", reference, &first.to_string()],
        b"",
    );
    let log = root.path().join("logs").join(reference);
    let original = fs::read(&log).unwrap();
    git(root.path(), &["reflog", "expire", reference], b"");
    let expected = fs::read(&log).unwrap();
    fs::write(&log, original).unwrap();
    let repository = Repository::open(root.path()).unwrap();
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::Configured(
            girt::retention::ReflogExpiry::from_config(repository.config(), 1_800_000_000).unwrap(),
        ));
    repository
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(fs::read(log).unwrap(), expected);
    assert!(
        repository
            .loose_objects()
            .read_commit(first, usize::MAX)
            .is_ok()
    );
    assert!(
        repository
            .loose_objects()
            .read_commit(second, usize::MAX)
            .is_ok()
    );
}

#[cfg(unix)]
fn git_expire_with_sources(root: &Path, system: &Path, global: &Path) {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    let output = command
        .current_dir(root)
        .env("GIT_CONFIG_SYSTEM", system)
        .env("GIT_CONFIG_GLOBAL", global)
        .args(["reflog", "expire", "refs/heads/main"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[rstest::rstest]
#[case::global_first(
    b"[gc \"refs/*\"]\nreflogExpire=never\n[include]\npath=included\n",
    b"[gc \"refs/heads/*\"]\nreflogExpire=now\nreflogExpireUnreachable=now\n",
    b"[gc \"refs/heads/*\"]\nreflogExpire=now\nreflogExpireUnreachable=now\n"
)]
#[case::local_last_same_pattern(
    b"[gc \"refs/*\"]\nreflogExpire=now\nreflogExpireUnreachable=now\n[include]\npath=included\n",
    b"[gc \"refs/*\"]\nreflogExpire=never\n",
    b"[gc \"refs/*\"]\nreflogExpire=now\n"
)]
#[case::include_first(
    b"[include]\npath=included\n",
    b"[gc \"refs/*\"]\nreflogExpireUnreachable=never\n",
    b"[gc \"refs/heads/*\"]\nreflogExpire=now\nreflogExpireUnreachable=now\n"
)]
fn layered_expiry_keeps_the_same_records_as_git(
    #[case] global_bytes: &[u8],
    #[case] included_bytes: &[u8],
    #[case] local_bytes: &[u8],
) {
    use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
    let (root, _, first, second) = fixture_format("sha1");
    let system = root.path().join("system");
    let global = root.path().join("global");
    fs::write(
        &system,
        b"[gc]\nreflogExpire=now\nreflogExpireUnreachable=now\n",
    )
    .unwrap();
    fs::write(&global, global_bytes).unwrap();
    fs::write(root.path().join("included"), included_bytes).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(root.path().join("config"))
        .unwrap()
        .write_all(local_bytes)
        .unwrap();
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "forward",
            "--create-reflog",
            "refs/heads/main",
            &second.to_string(),
        ],
        b"",
    );
    git(
        root.path(),
        &[
            "update-ref",
            "-m",
            "back",
            "refs/heads/main",
            &first.to_string(),
        ],
        b"",
    );
    let log = root.path().join("logs/refs/heads/main");
    let original = fs::read(&log).unwrap();
    git_expire_with_sources(root.path(), &system, &global);
    let expected = fs::read(&log).unwrap();
    fs::write(&log, original).unwrap();
    // Caller order deliberately differs from scope order; resolution must still match Git.
    let inputs = ConfigInputs::default().with_files(vec![
        ConfigFile::new(global, ConfigScope::Global).with_optional(false),
        ConfigFile::new(system, ConfigScope::System).with_optional(false),
    ]);
    let repository = Repository::open_with_config(root.path(), &inputs).unwrap();
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() + Duration::from_secs(60))
        .with_reflog_expiry(girt::retention::ReflogExpiryPolicy::Configured(
            girt::retention::ReflogExpiry::from_config(repository.config(), 1_800_000_000).unwrap(),
        ));
    repository
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(fs::read(log).unwrap(), expected);
    assert!(
        repository
            .loose_objects()
            .read_commit(first, usize::MAX)
            .is_ok()
    );
    assert!(
        repository
            .loose_objects()
            .read_commit(second, usize::MAX)
            .is_ok()
    );
}

// Pattern policy disables both expiry rules here, despite the global expire-all setting.
#[cfg(unix)]
#[rstest::rstest]
#[case::sha1("sha1")]
#[case::sha256("sha256")]
fn configured_noop_expiry_does_not_audit_missing_history(#[case] format: &str) {
    let (root, _) = missing_packed_history(format);
    git(root.path(), &["config", "gc.reflogExpire", "now"], b"");
    git(
        root.path(),
        &["config", "gc.refs/heads/*.reflogExpire", "never"],
        b"",
    );
    let repo = Repository::open(root.path()).unwrap();
    let path = root.path().join("logs/refs/heads/main");
    let before = fs::read(&path).unwrap();
    git(root.path(), &["reflog", "expire", "refs/heads/main"], b"");
    assert_eq!(fs::read(&path).unwrap(), before);
    let policy = RetentionPolicy::default().with_reflog_expiry(
        girt::retention::ReflogExpiryPolicy::Configured(
            girt::retention::ReflogExpiry::from_config(repo.config(), 1_800_000_000).unwrap(),
        ),
    );
    let report = repo
        .expire_reflogs(&mut FixtureIsolation, &policy, &AtomicBool::new(false))
        .unwrap();
    assert!(report.changed.is_empty());
    assert_eq!(fs::read(path).unwrap(), before);
}
