//! Retention planning in partial clones made by Git, and the maintenance that still refuses them.
#![cfg(unix)]
#[path = "support/apply_git.rs"]
mod support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::retention::{
    MaintenanceCause, PackConsolidation, PartialClonePolicy, PruneCause, RepackError, RepackLimits,
    RetentionError, RetentionErrorKind, RetentionOutcome, RetentionPlan, RetentionPolicy,
    RetireCause,
};
use girt::{ObjectFormat, ObjectId, ObjectKind, Repository};
use rstest::rstest;
use support::git;

struct PartialClone {
    _root: tempfile::TempDir,
    clone: PathBuf,
}

fn oid(bytes: &[u8]) -> ObjectId {
    std::str::from_utf8(bytes).unwrap().trim().parse().unwrap()
}

/// Two commits with a nested file, cloned over `file://` with `--filter=blob:none`, so every
/// blob is promised rather than stored.
fn partial_clone(format: ObjectFormat) -> PartialClone {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    std::fs::create_dir(&source).unwrap();
    git(
        &source,
        &[
            "init",
            "-q",
            "--template=",
            "-b",
            "main",
            &format!("--object-format={format}"),
        ],
        b"",
    );
    git(&source, &["config", "uploadpack.allowFilter", "true"], b"");
    std::fs::create_dir(source.join("d")).unwrap();
    std::fs::write(source.join("a"), "a\n").unwrap();
    std::fs::write(source.join("d/b"), "b\n").unwrap();
    git(&source, &["add", "."], b"");
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-m", "one"],
        b"",
    );
    std::fs::write(source.join("a"), "c\n").unwrap();
    git(
        &source,
        &["commit", "-q", "--no-gpg-sign", "-am", "two"],
        b"",
    );
    let clone = root.path().join("clone");
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--template=",
            "--no-local",
            "--filter=blob:none",
            "--no-checkout",
            &format!("file://{}", source.display()),
            clone.to_str().unwrap(),
        ],
        b"",
    );
    PartialClone { _root: root, clone }
}

/// Runs Git under the same isolation as `support::git_output`, never fetching a missing object.
fn git_offline(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut command = std::process::Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-config"))
        .env("GIT_NO_LAZY_FETCH", "1")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

/// The `?` lines of `git rev-list --objects --all --missing=print`.
fn git_missing(dir: &Path) -> BTreeSet<ObjectId> {
    let output = git_offline(dir, &["rev-list", "--objects", "--all", "--missing=print"]);
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix('?'))
        .map(|id| id.parse().unwrap())
        .collect()
}

fn pack_files(clone: &Path, extension: &str) -> Vec<PathBuf> {
    std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == extension))
        .collect()
}

fn promised() -> RetentionPolicy {
    RetentionPolicy::default().with_partial_clone(PartialClonePolicy::Promised)
}

fn plan(clone: &Path, policy: &RetentionPolicy) -> RetentionPlan {
    Repository::open(clone)
        .unwrap()
        .plan_retention(policy, &AtomicBool::new(false))
}

fn incomplete(plan: &RetentionPlan) -> &RetentionError {
    match &plan.outcome {
        RetentionOutcome::Incomplete(error) => error,
        outcome => panic!("expected an incomplete plan, got {outcome:?}"),
    }
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn default_policy_refuses_a_partial_clone(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let plan = plan(&partial.clone, &RetentionPolicy::default());
    assert!(matches!(incomplete(&plan), RetentionError::PartialClone));
    let refuse = RetentionPolicy::default().with_partial_clone(PartialClonePolicy::Refuse);
    let plan = self::plan(&partial.clone, &refuse);
    assert!(matches!(incomplete(&plan), RetentionError::PartialClone));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn promised_plan_stops_at_the_objects_git_reports_missing(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let expected = git_missing(&partial.clone);
    assert_eq!(expected.len(), 3);

    let plan = plan(&partial.clone, &promised());
    assert!(plan.is_complete(), "{:?}", plan.outcome);
    let boundaries: BTreeMap<_, _> = expected.iter().map(|&id| (id, ObjectKind::Blob)).collect();
    assert_eq!(plan.promised_boundaries, boundaries);
    // Two commits, two root trees and one shared subtree; no promised blob is reachable.
    assert_eq!(plan.reachable.len(), 5);
    assert!(expected.iter().all(|id| !plan.reachable.contains(id)));
    assert!(expected.iter().all(|id| !plan.required.contains(id)));
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn missing_objects_without_a_promise_stay_missing(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let expected = git_missing(&partial.clone);
    for marker in pack_files(&partial.clone, "promisor") {
        std::fs::remove_file(marker).unwrap();
    }
    assert!(
        !git_offline(
            &partial.clone,
            &["rev-list", "--objects", "--all", "--missing=allow-promisor"],
        )
        .status
        .success()
    );
    let plan = plan(&partial.clone, &promised());
    assert!(
        matches!(incomplete(&plan), RetentionError::MissingObject(id) if expected.contains(id))
    );
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn unpromised_blob_beside_promised_ones_stays_missing(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let absent = ObjectId::for_blob(format, b"never stored\n");
    let tree = git(
        &partial.clone,
        &["mktree", "--missing"],
        format!("100644 blob {absent}\tz\n").as_bytes(),
    );
    let tree = std::str::from_utf8(&tree).unwrap().trim().to_owned();
    let commit = oid(&git(
        &partial.clone,
        &["commit-tree", "--no-gpg-sign", &tree, "-m", "x"],
        b"",
    ));
    git(
        &partial.clone,
        &["update-ref", "refs/heads/x", &commit.to_string()],
        b"",
    );

    let plan = plan(&partial.clone, &promised());
    assert!(matches!(incomplete(&plan), RetentionError::MissingObject(id) if *id == absent));
}

/// A commit in a promisor pack promises its parent, but Git still refuses a missing parent.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn promised_but_missing_parent_commit_is_an_error(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let parent = ObjectId::for_blob(format, b"a parent commit that was never fetched\n");
    let tree = git(&partial.clone, &["rev-parse", "HEAD^{tree}"], b"");
    let tree = std::str::from_utf8(&tree).unwrap().trim();
    let body = format!(
        "tree {tree}\nparent {parent}\nauthor A <a@example.com> 1 +0000\n\
         committer A <a@example.com> 1 +0000\n\norphan\n"
    );
    let commit = oid(&git(
        &partial.clone,
        &[
            "hash-object",
            "-t",
            "commit",
            "-w",
            "--literally",
            "--stdin",
        ],
        body.as_bytes(),
    ));
    let pack = git(
        &partial.clone,
        &["pack-objects", "-q", ".git/objects/pack/pack"],
        format!("{commit}\n").as_bytes(),
    );
    let pack = std::str::from_utf8(&pack).unwrap().trim();
    std::fs::write(
        partial
            .clone
            .join(format!(".git/objects/pack/pack-{pack}.promisor")),
        b"",
    )
    .unwrap();
    git(
        &partial.clone,
        &["update-ref", "refs/heads/orphan", &commit.to_string()],
        b"",
    );
    assert!(
        !git_offline(
            &partial.clone,
            &["rev-list", "--objects", "--all", "--missing=allow-promisor"],
        )
        .status
        .success()
    );

    let plan = plan(&partial.clone, &promised());
    assert!(matches!(incomplete(&plan), RetentionError::MissingObject(id) if *id == parent));
}

/// Without partial-clone configuration, Git ignores `.promisor` files, and so does planning.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn promisor_packs_without_partial_clone_config_are_refused(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let expected = git_missing(&partial.clone);
    git(
        &partial.clone,
        &["config", "--unset", "remote.origin.promisor"],
        b"",
    );
    git(
        &partial.clone,
        &["config", "--unset", "remote.origin.partialclonefilter"],
        b"",
    );
    assert!(!pack_files(&partial.clone, "promisor").is_empty());
    assert!(!Repository::open(&partial.clone).unwrap().is_partial_clone());

    for policy in [RetentionPolicy::default(), promised()] {
        let plan = plan(&partial.clone, &policy);
        assert!(
            matches!(incomplete(&plan), RetentionError::MissingObject(id) if expected.contains(id))
        );
        assert!(plan.promised_boundaries.is_empty());
    }
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn promised_boundaries_are_bounded(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let plan = plan(&partial.clone, &promised().with_max_promised_boundaries(3));
    assert!(plan.is_complete(), "{:?}", plan.outcome);

    let plan = self::plan(&partial.clone, &promised().with_max_promised_boundaries(2));
    let error = incomplete(&plan);
    assert!(matches!(error, RetentionError::PromisedBoundaryLimit));
    assert_eq!(error.kind(), RetentionErrorKind::Limit);
}

#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn promised_plan_is_cancellable(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let repository = Repository::open(&partial.clone).unwrap();
    let plan = repository.plan_retention(&promised(), &AtomicBool::new(true));
    assert!(!plan.is_complete());
    assert!(plan.promised_boundaries.is_empty());
}

struct PrivateFixture;

impl girt::retention::MaintenanceIsolation for PrivateFixture {
    type Guard = ();

    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every path under `.git/objects` and its bytes.
fn objects_snapshot(clone: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![clone.join(".git/objects")];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.insert(path.clone(), Vec::new());
                pending.push(path);
            } else {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    files
}

fn is_partial_clone_refusal(error: &RetentionError) -> bool {
    matches!(error, RetentionError::PartialClone)
}

fn maintenance_refused(cause: &MaintenanceCause) -> bool {
    matches!(cause, MaintenanceCause::Incomplete(error) if is_partial_clone_refusal(error))
}

/// A complete promised plan never authorizes maintenance that changes storage.
#[rstest]
#[case::sha1(ObjectFormat::Sha1)]
#[case::sha256(ObjectFormat::Sha256)]
fn maintenance_refuses_a_partial_clone_under_the_promised_policy(#[case] format: ObjectFormat) {
    let partial = partial_clone(format);
    let repository = Repository::open(&partial.clone).unwrap();
    // An old, unreachable loose blob and an old pack that pruning and retirement could remove.
    let garbage = repository
        .loose_objects()
        .write_blob(b"old and unreachable\n")
        .unwrap();
    let hex = garbage.to_string();
    let path = partial
        .clone
        .join(".git/objects")
        .join(&hex[..2])
        .join(&hex[2..]);
    let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(old)
        .unwrap();
    for pack in pack_files(&partial.clone, "pack") {
        std::fs::File::open(&pack)
            .unwrap()
            .set_modified(old)
            .unwrap();
    }
    let policy = promised()
        .with_recent_cutoff(SystemTime::UNIX_EPOCH + Duration::from_secs(2))
        .with_pack_consolidation(PackConsolidation::Always);
    let repository = Repository::open(&partial.clone).unwrap();
    assert!(
        repository
            .plan_retention(&policy, &AtomicBool::new(false))
            .is_complete()
    );
    let before = objects_snapshot(&partial.clone);
    let cancel = AtomicBool::new(false);

    let repacked = repository.repack_retained(&policy, RepackLimits::default(), &cancel);
    assert!(
        matches!(&repacked, Err(RepackError::Incomplete(error)) if is_partial_clone_refusal(error)),
        "{repacked:?}"
    );
    let retired = repository.retire_old_packs(
        &mut PrivateFixture,
        &policy,
        RepackLimits::default(),
        &cancel,
    );
    assert!(
        matches!(&retired, Err(failure) if matches!(
            &failure.cause,
            RetireCause::Incomplete(error) if is_partial_clone_refusal(error)
        )),
        "{retired:?}"
    );
    let pruned = repository.prune_unreachable_loose(&mut PrivateFixture, &policy, &cancel);
    assert!(
        matches!(&pruned, Err(failure) if matches!(
            &failure.cause,
            PruneCause::Incomplete(error) if is_partial_clone_refusal(error)
        )),
        "{pruned:?}"
    );
    let maintained = repository.run_maintenance(
        &mut PrivateFixture,
        &policy,
        RepackLimits::default(),
        &cancel,
    );
    assert!(
        matches!(&maintained, Err(failure) if maintenance_refused(&failure.cause)),
        "{maintained:?}"
    );
    let collected = repository.collect_garbage(&mut PrivateFixture, &policy, &cancel);
    assert!(
        matches!(&collected, Err(failure) if maintenance_refused(&failure.cause)),
        "{collected:?}"
    );
    let consolidated = repository.collect_garbage_with(
        &mut PrivateFixture,
        &policy,
        RepackLimits::default(),
        &cancel,
    );
    assert!(
        matches!(&consolidated, Err(failure) if maintenance_refused(&failure.cause)),
        "{consolidated:?}"
    );
    let checkpointed =
        repository.collect_garbage_with_checkpoint(&mut PrivateFixture, &policy, &cancel, || {
            panic!("a partial clone is refused before planning completes")
        });
    assert!(
        matches!(&checkpointed, Err(failure) if maintenance_refused(&failure.cause)),
        "{checkpointed:?}"
    );

    assert_eq!(objects_snapshot(&partial.clone), before);
    assert!(path.exists());
}
