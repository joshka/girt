//! Pack consolidation during explicit composed maintenance.
//!
//! Git generates every fixture, reads every result, and is the oracle for which objects
//! `git gc` keeps.
#![cfg(unix)]
use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use girt::Repository;
use girt::retention::{
    MaintenanceCause, MaintenanceIsolation, PackConsolidation, RepackLimits, RetentionPolicy,
};

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
        .env("GIT_COMMITTER_NAME", "A")
        .env("GIT_COMMITTER_EMAIL", "a@example.com")
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

struct Exclusive;
impl MaintenanceIsolation for Exclusive {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}

/// A bare repository whose `main` history has one pack per commit, as repeated fetches leave it.
fn fetched_packs(commits: usize) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "--bare", "--template=", "."], b"");
    let mut parent: Option<String> = None;
    for number in 0..commits {
        let blob = git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            format!("content {number}\n").as_bytes(),
        );
        let tree = git(
            root.path(),
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        );
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(parent) = &parent {
            args.extend(["-p", parent.as_str()]);
        }
        let commit = git(root.path(), &args, format!("commit {number}\n").as_bytes());
        git(
            root.path(),
            &["update-ref", "refs/heads/main", &commit],
            b"",
        );
        git(
            root.path(),
            &[
                "pack-objects",
                "-q",
                "--revs",
                "--incremental",
                "objects/pack/pack",
            ],
            b"refs/heads/main\n",
        );
        git(root.path(), &["prune-packed"], b"");
        parent = Some(commit);
    }
    root
}

/// Packs one new unreachable blob alone and returns its pack path and identity.
fn unreachable_pack(root: &Path, content: &[u8]) -> (PathBuf, String) {
    let blob = git(root, &["hash-object", "-w", "--stdin"], content);
    let checksum = git(
        root,
        &["pack-objects", "-q", "objects/pack/pack"],
        format!("{blob}\n").as_bytes(),
    );
    git(root, &["prune-packed"], b"");
    (
        root.join(format!("objects/pack/pack-{checksum}.pack")),
        blob,
    )
}

fn objects(root: &Path) -> BTreeSet<String> {
    git(
        root,
        &[
            "cat-file",
            "--batch-all-objects",
            "--batch-check=%(objectname)",
        ],
        b"",
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

fn packs(root: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<_> = fs::read_dir(root.join("objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .collect();
    paths.sort();
    paths
}

fn pack_objects(pack: &Path) -> BTreeSet<String> {
    let root = pack.parent().unwrap();
    git(
        root,
        &["show-index"],
        &fs::read(pack.with_extension("idx")).unwrap(),
    )
    .lines()
    .map(|line| line.split_whitespace().nth(1).unwrap().to_owned())
    .collect()
}

fn set_limit(root: &Path, limit: &str) {
    git(root, &["config", "gc.autoPackLimit", limit], b"");
}

fn age(path: &Path, by: Duration) {
    fs::File::open(path)
        .unwrap()
        .set_modified(SystemTime::now() - by)
        .unwrap();
}

fn collect(root: &Path, policy: &RetentionPolicy) -> girt::retention::MaintenanceReport {
    Repository::open(root)
        .unwrap()
        .collect_garbage_with(
            &mut Exclusive,
            policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
}

#[test]
fn packs_above_the_configured_limit_become_one_pack_with_every_object() {
    let root = fetched_packs(4);
    let (_, unreachable) = unreachable_pack(root.path(), b"unreachable but recent\n");
    set_limit(root.path(), "3");
    let before = objects(root.path());
    assert_eq!(packs(root.path()).len(), 5);

    let report = collect(root.path(), &RetentionPolicy::default());

    assert!(report.consolidated);
    let removed = report.retired.unwrap().removed;
    let removed_packs = removed
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"));
    assert_eq!(removed_packs.count(), 5);
    assert_eq!(packs(root.path()).len(), 1);
    // The default policy has no cutoff, so even unreachable packed objects survive.
    assert_eq!(objects(root.path()), before);
    assert!(before.contains(&unreachable));
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

// `gc.autoPackLimit` counts more than the limit, as `git gc --auto` does.
#[rstest::rstest]
#[case::at_limit("4")]
#[case::disabled("0")]
#[case::negative("-1")]
fn packs_at_or_below_the_limit_stay_untouched(#[case] limit: &str) {
    let root = fetched_packs(4);
    set_limit(root.path(), limit);
    let before: Vec<_> = packs(root.path())
        .into_iter()
        .map(|path| (fs::read(&path).unwrap(), path))
        .collect();

    let report = collect(root.path(), &RetentionPolicy::default());

    assert!(!report.consolidated);
    assert!(report.retired.is_none());
    let after: Vec<_> = packs(root.path())
        .into_iter()
        .map(|path| (fs::read(&path).unwrap(), path))
        .collect();
    assert_eq!(after, before);
}

// Each configured limit alone would select the opposite of the explicit choice.
#[rstest::rstest]
#[case::always(PackConsolidation::Always, "0", 1)]
#[case::never(PackConsolidation::Never, "1", 2)]
fn explicit_choice_overrides_configuration(
    #[case] choice: PackConsolidation,
    #[case] limit: &str,
    #[case] remaining: usize,
) {
    let root = fetched_packs(2);
    set_limit(root.path(), limit);

    let report = collect(
        root.path(),
        &RetentionPolicy::default().with_pack_consolidation(choice),
    );

    assert_eq!(report.consolidated, choice == PackConsolidation::Always);
    assert_eq!(packs(root.path()).len(), remaining);
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

#[test]
fn keep_pack_stays_byte_for_byte_and_is_not_copied() {
    let root = fetched_packs(4);
    let keep = packs(root.path())[0].clone();
    fs::write(keep.with_extension("keep"), b"").unwrap();
    let kept_bytes = fs::read(&keep).unwrap();
    let kept_objects = pack_objects(&keep);
    let before = objects(root.path());

    let report = collect(
        root.path(),
        &RetentionPolicy::default().with_pack_consolidation(PackConsolidation::Always),
    );

    assert!(report.consolidated);
    let after = packs(root.path());
    assert_eq!(after.len(), 2);
    assert_eq!(fs::read(&keep).unwrap(), kept_bytes);
    let replacement = after.iter().find(|path| **path != keep).unwrap();
    assert!(pack_objects(replacement).is_disjoint(&kept_objects));
    assert_eq!(objects(root.path()), before);
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

// Git counts promisor packs toward the limit; consolidation leaves them and their objects alone.
#[test]
fn promisor_pack_counts_but_stays_in_place() {
    let root = fetched_packs(4);
    let promisor = packs(root.path())[0].clone();
    fs::write(promisor.with_extension("promisor"), b"").unwrap();
    let promisor_bytes = fs::read(&promisor).unwrap();
    let promised = pack_objects(&promisor);
    set_limit(root.path(), "3");

    let report = collect(root.path(), &RetentionPolicy::default());

    assert!(report.consolidated);
    let after = packs(root.path());
    assert_eq!(after.len(), 2);
    assert_eq!(fs::read(&promisor).unwrap(), promisor_bytes);
    let replacement = after.iter().find(|path| **path != promisor).unwrap();
    assert!(pack_objects(replacement).is_disjoint(&promised));
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

// A `.promisor` pack in a repository without partial-clone configuration is never retired, nor is
// its marker removed, even when it is old: configuration can return later, and the marker keeps the
// repository recoverable as a partial clone.
#[rstest::rstest]
#[case::consolidating(PackConsolidation::Always)]
#[case::retiring_only(PackConsolidation::Never)]
fn old_promisor_pack_and_marker_survive_gc(#[case] consolidation: PackConsolidation) {
    let root = fetched_packs(4);
    let promisor = packs(root.path())[0].clone();
    let marker = promisor.with_extension("promisor");
    fs::write(&marker, b"").unwrap();
    let promisor_bytes = fs::read(&promisor).unwrap();
    let index_bytes = fs::read(promisor.with_extension("idx")).unwrap();
    age_packs(root.path(), Duration::from_secs(10 * 86400));
    assert!(!Repository::open(root.path()).unwrap().is_partial_clone());

    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() - Duration::from_secs(5 * 86400))
        .with_pack_consolidation(consolidation);
    collect(root.path(), &policy);

    assert_eq!(fs::read(&promisor).unwrap(), promisor_bytes);
    assert_eq!(
        fs::read(promisor.with_extension("idx")).unwrap(),
        index_bytes
    );
    assert_eq!(fs::read(&marker).unwrap(), b"");
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

// With a cutoff, unreachable objects in old packs go and those in recent packs stay, matching
// the objects `git gc --prune=<cutoff>` keeps from the same layout.
#[test]
fn retained_objects_match_git_gc_with_a_cutoff() {
    let root = fetched_packs(4);
    let (old_pack, old_unreachable) = unreachable_pack(root.path(), b"old and unreachable\n");
    let (_, recent_unreachable) = unreachable_pack(root.path(), b"recent and unreachable\n");
    for pack in packs(root.path()) {
        if pack != old_pack {
            age(&pack, Duration::from_secs(2 * 86400));
        }
    }
    age(&old_pack, Duration::from_secs(10 * 86400));
    let oracle = tempfile::tempdir().unwrap();
    git(
        root.path(),
        &[
            "clone",
            "-q",
            "--bare",
            "--no-local",
            ".",
            oracle.path().to_str().unwrap(),
        ],
        b"",
    );
    fs::remove_dir_all(oracle.path().join("objects")).unwrap();
    copy_dir(&root.path().join("objects"), &oracle.path().join("objects"));
    git(oracle.path(), &["gc", "-q", "--prune=5.days.ago"], b"");

    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() - Duration::from_secs(5 * 86400))
        .with_pack_consolidation(PackConsolidation::Always);
    let report = collect(root.path(), &policy);

    assert!(report.consolidated);
    let kept = objects(root.path());
    assert!(!kept.contains(&old_unreachable));
    assert!(kept.contains(&recent_unreachable));
    assert_eq!(kept, objects(oracle.path()));
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).unwrap();
            let modified = entry.metadata().unwrap().modified().unwrap();
            fs::File::open(&target)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
    }
}

#[test]
fn malformed_limit_refuses_before_any_phase() {
    let root = fetched_packs(2);
    set_limit(root.path(), "many");
    let before = packs(root.path());

    let failure = Repository::open(root.path())
        .unwrap()
        .collect_garbage(
            &mut Exclusive,
            &RetentionPolicy::default(),
            &AtomicBool::new(false),
        )
        .unwrap_err();

    assert!(matches!(failure.cause, MaintenanceCause::Config(_)));
    assert!(failure.report.expired.is_none());
    assert!(failure.report.retired.is_none());
    assert_eq!(packs(root.path()), before);
}

// `run_maintenance` always retires old packs; consolidation additionally folds recent ones.
#[rstest::rstest]
#[case::consolidating(PackConsolidation::Always, 1)]
#[case::retiring_only(PackConsolidation::Never, 3)]
fn run_maintenance_folds_recent_packs_only_when_consolidating(
    #[case] choice: PackConsolidation,
    #[case] remaining: usize,
) {
    let root = fetched_packs(3);
    let before = objects(root.path());

    let report = Repository::open(root.path())
        .unwrap()
        .run_maintenance(
            &mut Exclusive,
            &RetentionPolicy::default().with_pack_consolidation(choice),
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();

    assert_eq!(report.consolidated, choice == PackConsolidation::Always);
    assert_eq!(packs(root.path()).len(), remaining);
    assert_eq!(objects(root.path()), before);
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

// A recent pack's unreachable commit keeps its parent and tree even when those live only in an
// old pack that consolidation retires: the folded pack's closure stays required.
#[test]
fn recent_unreachable_commit_keeps_its_old_dependencies() {
    let root = fetched_packs(2);
    let blob = git(
        root.path(),
        &["hash-object", "-w", "--stdin"],
        b"side content\n",
    );
    let tree = git(
        root.path(),
        &["mktree"],
        format!("100644 blob {blob}\tside\n").as_bytes(),
    );
    let parent = git(root.path(), &["commit-tree", &tree], b"old side\n");
    git(
        root.path(),
        &["pack-objects", "-q", "objects/pack/pack"],
        format!("{blob}\n{tree}\n{parent}\n").as_bytes(),
    );
    age_packs(root.path(), Duration::from_secs(10 * 86400));
    let child = git(
        root.path(),
        &["commit-tree", &tree, "-p", &parent],
        b"recent side\n",
    );
    git(
        root.path(),
        &["pack-objects", "-q", "objects/pack/pack"],
        format!("{child}\n").as_bytes(),
    );
    git(root.path(), &["prune-packed"], b"");

    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() - Duration::from_secs(5 * 86400))
        .with_pack_consolidation(PackConsolidation::Always);
    let report = collect(root.path(), &policy);

    assert!(report.consolidated);
    assert_eq!(packs(root.path()).len(), 1);
    let required: BTreeSet<_> = [child, parent, tree, blob].into_iter().collect();
    assert!(objects(root.path()).is_superset(&required));
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

fn age_packs(root: &Path, by: Duration) {
    packs(root).iter().for_each(|pack| age(pack, by));
}

/// Consolidation never folds a promisor pack's objects into an ordinary pack: after consolidating
/// old packs, every object that was only in the promisor pack is still only in a marked pack.
#[test]
fn consolidation_never_merges_promisor_and_ordinary_packs() {
    let root = fetched_packs(4);
    let promisor = packs(root.path())[1].clone();
    fs::write(promisor.with_extension("promisor"), b"").unwrap();
    let promised = pack_objects(&promisor);
    age_packs(root.path(), Duration::from_secs(10 * 86400));
    let policy = RetentionPolicy::default()
        .with_recent_cutoff(SystemTime::now() - Duration::from_secs(5 * 86400))
        .with_pack_consolidation(PackConsolidation::Always);

    let report = collect(root.path(), &policy);

    assert!(report.consolidated);
    for pack in packs(root.path()) {
        let marked = pack.with_extension("promisor").exists();
        let held = pack_objects(&pack);
        if marked {
            assert_eq!(pack, promisor);
            assert_eq!(held, promised);
        } else {
            assert!(
                held.is_disjoint(&promised),
                "{} holds promisor objects",
                pack.display()
            );
        }
    }
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}

/// Without partial-clone configuration the isolated repack retires nothing, so an old `.promisor`
/// pack and its marker stay byte for byte.
#[test]
fn isolated_repack_never_retires_promisor_packs_without_partial_clone_config() {
    let root = fetched_packs(3);
    let promisor = packs(root.path())[0].clone();
    let marker = promisor.with_extension("promisor");
    fs::write(&marker, b"").unwrap();
    let promisor_bytes = fs::read(&promisor).unwrap();
    age_packs(root.path(), Duration::from_secs(10 * 86400));
    let repository = Repository::open(root.path()).unwrap();
    assert!(!repository.is_partial_clone());
    let policy = RetentionPolicy::default()
        .with_partial_clone(girt::retention::PartialClonePolicy::Promised)
        .with_recent_cutoff(SystemTime::now() - Duration::from_secs(5 * 86400));

    let report = repository
        .repack_retained_isolated(
            &mut Exclusive,
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
        )
        .unwrap();

    assert!(report.promisor.is_none());
    assert!(report.ordinary.is_some());
    assert!(report.removed.is_empty() && report.retained.is_empty());
    assert_eq!(fs::read(&promisor).unwrap(), promisor_bytes);
    assert_eq!(fs::read(&marker).unwrap(), b"");
    git(root.path(), &["fsck", "--full", "--strict"], b"");
}
