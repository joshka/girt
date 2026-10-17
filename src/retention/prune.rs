use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;
use std::{fs, io};

use super::{RetentionError, RetentionPolicy};
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
    ///
    /// # Errors
    ///
    /// Implementors must return an error when repository-wide exclusion cannot be established.
    /// A failed acquisition must not grant a guard that suggests maintenance can proceed.
    fn acquire(&mut self, repository: &Repository) -> io::Result<Self::Guard>;
}

/// Loose object identities removed from the primary object store.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct PruneReport {
    /// Successfully removed identities, in path order. Derived commit-graph cleanup is not listed.
    pub deleted: Vec<ObjectId>,
}

/// Why a loose-object sweep stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PruneCause {
    /// A pinned pack/index changed or a content read found damage before this mutation boundary.
    #[error(transparent)]
    PackChanged(Box<crate::ObjectReadError>),
    /// Caller isolation could not be established.
    #[error("maintenance isolation unavailable")]
    Isolation(#[source] io::Error),
    /// A root, object or storage scan was incomplete.
    #[error("retention scan incomplete")]
    Incomplete(#[source] RetentionError),
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
///
/// `report.deleted` is authoritative for confirmed effects; `uncertain` identifies an unlink
/// whose outcome needs inspection. Keep `cause` and its source chain. An incomplete or changed
/// observation never authorizes continuing with candidates from an older plan.
#[derive(Debug, thiserror::Error)]
#[error("loose object pruning failed")]
#[non_exhaustive]
pub struct PruneFailure {
    /// The reason the sweep stopped.
    #[source]
    pub cause: PruneCause,
    /// Successfully removed objects before the failure.
    pub report: PruneReport,
    /// Identity at a failed object unlink boundary; inspect storage before retrying.
    /// A later commit-graph cleanup failure has no uncertain object identity.
    pub uncertain: Option<ObjectId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Candidate {
    id: ObjectId,
    path: PathBuf,
    len: u64,
    modified: SystemTime,
}

impl Repository {
    /// Removes old unreachable loose objects from this repository's primary store.
    ///
    /// The caller supplies a repository-wide isolation boundary through `isolation`. Inside that
    /// boundary this operation inventories primary loose entries. With no `recent_cutoff`, or if
    /// none are at or before its instant, it returns an empty report without validating object
    /// history. Otherwise it builds one complete plan, inventories loose entries twice, and
    /// revalidates root-bearing metadata and storage fingerprints before the first deletion.
    /// All reflog candidates are retained regardless of the policy's expiry cutoffs; this
    /// operation does not expire reflogs, repack, or remove packs or alternate storage. It
    /// removes only regular primary loose files at or before `recent_cutoff` whose identities
    /// are outside the complete reachable closure. No preceding additive repack is required.
    /// The inclusive loose cutoff matches Git prune
    /// expiry; files newer than the exact instant remain roots, including subsecond
    /// modification times.
    ///
    /// After any object deletion, removes `objects/info/commit-graph` and the
    /// `objects/info/commit-graphs` directory so Git cannot consult stale derived history.
    /// Git can rebuild these accelerators; queries lose their speedup until then. A cleanup
    /// failure returns [`PruneCause::Io`] with the completed object-deletion report and no
    /// uncertain object. No-op pruning leaves these caches untouched.
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
    ///
    /// # Default policy refuses age-based reclamation
    ///
    /// Even an unreferenced loose object survives when no cutoff is selected. This zero-deletion
    /// fast path does not validate the repository's history.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let cancel = AtomicBool::new(false);
    ///
    /// // This fixture is private to this single-threaded example. No external writer, reader or
    /// // alternate-store dependent exists. A shared repository needs a real exclusion mechanism.
    /// struct PrivateFixture;
    /// impl girt::retention::MaintenanceIsolation for PrivateFixture {
    ///     type Guard = ();
    ///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
    ///         Ok(())
    ///     }
    /// }
    ///
    /// use girt::retention::RetentionPolicy;
    /// let id = repo.loose_objects().write_blob(b"not yet eligible")?;
    /// let report =
    ///     repo.prune_unreachable_loose(&mut PrivateFixture, &RetentionPolicy::default(), &cancel)?;
    /// assert!(report.deleted.is_empty());
    /// assert!(repo.loose_objects().read_blob(id, 1024).is_ok());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
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
        self.finish_prune(report)
    }

    fn finish_prune(&self, report: PruneReport) -> Result<PruneReport, PruneFailure> {
        if !report.deleted.is_empty() {
            // This derived-data cleanup needs no pack freshness proof: removing a commit-graph
            // cannot remove an object. It can name pruned commits; Git rebuilds it when needed.
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
        after_plan: impl FnOnce(),
        after_delete: impl FnMut(),
        remove: impl FnMut(&std::path::Path) -> io::Result<()>,
    ) -> Result<PruneReport, PruneFailure> {
        self.prune_planned_at(
            policy,
            cancel,
            None,
            (|| {}, after_plan, after_delete),
            remove,
        )
    }

    #[cfg(unix)]
    pub(super) fn prune_planned(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
        session: &super::inputs::Session,
    ) -> Result<PruneReport, PruneFailure> {
        let report = self.prune_planned_at(
            policy,
            cancel,
            Some(session),
            (|| {}, || {}, || {}),
            |path| fs::remove_file(path),
        )?;
        self.finish_prune(report)
    }

    fn prune_planned_at(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
        session: Option<&super::inputs::Session>,
        checkpoints: (impl FnOnce(), impl FnOnce(), impl FnMut()),
        mut remove: impl FnMut(&std::path::Path) -> io::Result<()>,
    ) -> Result<PruneReport, PruneFailure> {
        let _headers = crate::pack::HeaderScope::new();
        let (before_plan, after_plan, mut after_delete) = checkpoints;
        let mut report = PruneReport::default();
        check(cancel, &report)?;
        // Repository::open refreshes shallow roots and configuration that this handle may have
        // observed before the isolation boundary was acquired.
        let current = Repository::open(self.git_dir()).map_err(|error| {
            failure(
                PruneCause::Incomplete(RetentionError::read(error)),
                report.clone(),
            )
        })?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(failure(PruneCause::Changed, report));
        }
        let observed = candidates(&current, policy, cancel)
            .map_err(|cause| failure(cause, PruneReport::default()))?;
        before_plan();
        if session.is_none() && observed.is_empty() {
            // Caller isolation holds through this complete inventory. No unlink is possible,
            // so unvisited history need not supply deletion evidence.
            check(cancel, &report)?;
            return Ok(report);
        }
        let owned = if session.is_none() {
            Some(
                super::inputs::Session::new(&current, policy, cancel).map_err(|reason| {
                    failure(
                        if cancel.load(Ordering::Relaxed) {
                            PruneCause::Cancelled
                        } else {
                            PruneCause::Incomplete(reason)
                        },
                        report.clone(),
                    )
                })?,
            )
        } else {
            None
        };
        let session = session.or(owned.as_ref()).expect("retention session");
        let first = &session.plan;
        after_plan();
        check(cancel, &report)?;
        let refreshed = Repository::open(self.git_dir()).map_err(|error| {
            failure(
                PruneCause::Incomplete(RetentionError::read(error)),
                report.clone(),
            )
        })?;
        if refreshed.object_dir() != current.object_dir()
            || refreshed.object_format() != current.object_format()
            || !refreshed
                .shallow_roots()
                .iter()
                .eq(current.shallow_roots().iter())
        {
            return Err(failure(PruneCause::Changed, report));
        }
        session
            .plan
            .check_packs_unchanged(cancel)
            .map_err(|error| failure(PruneCause::PackChanged(Box::new(error)), report.clone()))?;
        session.validate(&refreshed, policy, cancel).map_err(|_| {
            failure(
                if cancel.load(Ordering::Relaxed) {
                    PruneCause::Cancelled
                } else {
                    PruneCause::Changed
                },
                report.clone(),
            )
        })?;
        let repeated = candidates(&refreshed, policy, cancel)
            .map_err(|cause| failure(cause, report.clone()))?;
        if observed != repeated {
            return Err(failure(PruneCause::Changed, report));
        }
        for candidate in observed {
            check(cancel, &report)?;
            if first.reachable.contains(&candidate.id)
                || first.protected_objects.contains(&candidate.id)
            {
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
            session
                .plan
                .check_packs_unchanged(cancel)
                .map_err(|error| {
                    failure(PruneCause::PackChanged(Box::new(error)), report.clone())
                })?;
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

pub(super) fn candidates(
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
            return Err(PruneCause::Incomplete(RetentionError::NonUtf8ObjectFanout));
        };
        if prefix.len() != 2 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PruneCause::Incomplete(
                RetentionError::UnknownObjectDirectoryEntry,
            ));
        }
        if !fs::symlink_metadata(entry.path())
            .map_err(PruneCause::Io)?
            .is_dir()
        {
            return Err(PruneCause::Incomplete(
                RetentionError::NonDirectoryObjectFanout,
            ));
        }
        for file in fs::read_dir(entry.path()).map_err(PruneCause::Io)? {
            check_count(&mut visited, policy.max_entries)?;
            check_cancel(cancel)?;
            let file = file.map_err(PruneCause::Io)?;
            let path = file.path();
            let metadata = fs::symlink_metadata(&path).map_err(PruneCause::Io)?;
            if !metadata.is_file() {
                return Err(PruneCause::Incomplete(RetentionError::NonFileLooseObject));
            }
            let Some(suffix) = file.file_name().to_str().map(str::to_owned) else {
                return Err(PruneCause::Incomplete(RetentionError::NonUtf8LooseObject));
            };
            let id = ObjectId::from_hex(repository.object_format(), &format!("{prefix}{suffix}"))
                .map_err(|error| PruneCause::Incomplete(RetentionError::malformed(error)))?;
            let modified = metadata.modified().map_err(PruneCause::Io)?;
            if policy
                .recent_cutoff
                .is_some_and(|cutoff| modified <= cutoff)
            {
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
        Err(PruneCause::Incomplete(
            RetentionError::LooseObjectEntryLimit,
        ))
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
            recent_cutoff: Some(SystemTime::now() + Duration::from_secs(60)),
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
    fn new_loose_root_between_inventory_and_plan_refuses_before_deletion() {
        let (root, repo, policy) = fixture();
        let old = git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            b"old candidate",
        );
        let cancel = AtomicBool::new(false);
        let result = repo.prune_planned_at(
            &policy,
            &cancel,
            None,
            (
                || {
                    let added = git(root.path(), &["hash-object", "-w", "--stdin"], b"new root");
                    git(root.path(), &["update-ref", "refs/tags/added", &added], b"");
                },
                || {},
                || {},
            ),
            |path| fs::remove_file(path),
        );
        let error = result.unwrap_err();
        assert!(matches!(error.cause, PruneCause::Changed));
        assert!(error.report.deleted.is_empty());
        assert_eq!(
            git(root.path(), &["cat-file", "blob", "refs/tags/added"], b""),
            "new root"
        );
        assert_eq!(
            git(root.path(), &["cat-file", "blob", &old], b""),
            "old candidate"
        );
    }

    fn timestamp_handle(path: &Path) -> fs::File {
        #[cfg(not(windows))]
        {
            fs::File::open(path).unwrap()
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;

            use windows_sys::Win32::Storage::FileSystem::FILE_WRITE_ATTRIBUTES;
            fs::OpenOptions::new()
                .access_mode(FILE_WRITE_ATTRIBUTES)
                .open(path)
                .unwrap()
        }
    }

    #[test]
    fn default_policy_preserves_epoch_zero_loose_object() {
        let (root, repo, _) = fixture();
        let id = git(
            root.path(),
            &["hash-object", "-w", "--stdin"],
            b"epoch zero",
        );
        let path = root.path().join("objects").join(&id[..2]).join(&id[2..]);
        timestamp_handle(&path)
            .set_modified(std::time::UNIX_EPOCH)
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            std::time::UNIX_EPOCH
        );
        let report = repo
            .prune_unreachable_loose_at(
                &RetentionPolicy::default(),
                &AtomicBool::new(false),
                || {},
                || {},
                |path| fs::remove_file(path),
            )
            .unwrap();
        assert!(report.deleted.is_empty());
        assert_eq!(
            git(root.path(), &["cat-file", "blob", &id], b""),
            "epoch zero"
        );
    }

    #[test]
    fn freshened_candidate_refuses_before_deletion() {
        let (root, repo, policy) = fixture();
        let id = git(root.path(), &["hash-object", "-w", "--stdin"], b"freshened");
        let path = root.path().join("objects").join(&id[..2]).join(&id[2..]);
        let newer = policy.recent_cutoff.unwrap() + Duration::from_secs(1);
        let file = timestamp_handle(&path);
        let result = repo.prune_unreachable_loose_at(
            &policy,
            &AtomicBool::new(false),
            || file.set_modified(newer).unwrap(),
            || {},
            |path| fs::remove_file(path),
        );
        let error = result.unwrap_err();
        assert!(matches!(error.cause, PruneCause::Changed));
        assert!(error.report.deleted.is_empty());
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), newer);
        assert_eq!(
            git(root.path(), &["cat-file", "blob", &id], b""),
            "freshened"
        );
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
    #[cfg(unix)]
    #[rstest::rstest]
    #[case::truncate(true)]
    #[case::overwrite(false)]
    fn warmed_pack_change_refuses_prune(#[case] truncate: bool) {
        let (_root, repo, policy, pack) = super::super::walk_tests::header_fixture();
        let error = repo
            .prune_unreachable_loose_at(
                &policy,
                &AtomicBool::new(false),
                || super::super::walk_tests::change_pack(&pack, truncate),
                || {},
                |_| panic!("must not unlink"),
            )
            .unwrap_err();
        assert!(matches!(&error.cause, PruneCause::PackChanged(source)
            if matches!(source.as_ref(), crate::ObjectReadError::PackChanged { path } if path == &pack)));
        assert!(error.report.deleted.is_empty());
    }
}
