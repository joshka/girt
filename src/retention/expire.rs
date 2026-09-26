#[cfg(unix)]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::fs::File;
use std::io;
#[cfg(unix)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(unix)]
use tempfile::NamedTempFile;

use super::{MaintenanceIsolation, ReflogSource, RetentionPolicy};
#[cfg(unix)]
use super::{RetentionOutcome, RetentionPlan};
use crate::Repository;
#[cfg(unix)]
use crate::refs::ImportedRecord;
#[cfg(unix)]
use crate::refs::reftable::{ExpireStackError, RecordName, Snapshot, expire_logs};
use crate::refs::{Backend, ReferenceError};

/// Histories whose expiry rewrite was durably published.
#[derive(Clone, Debug, Default)]
pub struct ExpireReport {
    /// Known published histories, in source order.
    pub changed: Vec<ReflogSource>,
    /// Old unlisted reftable files whose cleanup failed after durable publication.
    pub retained_tables: Vec<PathBuf>,
}

/// Why expiry stopped.
#[derive(Debug, thiserror::Error)]
pub enum ExpireCause {
    /// The caller could not exclude writers and dependent readers.
    #[error("maintenance isolation unavailable: {0}")]
    Isolation(#[source] io::Error),
    /// A bounded root or log observation was incomplete.
    #[error("retention scan incomplete: {0}")]
    Incomplete(String),
    /// Stored history or the retention observation changed before publication.
    #[error("reflog generation changed")]
    Changed,
    /// The reference backend has no conditional expiry writer on this platform.
    #[error("reflog expiry is unsupported for this reference backend")]
    UnsupportedBackend,
    /// The platform has no verified directory synchronization for destructive expiry.
    #[error("durable reflog expiry is unsupported on this platform")]
    UnsupportedDurability,
    /// Cancellation was observed between publication steps.
    #[error("reflog expiry cancelled")]
    Cancelled,
    /// An artifact operation failed; inspect the partial report.
    #[error("reflog expiry I/O: {0}")]
    Io(#[source] io::Error),
    /// A conditional reftable stack publication or cleanup failed.
    #[error("reftable expiry: {0}")]
    Reference(#[source] Box<ReferenceError>),
}

/// Expiry failure with known publications and a possible uncertain replacement.
#[derive(Debug, thiserror::Error)]
#[error("{cause}")]
pub struct ExpireFailure {
    /// Failure class.
    #[source]
    pub cause: ExpireCause,
    /// Histories whose replacement reported success.
    pub report: Box<ExpireReport>,
    /// History at a failed replacement boundary, if its effect is uncertain.
    pub uncertain: Option<ReflogSource>,
    /// Reftable stack whose replacement list may be visible at a failed boundary.
    pub uncertain_stack: Option<PathBuf>,
}

#[cfg(unix)]
struct PreparedLog {
    source: ReflogSource,
    path: PathBuf,
    original: Vec<u8>,
    retained: Vec<u8>,
}

#[cfg(unix)]
struct PreparedStack {
    snapshot: Snapshot,
    keys: BTreeSet<(RecordName, u64)>,
    sources: Vec<ReflogSource>,
}

impl Repository {
    /// Expires eligible reflog records under caller-owned isolation.
    ///
    /// The guard must exclude external Git and girt writers, readers depending on old history,
    /// and alternate-store dependents for the whole action. A fresh complete retention plan
    /// classifies records using the policy's live and unreachable cutoffs. The operation prepares
    /// every changed files log's exact retained bytes or reftable stack's merged records, rescans
    /// roots and expiry positions, and conditionally publishes each replacement. Files preserve
    /// the original bytes and order of retained records; empty histories remain empty files.
    /// Reftable preserves retained binary records and reference update indexes in a compacted
    /// replacement stack. Object storage is not pruned. On Unix, replacement files and containing
    /// directories are synchronized before a change is reported as durable, assuming a local
    /// filesystem that honors those calls. Other platforms refuse until the same publication
    /// contract is established. A fresh rescan remains necessary before later object pruning.
    ///
    /// Cancellation, I/O and interruption can leave a published prefix. An error from the
    /// replacement call names the uncertain history. Retry only after a fresh scan; do not reuse
    /// an older [`RetentionPlan`]. A directory synchronization error after replacement identifies
    /// the uncertain history or stack; callers must inspect it before relying on expired roots.
    ///
    /// # Errors
    ///
    /// Isolation refusal, unsupported backend, incomplete or changed input, cancellation and I/O
    /// return the known published prefix and any uncertain replacement.
    pub fn expire_reflogs<I: MaintenanceIsolation>(
        &self,
        isolation: &mut I,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<ExpireReport, ExpireFailure> {
        let _guard = isolation
            .acquire(self)
            .map_err(|error| failed(ExpireCause::Isolation(error), ExpireReport::default()))?;
        self.expire_reflogs_exclusive(policy, cancel)
    }

    #[cfg(unix)]
    pub(super) fn expire_reflogs_exclusive(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<ExpireReport, ExpireFailure> {
        if self.reference_backend() == Backend::Reftable {
            return self.expire_reftable_logs(policy, cancel);
        }
        self.expire_reflogs_at(
            policy,
            cancel,
            || {},
            || {},
            |temp, path| temp.persist(path).map(|_| ()).map_err(|error| error.error),
            |parent| File::open(parent).and_then(|directory| directory.sync_all()),
        )
    }

    #[cfg(unix)]
    fn expire_reftable_logs(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<ExpireReport, ExpireFailure> {
        let mut report = ExpireReport::default();
        check(cancel, &report)?;
        let current = Repository::open(self.git_dir())
            .map_err(|error| failed(ExpireCause::Incomplete(error.to_string()), report.clone()))?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
            || current.reference_backend() != Backend::Reftable
        {
            return Err(failed(ExpireCause::Changed, report));
        }
        let first = complete_plan(&current, policy, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        let prepared = prepare_reftable(&first, policy, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        let refreshed = Repository::open(self.git_dir())
            .map_err(|error| failed(ExpireCause::Incomplete(error.to_string()), report.clone()))?;
        if refreshed.object_dir() != current.object_dir()
            || refreshed.object_format() != current.object_format()
            || refreshed.reference_backend() != Backend::Reftable
            || !refreshed
                .shallow_roots()
                .iter()
                .eq(current.shallow_roots().iter())
        {
            return Err(failed(ExpireCause::Changed, report));
        }
        let second = complete_plan(&refreshed, policy, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        if first.roots != second.roots
            || first.reachable != second.reachable
            || first.required != second.required
            || first.observed_logs != second.observed_logs
            || first.reflog_expiry_candidates != second.reflog_expiry_candidates
            || first.protected_packs != second.protected_packs
            || first.alternate_stores != second.alternate_stores
        {
            return Err(failed(ExpireCause::Changed, report));
        }
        for (directory, stack) in prepared {
            check(cancel, &report)?;
            match expire_logs(
                &directory,
                self.object_format(),
                policy.reftable,
                &stack.snapshot,
                &stack.keys,
                cancel,
            ) {
                Ok(outcome) => {
                    report.changed.extend(stack.sources);
                    report
                        .retained_tables
                        .extend(outcome.retained.into_iter().map(|(path, _)| path));
                }
                Err(ExpireStackError::Changed) => {
                    return Err(failed(ExpireCause::Changed, report));
                }
                Err(ExpireStackError::Failed {
                    source,
                    publication_uncertain,
                    durable,
                }) => {
                    if durable {
                        report.changed.extend(stack.sources);
                    }
                    return Err(ExpireFailure {
                        cause: match source {
                            ReferenceError::Cancelled => ExpireCause::Cancelled,
                            other => ExpireCause::Reference(Box::new(other)),
                        },
                        report: Box::new(report),
                        uncertain: None,
                        uncertain_stack: publication_uncertain.then_some(directory),
                    });
                }
            }
        }
        Ok(report)
    }

    #[cfg(not(unix))]
    pub(super) fn expire_reflogs_exclusive(
        &self,
        _policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<ExpireReport, ExpireFailure> {
        let report = ExpireReport::default();
        check(cancel, &report)?;
        let current = Repository::open(self.git_dir())
            .map_err(|error| failed(ExpireCause::Incomplete(error.to_string()), report.clone()))?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(failed(ExpireCause::Changed, report));
        }
        if current.reference_backend() != Backend::Files {
            return Err(failed(ExpireCause::UnsupportedBackend, report));
        }
        Err(failed(ExpireCause::UnsupportedDurability, report))
    }

    // Checkpoints make source changes, post-publication cancellation and rename faults repeatable.
    #[cfg(unix)]
    fn expire_reflogs_at(
        &self,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
        after_prepare: impl FnOnce(),
        mut after_publish: impl FnMut(),
        mut replace: impl FnMut(NamedTempFile, &Path) -> io::Result<()>,
        mut sync_directory: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<ExpireReport, ExpireFailure> {
        let mut report = ExpireReport::default();
        check(cancel, &report)?;
        let current = Repository::open(self.git_dir())
            .map_err(|error| failed(ExpireCause::Incomplete(error.to_string()), report.clone()))?;
        if current.object_dir() != self.object_dir()
            || current.object_format() != self.object_format()
        {
            return Err(failed(ExpireCause::Changed, report));
        }
        if current.reference_backend() != Backend::Files {
            return Err(failed(ExpireCause::UnsupportedBackend, report));
        }
        let first = complete_plan(&current, policy, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        let prepared =
            prepare(&first, policy, cancel).map_err(|cause| failed(cause, report.clone()))?;
        after_prepare();
        let refreshed = Repository::open(self.git_dir())
            .map_err(|error| failed(ExpireCause::Incomplete(error.to_string()), report.clone()))?;
        if refreshed.object_dir() != current.object_dir()
            || refreshed.object_format() != current.object_format()
            || refreshed.reference_backend() != Backend::Files
            || !refreshed
                .shallow_roots()
                .iter()
                .eq(current.shallow_roots().iter())
        {
            return Err(failed(ExpireCause::Changed, report));
        }
        let second = complete_plan(&refreshed, policy, cancel)
            .map_err(|cause| failed(cause, report.clone()))?;
        if first.roots != second.roots
            || first.reachable != second.reachable
            || first.required != second.required
            || first.observed_logs != second.observed_logs
            || first.reflog_expiry_candidates != second.reflog_expiry_candidates
            || first.protected_packs != second.protected_packs
            || first.alternate_stores != second.alternate_stores
        {
            return Err(failed(ExpireCause::Changed, report));
        }
        for log in &prepared {
            check(cancel, &report)?;
            if read_bounded(&log.path, policy.reflog.bytes)
                .map_err(|cause| failed(cause, report.clone()))?
                != log.original
            {
                return Err(failed(ExpireCause::Changed, report));
            }
        }
        for log in prepared {
            check(cancel, &report)?;
            if read_bounded(&log.path, policy.reflog.bytes)
                .map_err(|cause| failed(cause, report.clone()))?
                != log.original
            {
                return Err(failed(ExpireCause::Changed, report));
            }
            let parent = log.path.parent().expect("reflog has parent");
            let mut temp = NamedTempFile::new_in(parent)
                .map_err(|error| failed(ExpireCause::Io(error), report.clone()))?;
            temp.write_all(&log.retained)
                .map_err(|error| failed(ExpireCause::Io(error), report.clone()))?;
            temp.as_file()
                .sync_all()
                .map_err(|error| failed(ExpireCause::Io(error), report.clone()))?;
            replace(temp, &log.path).map_err(|error| ExpireFailure {
                cause: ExpireCause::Io(error),
                report: Box::new(report.clone()),
                uncertain: Some(log.source.clone()),
                uncertain_stack: None,
            })?;
            sync_directory(parent).map_err(|error| ExpireFailure {
                cause: ExpireCause::Io(error),
                report: Box::new(report.clone()),
                uncertain: Some(log.source.clone()),
                uncertain_stack: None,
            })?;
            report.changed.push(log.source);
            after_publish();
        }
        Ok(report)
    }
}

#[cfg(unix)]
fn complete_plan(
    repository: &Repository,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
) -> Result<RetentionPlan, ExpireCause> {
    let plan = repository.plan_retention(policy, cancel);
    if cancel.load(Ordering::Relaxed) {
        return Err(ExpireCause::Cancelled);
    }
    match &plan.outcome {
        RetentionOutcome::Complete => Ok(plan),
        RetentionOutcome::Incomplete(reason) => Err(ExpireCause::Incomplete(reason.clone())),
    }
}

#[cfg(unix)]
fn prepare(
    plan: &RetentionPlan,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
) -> Result<Vec<PreparedLog>, ExpireCause> {
    let mut prepared = Vec::new();
    let mut total = 0u64;
    for (source, positions) in &plan.reflog_expiry_candidates {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExpireCause::Cancelled);
        }
        let repository = Repository::open(source.root())
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?;
        if repository.reference_backend() != Backend::Files {
            return Err(ExpireCause::UnsupportedBackend);
        }
        let refs = repository
            .references()
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?;
        let path = refs
            .reflog_path(source.name())
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?;
        let log = refs
            .imported_reflog(source.name(), policy.reflog, cancel)
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?
            .ok_or(ExpireCause::Changed)?;
        if !log.is_complete() {
            return Err(ExpireCause::Incomplete(format!(
                "incomplete reflog: {:?}",
                log.end()
            )));
        }
        let mut original = Vec::new();
        let mut retained = Vec::new();
        for (index, record) in log.records().iter().enumerate() {
            let ImportedRecord::File { bytes, .. } = record else {
                return Err(ExpireCause::UnsupportedBackend);
            };
            original.extend_from_slice(bytes);
            if positions.binary_search(&(index + 1)).is_err() {
                retained.extend_from_slice(bytes);
            }
        }
        total = total.saturating_add(original.len() as u64);
        if total > policy.max_reflog_bytes {
            return Err(ExpireCause::Incomplete(
                "aggregate reflog byte limit".into(),
            ));
        }
        if read_bounded(&path, policy.reflog.bytes)? != original {
            return Err(ExpireCause::Changed);
        }
        prepared.push(PreparedLog {
            source: source.clone(),
            path,
            original,
            retained,
        });
    }
    Ok(prepared)
}

#[cfg(unix)]
fn prepare_reftable(
    plan: &RetentionPlan,
    policy: &RetentionPolicy,
    cancel: &AtomicBool,
) -> Result<BTreeMap<PathBuf, PreparedStack>, ExpireCause> {
    let mut stacks = BTreeMap::new();
    for (source, positions) in &plan.reflog_expiry_candidates {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExpireCause::Cancelled);
        }
        let repository = Repository::open(source.root())
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?;
        if repository.reference_backend() != Backend::Reftable {
            return Err(ExpireCause::Changed);
        }
        let log = repository
            .references()
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?
            .with_reftable_limits(policy.reftable)
            .imported_reflog(source.name(), policy.reflog, cancel)
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?
            .ok_or(ExpireCause::Changed)?;
        if !log.is_complete() {
            return Err(ExpireCause::Incomplete(format!(
                "incomplete reflog: {:?}",
                log.end()
            )));
        }
        let directory = source.root().join("reftable");
        if !stacks.contains_key(&directory) {
            let snapshot = Snapshot::read(
                &directory,
                repository.object_format(),
                policy.reftable,
                cancel,
            )
            .map_err(|error| ExpireCause::Incomplete(error.to_string()))?;
            stacks.insert(
                directory.clone(),
                PreparedStack {
                    snapshot,
                    keys: BTreeSet::new(),
                    sources: Vec::new(),
                },
            );
        }
        let stack = stacks.get_mut(&directory).expect("stack inserted above");
        stack.sources.push(source.clone());
        let name = RecordName::from(source.name().clone());
        for position in positions {
            let Some(record) = position
                .checked_sub(1)
                .and_then(|index| log.records().get(index))
            else {
                return Err(ExpireCause::Changed);
            };
            let ImportedRecord::Reftable { update_index, .. } = record else {
                return Err(ExpireCause::Changed);
            };
            if !stack.keys.insert((name.clone(), *update_index)) {
                return Err(ExpireCause::Changed);
            }
        }
    }
    Ok(stacks)
}

#[cfg(unix)]
fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, ExpireCause> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(ExpireCause::Io)?
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(ExpireCause::Io)?;
    if bytes.len() > limit {
        Err(ExpireCause::Incomplete("reflog byte limit".into()))
    } else {
        Ok(bytes)
    }
}

fn check(cancel: &AtomicBool, report: &ExpireReport) -> Result<(), ExpireFailure> {
    if cancel.load(Ordering::Relaxed) {
        Err(failed(ExpireCause::Cancelled, report.clone()))
    } else {
        Ok(())
    }
}

fn failed(cause: ExpireCause, report: ExpireReport) -> ExpireFailure {
    ExpireFailure {
        cause,
        report: Box::new(report),
        uncertain: None,
        uncertain_stack: None,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::process::{Command, Stdio};
    use std::sync::atomic::Ordering;
    use std::time::{Duration, SystemTime};

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

    fn fixture() -> (
        tempfile::TempDir,
        Repository,
        RetentionPolicy,
        [PathBuf; 2],
        String,
    ) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "--bare", "--template=", "."], b"");
        let id = git(root.path(), &["hash-object", "-w", "--stdin"], b"logged");
        let path_a = root.path().join("logs/refs/heads/deleted-a");
        let path_b = root.path().join("logs/refs/heads/deleted-b");
        fs::create_dir_all(path_a.parent().unwrap()).unwrap();
        let record = format!("{} {id} A <a@b> 1700000000 +0000\told\n", "0".repeat(40));
        fs::write(&path_a, &record).unwrap();
        fs::write(&path_b, &record).unwrap();
        let repository = Repository::open(root.path()).unwrap();
        let policy = RetentionPolicy {
            recent_cutoff: SystemTime::now() + Duration::from_secs(60),
            reflog_expire_unreachable_before: Some(1700000001),
            ..Default::default()
        };
        (root, repository, policy, [path_a, path_b], id)
    }

    #[test]
    fn changed_log_after_preparation_refuses_without_publication() {
        let (_root, repository, policy, paths, id) = fixture();
        let before = fs::read(&paths[0]).unwrap();
        let error = repository
            .expire_reflogs_at(
                &policy,
                &AtomicBool::new(false),
                || {
                    let mut file = fs::OpenOptions::new().append(true).open(&paths[0]).unwrap();
                    writeln!(file, "{id} {id} A <a@b> 1700000002 +0000\tnew").unwrap();
                },
                || {},
                |temp, path| temp.persist(path).map(|_| ()).map_err(|error| error.error),
                |parent| File::open(parent).and_then(|directory| directory.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, ExpireCause::Changed));
        assert!(error.report.changed.is_empty());
        assert!(fs::read(&paths[0]).unwrap().starts_with(&before));
        assert_eq!(fs::read(&paths[1]).unwrap(), before);
    }

    #[test]
    fn cancellation_after_first_rewrite_reports_partial_prefix() {
        let (_root, repository, policy, paths, _) = fixture();
        let cancel = AtomicBool::new(false);
        let error = repository
            .expire_reflogs_at(
                &policy,
                &cancel,
                || {},
                || cancel.store(true, Ordering::Relaxed),
                |temp, path| temp.persist(path).map(|_| ()).map_err(|error| error.error),
                |parent| File::open(parent).and_then(|directory| directory.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, ExpireCause::Cancelled));
        assert_eq!(error.report.changed.len(), 1);
        assert_eq!(fs::read(&paths[0]).unwrap(), b"");
        assert!(!fs::read(&paths[1]).unwrap().is_empty());
    }

    #[test]
    fn failed_second_rewrite_names_uncertain_log() {
        let (_root, repository, policy, paths, _) = fixture();
        let mut calls = 0;
        let error = repository
            .expire_reflogs_at(
                &policy,
                &AtomicBool::new(false),
                || {},
                || {},
                |temp, path| {
                    calls += 1;
                    if calls == 2 {
                        Err(io::ErrorKind::PermissionDenied.into())
                    } else {
                        temp.persist(path).map(|_| ()).map_err(|error| error.error)
                    }
                },
                |parent| File::open(parent).and_then(|directory| directory.sync_all()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, ExpireCause::Io(_)));
        assert_eq!(error.report.changed.len(), 1);
        assert_eq!(
            error.uncertain.unwrap().name().as_bytes(),
            b"refs/heads/deleted-b"
        );
        assert_eq!(fs::read(&paths[0]).unwrap(), b"");
        assert!(!fs::read(&paths[1]).unwrap().is_empty());
    }

    #[test]
    fn directory_sync_failure_marks_published_log_uncertain() {
        let (_root, repository, policy, paths, _) = fixture();
        let error = repository
            .expire_reflogs_at(
                &policy,
                &AtomicBool::new(false),
                || {},
                || {},
                |temp, path| temp.persist(path).map(|_| ()).map_err(|error| error.error),
                |_| Err(io::ErrorKind::Other.into()),
            )
            .unwrap_err();
        assert!(matches!(error.cause, ExpireCause::Io(_)));
        assert!(error.report.changed.is_empty());
        assert_eq!(
            error.uncertain.unwrap().name().as_bytes(),
            b"refs/heads/deleted-a"
        );
        assert_eq!(fs::read(&paths[0]).unwrap(), b"");
        assert!(!fs::read(&paths[1]).unwrap().is_empty());
    }
}
