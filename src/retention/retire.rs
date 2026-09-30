#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::fs::{self, File};
use std::io;
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::time::SystemTime;

use super::{MaintenanceIsolation, RepackError, RepackLimits, RepackPublished, RetentionPolicy};
#[cfg(unix)]
use super::{RetentionOutcome, RetentionPlan};
#[cfg(unix)]
use crate::pack::FilePack;
use crate::{ObjectId, ObjectReadError, Repository};

/// Additive publication and known old-artifact removals from one isolated attempt.
#[derive(Clone, Debug, Default)]
pub struct RetireReport {
    /// Replacement pair visibly published by R32; later synchronization or verification may fail.
    pub published: Option<RepackPublished>,
    /// Both pack and object directories synchronized after publication.
    pub durable: bool,
    /// Old index and pack paths whose unlink calls reported success, in execution order.
    pub removed: Vec<PathBuf>,
}

/// Why old pack retirement stopped.
#[derive(Debug, thiserror::Error)]
pub enum RetireCause {
    /// The caller could not exclude writers, old readers or alternate dependents.
    #[error("maintenance isolation unavailable: {0}")]
    Isolation(#[source] io::Error),
    /// Directory durability is unavailable for this platform.
    #[error("pack retirement is unsupported on this platform")]
    UnsupportedPlatform,
    /// A bounded root or artifact observation was incomplete.
    #[error("retention scan incomplete: {0}")]
    Incomplete(String),
    /// Roots, shallow state or an old pack generation changed before deletion.
    #[error("maintenance generation changed")]
    Changed,
    /// Additive replacement publication failed without old-pack deletion.
    #[error(transparent)]
    Repack(Box<RepackError>),
    /// The replacement does not contain a retained object.
    #[error("replacement pack missing retained object {0}")]
    ReplacementMissing(ObjectId),
    /// The replacement could not be verified.
    #[error(transparent)]
    Read(Box<ObjectReadError>),
    /// Cancellation was observed between publication or pair-retirement steps.
    #[error("pack retirement cancelled")]
    Cancelled,
    /// An artifact or directory operation failed.
    #[error("pack retirement I/O: {0}")]
    Io(#[source] io::Error),
}

/// Failure with the known successful effects and any uncertain artifact.
#[derive(Debug, thiserror::Error)]
#[error("{cause}")]
pub struct RetireFailure {
    /// Failure class.
    #[source]
    pub cause: RetireCause,
    /// Visible publication, its durability state, and old paths whose unlink returned success.
    pub report: Box<RetireReport>,
    /// Artifact at a failed unlink or directory-sync boundary, if effects are uncertain.
    pub uncertain: Option<PathBuf>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stamp {
    len: u64,
    modified: SystemTime,
}

impl Repository {
    /// Publishes every currently retained object and retires unprotected old pack pairs.
    ///
    /// The caller's [`MaintenanceIsolation`] guard must exclude external Git and girt writers,
    /// old readers pinned to packs, and repositories depending on this store as an alternate for
    /// the whole operation. The operation ignores reflog expiry cutoffs: every observed history
    /// remains a root. It makes fresh complete scans, publishes a bounded replacement pair,
    /// synchronizes the pack and object directories, verifies each retained ID through that pair,
    /// and compares
    /// old pack generations before deleting an old index followed by its pack. `.keep`, recent
    /// packs and packs with unsupported sidecars remain untouched. Optional `.rev` and `.bitmap`
    /// sidecars are retired with their pair. Alternate stores are never
    /// mutated. A replacement identical to an old pair is retained. On non-Unix platforms the
    /// operation refuses before publication because directory durability is not established.
    ///
    /// An old pair is removed only after its retained objects are present in a verified durable
    /// replacement. Failure after publication can leave the new pair and a prefix of old paths
    /// removed. An index-only removal leaves an unindexed old pack; readers ignore it. Cancellation
    /// is checked before each pair, not between its two unlinks. A failed unlink or directory
    /// synchronization names its uncertain path. Retry only after a fresh scan.
    ///
    /// # Errors
    ///
    /// Isolation refusal, unsupported platform, incomplete or changed observations, publication,
    /// verification, cancellation and I/O report known effects and an uncertain boundary if any.
    pub fn retire_old_packs<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<RetireReport, RetireFailure> {
        check(cancel, &RetireReport::default())?;
        let _guard = isolation
            .acquire(self)
            .map_err(|error| failed(RetireCause::Isolation(error), RetireReport::default()))?;
        #[cfg(not(unix))]
        {
            let _ = (policy, limits);
            Err(failed(
                RetireCause::UnsupportedPlatform,
                RetireReport::default(),
            ))
        }
        #[cfg(unix)]
        self.retire_old_packs_exclusive(policy, limits, cancel)
    }

    #[cfg(unix)]
    pub(super) fn retire_old_packs_exclusive(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
    ) -> Result<RetireReport, RetireFailure> {
        self.retire_old_packs_at(
            policy,
            limits,
            cancel,
            || {},
            |path| fs::remove_file(path),
            |directory| File::open(directory).and_then(|dir| dir.sync_all()),
        )
    }

    // Checkpoints keep writer races and unlink faults deterministic in local tests.
    #[cfg(unix)]
    fn retire_old_packs_at(
        &self,
        policy: &RetentionPolicy,
        limits: RepackLimits,
        cancel: &AtomicBool,
        after_publication: impl FnOnce(),
        mut remove: impl FnMut(&Path) -> io::Result<()>,
        mut sync_directory: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<RetireReport, RetireFailure> {
        let mut report = RetireReport::default();
        check(cancel, &report)?;
        let mut keep_all_logs = policy.clone();
        keep_all_logs.reflog_expire_before = None;
        keep_all_logs.reflog_expire_unreachable_before = None;
        let current = Repository::open(self.git_dir())
            .map_err(|error| failed(RetireCause::Incomplete(error.to_string()), report.clone()))?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(failed(RetireCause::Changed, report));
        }
        let first = complete_plan(&current, &keep_all_logs, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        let directory = current.object_dir().join("pack");
        let before = snapshot(&directory, policy.max_entries, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        let published = current
            .repack_retained(&keep_all_logs, limits, cancel)
            .map_err(|error| failed(RetireCause::Repack(Box::new(error)), report.clone()))?;
        let replacement = directory.join(format!("pack-{}", published.written.checksum));
        let replacement_pack = replacement.with_extension("pack");
        let replacement_index = replacement.with_extension("idx");
        report.published = Some(published);
        sync_directory(&directory)
            .map_err(|error| failed(RetireCause::Io(error), report.clone()))?;
        sync_directory(current.object_dir())
            .map_err(|error| failed(RetireCause::Io(error), report.clone()))?;
        report.durable = true;
        after_publication();
        let refreshed = Repository::open(self.git_dir())
            .map_err(|error| failed(RetireCause::Incomplete(error.to_string()), report.clone()))?;
        if refreshed.object_dir() != current.object_dir()
            || refreshed.object_format() != current.object_format()
            || !refreshed
                .shallow_roots()
                .iter()
                .eq(current.shallow_roots().iter())
        {
            return Err(failed(RetireCause::Changed, report));
        }
        let second = complete_plan(&refreshed, &keep_all_logs, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        if first.roots != second.roots
            || first.reachable != second.reachable
            || first.required != second.required
            || first.alternate_stores != second.alternate_stores
        {
            return Err(failed(RetireCause::Changed, report));
        }
        let mut pack_budget = policy.packs;
        let pack = FilePack::open(
            self.object_format(),
            &replacement_index,
            &mut pack_budget,
            cancel,
        )
        .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?;
        for id in &first.required {
            check(cancel, &report)?;
            let position = pack
                .find(*id)
                .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?
                .ok_or_else(|| failed(RetireCause::ReplacementMissing(*id), report.clone()))?;
            let object = pack
                .read(position, policy.read, cancel)
                .map_err(|error| failed(RetireCause::Read(Box::new(error)), report.clone()))?;
            if object.id() != *id {
                return Err(failed(RetireCause::ReplacementMissing(*id), report));
            }
        }
        drop(pack);
        let after = snapshot(&directory, policy.max_entries, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        if !same_old_generation(&before, &after, &replacement_pack, &replacement_index) {
            return Err(failed(RetireCause::Changed, report));
        }
        let retiring = old_indices(&before, &first, &replacement_pack);
        if !retiring.is_empty() {
            // A multi-pack index (and its bitmaps) naming a retired pack would make the object
            // store unreadable to Git; Git rebuilds it when needed, as `git repack -d` does.
            for path in multi_pack_index_paths(&directory)
                .map_err(|error| failed(RetireCause::Io(error), report.clone()))?
            {
                let result = if path.is_dir() {
                    fs::remove_dir_all(&path)
                } else {
                    remove(&path)
                };
                result.map_err(|error| RetireFailure {
                    cause: RetireCause::Io(error),
                    report: Box::new(report.clone()),
                    uncertain: Some(path.clone()),
                })?;
                report.removed.push(path);
            }
        }
        for index in retiring {
            check(cancel, &report)?;
            let old_pack = index.with_extension("pack");
            let artifacts = retirable_paths(&index, &before);
            for path in &artifacts {
                recheck(path, before[path]).map_err(|cause| failed(cause, report.clone()))?;
            }
            for path in artifacts {
                remove(&path).map_err(|error| RetireFailure {
                    cause: RetireCause::Io(error),
                    report: Box::new(report.clone()),
                    uncertain: Some(path.clone()),
                })?;
                report.removed.push(path);
            }
            sync_directory(&directory).map_err(|error| RetireFailure {
                cause: RetireCause::Io(error),
                report: Box::new(report.clone()),
                uncertain: Some(old_pack),
            })?;
        }
        Ok(report)
    }
}

#[cfg(unix)]
fn complete_plan(
    repository: &Repository,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
) -> Result<RetentionPlan, RetireCause> {
    let plan = repository.plan_retention(policy, cancel);
    if cancel.load(Ordering::Relaxed) {
        return Err(RetireCause::Cancelled);
    }
    match &plan.outcome {
        RetentionOutcome::Complete => Ok(plan),
        RetentionOutcome::Incomplete(reason) => Err(RetireCause::Incomplete(reason.clone())),
    }
}

#[cfg(unix)]
fn snapshot(
    directory: &Path,
    limit: usize,
    cancel: &AtomicBool,
) -> Result<BTreeMap<PathBuf, Stamp>, RetireCause> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(RetireCause::Io(error)),
    };
    let mut files = BTreeMap::new();
    for entry in entries {
        if cancel.load(Ordering::Relaxed) {
            return Err(RetireCause::Cancelled);
        }
        if files.len() >= limit {
            return Err(RetireCause::Incomplete("pack directory entry limit".into()));
        }
        let path = entry.map_err(RetireCause::Io)?.path();
        let metadata = fs::symlink_metadata(&path).map_err(RetireCause::Io)?;
        if !metadata.is_file() {
            return Err(RetireCause::Incomplete("non-file pack entry".into()));
        }
        files.insert(
            path,
            Stamp {
                len: metadata.len(),
                modified: metadata.modified().map_err(RetireCause::Io)?,
            },
        );
    }
    Ok(files)
}

#[cfg(unix)]
fn same_old_generation(
    before: &BTreeMap<PathBuf, Stamp>,
    after: &BTreeMap<PathBuf, Stamp>,
    new_pack: &Path,
    new_index: &Path,
) -> bool {
    before
        .iter()
        .filter(|(path, _)| path.as_path() != new_pack && path.as_path() != new_index)
        .eq(after
            .iter()
            .filter(|(path, _)| path.as_path() != new_pack && path.as_path() != new_index))
}

#[cfg(unix)]
fn old_indices(
    before: &BTreeMap<PathBuf, Stamp>,
    plan: &RetentionPlan,
    new_pack: &Path,
) -> Vec<PathBuf> {
    before
        .keys()
        .filter(|path| path.extension().is_some_and(|ext| ext == "idx"))
        .filter(|index| {
            let pack = index.with_extension("pack");
            pack != new_pack
                && before.contains_key(&pack)
                && !plan.protected_packs.contains(&pack)
                && !before.keys().any(|path| {
                    path.file_stem() == pack.file_stem()
                        && !matches!(
                            path.extension().and_then(|ext| ext.to_str()),
                            Some("idx" | "pack" | "rev" | "bitmap")
                        )
                })
        })
        .cloned()
        .collect()
}

#[cfg(unix)]
fn retirable_paths(index: &Path, before: &BTreeMap<PathBuf, Stamp>) -> Vec<PathBuf> {
    let mut paths = vec![index.to_path_buf()];
    for extension in ["rev", "bitmap"] {
        let path = index.with_extension(extension);
        if before.contains_key(&path) {
            paths.push(path);
        }
    }
    paths.push(index.with_extension("pack"));
    paths
}

#[cfg(unix)]
fn recheck(path: &Path, expected: Stamp) -> Result<(), RetireCause> {
    let metadata = fs::symlink_metadata(path).map_err(RetireCause::Io)?;
    if !metadata.is_file()
        || metadata.len() != expected.len
        || metadata.modified().map_err(RetireCause::Io)? != expected.modified
    {
        return Err(RetireCause::Changed);
    }
    Ok(())
}

fn check(cancel: &AtomicBool, report: &RetireReport) -> Result<(), RetireFailure> {
    if cancel.load(Ordering::Relaxed) {
        Err(failed(RetireCause::Cancelled, report.clone()))
    } else {
        Ok(())
    }
}

fn failed(cause: RetireCause, report: RetireReport) -> RetireFailure {
    RetireFailure {
        cause,
        report: Box::new(report),
        uncertain: None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use super::*;

    fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
        let mut child = Command::new("git")
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

    fn fixture() -> (tempfile::TempDir, Repository, RetentionPolicy, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "--template=", "."], b"");
        let tree = git(root.path(), &["mktree"], b"");
        let commit = git(root.path(), &["commit-tree", &tree], b"one\n");
        git(
            root.path(),
            &["update-ref", "refs/heads/main", &commit],
            b"",
        );
        git(
            root.path(),
            &["repack", "-ad", "--no-write-bitmap-index"],
            b"",
        );
        let repository = Repository::open(root.path()).unwrap();
        let directory = repository.object_dir().join("pack");
        let old_index = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
            .unwrap();
        let policy = RetentionPolicy {
            recent_cutoff: SystemTime::now() + Duration::from_secs(60),
            ..Default::default()
        };
        (root, repository, policy, old_index)
    }

    #[test]
    fn retiring_packs_removes_multi_pack_index_so_git_can_read_the_store() {
        let (root, _repository, policy, _old_index) = fixture();
        let tree = git(root.path(), &["mktree"], b"");
        let parent = git(root.path(), &["rev-parse", "refs/heads/main"], b"");
        let commit = git(
            root.path(),
            &["commit-tree", &tree, "-p", &parent],
            b"two\n",
        );
        git(
            root.path(),
            &["update-ref", "refs/heads/main", &commit],
            b"",
        );
        git(
            root.path(),
            &["repack", "-d", "--no-write-bitmap-index"],
            b"",
        );
        git(root.path(), &["multi-pack-index", "write"], b"");
        let repository = Repository::open(root.path()).unwrap();
        let report = repository
            .retire_old_packs_exclusive(&policy, RepackLimits::default(), &AtomicBool::new(false))
            .unwrap();
        let midx = repository.object_dir().join("pack/multi-pack-index");
        assert!(report.removed.contains(&midx));
        assert!(!midx.exists());
        git(root.path(), &["fsck", "--strict"], b"");
    }

    #[test]
    fn new_root_after_publication_refuses_old_pack_deletion() {
        let (root, repository, policy, old_index) = fixture();
        let result = repository.retire_old_packs_at(
            &policy,
            RepackLimits::default(),
            &AtomicBool::new(false),
            || {
                let id = git(root.path(), &["hash-object", "-w", "--stdin"], b"new");
                git(root.path(), &["update-ref", "refs/tags/new", &id], b"");
            },
            |path| fs::remove_file(path),
            |directory| File::open(directory).and_then(|dir| dir.sync_all()),
        );
        let error = result.unwrap_err();
        assert!(matches!(error.cause, RetireCause::Changed));
        assert!(error.report.published.is_some());
        assert!(error.report.removed.is_empty());
        assert!(old_index.exists());
    }

    #[test]
    fn cancellation_after_publication_preserves_old_pack() {
        let (_root, repository, policy, old_index) = fixture();
        let cancel = AtomicBool::new(false);
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &cancel,
                || cancel.store(true, Ordering::Relaxed),
                |path| fs::remove_file(path),
                |directory| File::open(directory).and_then(|dir| dir.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Cancelled));
        assert!(error.report.published.is_some());
        assert!(error.report.removed.is_empty());
        assert!(old_index.exists());
    }

    #[test]
    fn unlink_fault_reports_removed_index_and_uncertain_sidecar() {
        let (root, repository, policy, old_index) = fixture();
        let mut calls = 0;
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {},
                |path| {
                    calls += 1;
                    if calls == 2 {
                        Err(io::ErrorKind::PermissionDenied.into())
                    } else {
                        fs::remove_file(path)
                    }
                },
                |directory| File::open(directory).and_then(|dir| dir.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Io(_)));
        assert!(error.report.published.is_some());
        assert_eq!(error.report.removed, std::slice::from_ref(&old_index));
        assert!(error.uncertain.is_some());
        let tip = git(root.path(), &["rev-parse", "refs/heads/main"], b"");
        assert_eq!(git(root.path(), &["cat-file", "-t", &tip], b""), "commit");
    }

    #[test]
    fn publication_disk_full_sync_failure_retains_old_pair_and_reports_visible_new_pair() {
        let (_root, repository, policy, old_index) = fixture();
        let error = repository
            .retire_old_packs_at(
                &policy,
                RepackLimits::default(),
                &AtomicBool::new(false),
                || {},
                |path| fs::remove_file(path),
                |_| Err(io::Error::from_raw_os_error(28)),
            )
            .unwrap_err();
        assert!(matches!(error.cause, RetireCause::Io(_)));
        assert!(error.report.published.is_some());
        assert!(!error.report.durable);
        assert!(error.report.removed.is_empty());
        assert!(old_index.exists());
    }
}

/// Multi-pack index files in a pack directory: the index, its bitmaps and reverse indexes, and an
/// incremental chain directory.
#[cfg(unix)]
pub(super) fn multi_pack_index_paths(directory: &Path) -> io::Result<Vec<std::path::PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.as_encoded_bytes().starts_with(b"multi-pack-index") {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}
