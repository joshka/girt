use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use std::{fs, io};

use super::{RetentionOutcome, RetentionPlan, RetentionPolicy};
use crate::{ObjectId, Repository};

/// Acquires caller-owned exclusion of every writer and dependent reader.
///
/// The returned guard must hold its isolation boundary until dropped. It must exclude external
/// Git processes, not just girt callers, and readers that may still need an old loose object. It
/// must also account for repositories using this object store as an alternate. Girt cannot
/// discover or enforce those external relationships. Return an error if the boundary cannot be
/// established. A lock observed only by girt is insufficient.
pub trait MaintenanceIsolation {
    /// Guard whose lifetime holds the repository-wide exclusion boundary.
    type Guard;

    /// Acquires the boundary before any maintenance scan or mutation.
    fn acquire(&mut self, repository: &Repository) -> io::Result<Self::Guard>;
}

/// Loose object identities removed from the primary object store.
#[derive(Clone, Debug, Default)]
pub struct PruneReport {
    /// Successfully removed identities, in path order.
    pub deleted: Vec<ObjectId>,
}

/// Why a loose-object sweep stopped.
#[derive(Debug, thiserror::Error)]
pub enum PruneCause {
    /// Caller isolation could not be established.
    #[error("maintenance isolation unavailable")]
    Isolation(#[source] io::Error),
    /// A root, object or storage scan was incomplete.
    #[error("retention scan incomplete: {0}")]
    Incomplete(String),
    /// The observed roots or loose-object generation changed before mutation.
    #[error("maintenance generation changed")]
    Changed,
    /// Cancellation was observed; inspect the report for completed deletions.
    #[error("maintenance cancelled")]
    Cancelled,
    /// A filesystem operation failed; inspect the report for completed deletions.
    #[error("maintenance I/O")]
    Io(#[source] io::Error),
}

/// A failed sweep retains the known successful deletion prefix and any uncertain unlink.
#[derive(Debug, thiserror::Error)]
#[error("loose object pruning failed")]
pub struct PruneFailure {
    /// The reason the sweep stopped.
    #[source]
    pub cause: PruneCause,
    /// Successfully removed objects before the failure.
    pub report: PruneReport,
    /// Identity at a failed unlink boundary; inspect storage before retrying.
    pub uncertain: Option<ObjectId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Candidate {
    id: ObjectId,
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

impl Repository {
    /// Removes old unreachable loose objects from this repository's primary store.
    ///
    /// The caller supplies a repository-wide isolation boundary through `isolation`. Inside that
    /// boundary this operation scans all roots and reachable objects twice, inventories loose
    /// entries twice, and refuses mutation if the observations differ. All reflog candidates are
    /// retained regardless of the policy's expiry cutoffs; this operation does not expire reflogs,
    /// repack, or remove packs or alternate storage. It removes only regular primary loose files
    /// older than `recent_cutoff` whose identities are outside the complete reachable closure.
    /// This does not depend on R32's additive publication for object availability.
    ///
    /// A complete scan or matching generations cannot exclude an external Git process. The
    /// isolation implementation must exclude those processes and old readers until the returned
    /// guard is dropped; otherwise acquisition must refuse. Cancellation or I/O after a deletion
    /// returns the known completed prefix in [`PruneFailure::report`]. An unlink error identifies
    /// its uncertain target in [`PruneFailure::uncertain`]. A retry must rescan from scratch.
    /// Deletion is not crash atomic; interruption can leave any prefix removed. Since no reachable
    /// object is removed, directory synchronization is unnecessary for object availability.
    ///
    /// # Errors
    ///
    /// Isolation refusal, incomplete scans, changed observations, cancellation and I/O are
    /// reported with the known completed prefix. Before the first deletion it is empty.
    pub fn prune_unreachable_loose<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<PruneReport, PruneFailure> {
        let _guard = isolation
            .acquire(self)
            .map_err(|error| failure(PruneCause::Isolation(error), PruneReport::default()))?;
        self.prune_unreachable_loose_exclusive(policy, cancel)
    }

    pub(super) fn prune_unreachable_loose_exclusive(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<PruneReport, PruneFailure> {
        let report = self.prune_unreachable_loose_at(
            policy,
            cancel,
            || {},
            || {},
            |path| fs::remove_file(path),
        )?;
        if !report.deleted.is_empty() {
            // A commit-graph can name pruned commits; Git rebuilds it when needed, as `git gc`
            // does.
            let info = self.object_dir().join("info");
            let remove = |result: io::Result<()>| match result {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    Err(failure(PruneCause::Io(error), report.clone()))
                }
                _ => Ok(()),
            };
            remove(fs::remove_file(info.join("commit-graph")))?;
            remove(fs::remove_dir_all(info.join("commit-graphs")))?;
        }
        Ok(report)
    }

    // Checkpoints keep generation changes and post-deletion cancellation deterministic in tests.
    fn prune_unreachable_loose_at(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
        after_inventory: impl FnOnce(),
        mut after_delete: impl FnMut(),
        mut remove: impl FnMut(&std::path::Path) -> io::Result<()>,
    ) -> Result<PruneReport, PruneFailure> {
        let mut report = PruneReport::default();
        check(cancel, &report)?;
        // Repository::open refreshes shallow roots and configuration that this handle may have
        // observed before the isolation boundary was acquired.
        let current = Repository::open(self.git_dir())
            .map_err(|error| failure(PruneCause::Incomplete(error.to_string()), report.clone()))?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(failure(PruneCause::Changed, report));
        }
        let first = complete_plan(&current, policy, cancel)
            .map_err(|cause| failure(cause, report.clone()))?;
        let observed = candidates(&current, policy, cancel)
            .map_err(|cause| failure(cause, PruneReport::default()))?;
        after_inventory();
        let refreshed = Repository::open(self.git_dir())
            .map_err(|error| failure(PruneCause::Incomplete(error.to_string()), report.clone()))?;
        if refreshed.object_dir() != current.object_dir()
            || refreshed.object_format() != current.object_format()
            || !refreshed
                .shallow_roots()
                .iter()
                .eq(current.shallow_roots().iter())
        {
            return Err(failure(PruneCause::Changed, report));
        }
        let second = complete_plan(&refreshed, policy, cancel)
            .map_err(|cause| failure(cause, report.clone()))?;
        let repeated = candidates(&refreshed, policy, cancel)
            .map_err(|cause| failure(cause, report.clone()))?;
        if first.roots != second.roots
            || first.reachable != second.reachable
            || first.protected_packs != second.protected_packs
            || first.alternate_stores != second.alternate_stores
            || observed != repeated
        {
            return Err(failure(PruneCause::Changed, report));
        }
        for candidate in observed {
            check(cancel, &report)?;
            if first.reachable.contains(&candidate.id) {
                continue;
            }
            let metadata = fs::symlink_metadata(&candidate.path)
                .map_err(|error| failure(PruneCause::Io(error), report.clone()))?;
            let modified = metadata
                .modified()
                .map_err(|error| failure(PruneCause::Io(error), report.clone()))?;
            if !metadata.is_file()
                || metadata.len() != candidate.len
                || modified != candidate.modified
            {
                return Err(failure(PruneCause::Changed, report));
            }
            remove(&candidate.path).map_err(|error| PruneFailure {
                cause: PruneCause::Io(error),
                report: report.clone(),
                uncertain: Some(candidate.id),
            })?;
            report.deleted.push(candidate.id);
            after_delete();
        }
        Ok(report)
    }
}

fn complete_plan(
    repository: &Repository,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
) -> Result<RetentionPlan, PruneCause> {
    let plan = repository.plan_retention(policy, cancel);
    check_cancel(cancel)?;
    match &plan.outcome {
        RetentionOutcome::Complete => Ok(plan),
        RetentionOutcome::Incomplete(reason) => Err(PruneCause::Incomplete(reason.clone())),
    }
}

fn candidates(
    repository: &Repository,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
) -> Result<Vec<Candidate>, PruneCause> {
    let mut result = Vec::new();
    let mut visited = 0usize;
    for entry in fs::read_dir(repository.object_dir()).map_err(PruneCause::Io)? {
        check_count(&mut visited, policy.max_entries)?;
        check_cancel(cancel)?;
        let entry = entry.map_err(PruneCause::Io)?;
        let name = entry.file_name();
        if name == "pack" || name == "info" {
            continue;
        }
        let Some(prefix) = name.to_str() else {
            return Err(PruneCause::Incomplete("non-UTF-8 object fanout".into()));
        };
        if prefix.len() != 2 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PruneCause::Incomplete(
                "unknown object directory entry".into(),
            ));
        }
        if !fs::symlink_metadata(entry.path())
            .map_err(PruneCause::Io)?
            .is_dir()
        {
            return Err(PruneCause::Incomplete("non-directory object fanout".into()));
        }
        for file in fs::read_dir(entry.path()).map_err(PruneCause::Io)? {
            check_count(&mut visited, policy.max_entries)?;
            check_cancel(cancel)?;
            let file = file.map_err(PruneCause::Io)?;
            let path = file.path();
            let metadata = fs::symlink_metadata(&path).map_err(PruneCause::Io)?;
            if !metadata.is_file() {
                return Err(PruneCause::Incomplete("non-file loose object".into()));
            }
            let Some(suffix) = file.file_name().to_str().map(str::to_owned) else {
                return Err(PruneCause::Incomplete("non-UTF-8 loose object".into()));
            };
            let id = ObjectId::from_hex(repository.object_format(), &format!("{prefix}{suffix}"))
                .map_err(|error| PruneCause::Incomplete(error.to_string()))?;
            let modified = metadata.modified().map_err(PruneCause::Io)?;
            if modified < policy.recent_cutoff {
                result.push(Candidate {
                    id,
                    path,
                    len: metadata.len(),
                    modified,
                });
            }
        }
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(result)
}

fn check_count(visited: &mut usize, limit: usize) -> Result<(), PruneCause> {
    *visited += 1;
    if *visited > limit {
        Err(PruneCause::Incomplete("loose object entry limit".into()))
    } else {
        Ok(())
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), PruneCause> {
    if cancel.load(Ordering::Relaxed) {
        Err(PruneCause::Cancelled)
    } else {
        Ok(())
    }
}

fn check(cancel: &AtomicBool, report: &PruneReport) -> Result<(), PruneFailure> {
    check_cancel(cancel).map_err(|cause| {
        failure(
            cause,
            PruneReport {
                deleted: report.deleted.clone(),
            },
        )
    })
}

fn failure(cause: PruneCause, report: PruneReport) -> PruneFailure {
    PruneFailure {
        cause,
        report,
        uncertain: None,
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::*;

    fn git(root: &Path, args: &[&str], input: &[u8]) -> String {
        let mut child = Command::new("git")
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", root.join("absent-config"))
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

    fn fixture() -> (tempfile::TempDir, Repository, RetentionPolicy) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "--template=", "."], b"");
        let repo = Repository::open(root.path()).unwrap();
        let policy = RetentionPolicy {
            recent_cutoff: SystemTime::now() + Duration::from_secs(60),
            ..Default::default()
        };
        (root, repo, policy)
    }

    #[test]
    fn new_root_between_scans_refuses_before_deletion() {
        let (root, repo, policy) = fixture();
        let id = git(root.path(), &["hash-object", "-w", "--stdin"], b"candidate");
        let cancel = AtomicBool::new(false);
        let result = repo.prune_unreachable_loose_at(
            &policy,
            &cancel,
            || {
                git(root.path(), &["update-ref", "refs/tags/blob", &id], b"");
            },
            || {},
            |path| fs::remove_file(path),
        );
        let error = result.unwrap_err();
        assert!(matches!(error.cause, PruneCause::Changed));
        assert!(error.report.deleted.is_empty());
        assert_eq!(git(root.path(), &["cat-file", "-t", &id], b""), "blob");
    }

    #[test]
    fn cancellation_after_first_unlink_reports_partial_prefix() {
        let (root, repo, policy) = fixture();
        let first = git(root.path(), &["hash-object", "-w", "--stdin"], b"first");
        let second = git(root.path(), &["hash-object", "-w", "--stdin"], b"second");
        let cancel = AtomicBool::new(false);
        let error = repo
            .prune_unreachable_loose_at(
                &policy,
                &cancel,
                || {},
                || cancel.store(true, Ordering::Relaxed),
                |path| fs::remove_file(path),
            )
            .unwrap_err();
        assert!(matches!(error.cause, PruneCause::Cancelled));
        assert_eq!(error.report.deleted.len(), 1);
        let deleted = error.report.deleted[0].to_string();
        assert!(deleted == first || deleted == second);
        let survivor = if deleted == first { second } else { first };
        assert_eq!(
            git(root.path(), &["cat-file", "-t", &survivor], b""),
            "blob"
        );
    }

    #[test]
    fn unlink_fault_reports_known_prefix_and_uncertain_target() {
        let (root, repo, policy) = fixture();
        let first = git(root.path(), &["hash-object", "-w", "--stdin"], b"first");
        let second = git(root.path(), &["hash-object", "-w", "--stdin"], b"second");
        let mut calls = 0;
        let error = repo
            .prune_unreachable_loose_at(
                &policy,
                &AtomicBool::new(false),
                || {},
                || {},
                |path| {
                    calls += 1;
                    if calls == 2 {
                        Err(io::ErrorKind::PermissionDenied.into())
                    } else {
                        fs::remove_file(path)
                    }
                },
            )
            .unwrap_err();
        assert!(matches!(error.cause, PruneCause::Io(_)));
        assert_eq!(error.report.deleted.len(), 1);
        let deleted = error.report.deleted[0].to_string();
        let uncertain = error.uncertain.unwrap().to_string();
        assert!(deleted == first || deleted == second);
        assert!(uncertain == first || uncertain == second);
        assert_ne!(deleted, uncertain);
        assert_eq!(
            git(root.path(), &["cat-file", "-t", &uncertain], b""),
            "blob"
        );
    }
}
