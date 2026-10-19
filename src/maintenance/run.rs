//! Explicit composition of bounded maintenance primitives.
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{AutoThreshold, MaintenanceConfig, MaintenanceTask, TaskSelection};
use crate::commit_graph::{CommitGraph, GraphInstallReport, GraphWriteMode};
use crate::pack::LoosePackLimits;
use crate::refs::{PackRefsLimits, PackRefsOptions, PackRefsReport, Target};
use crate::retention::{MaintenanceIsolation, PrunePackedLimits, PrunePackedSelection};
use crate::{AlternateLimits, HistoryLimits, ObjectKind, PackLimits, PeelLimits, Repository};

mod incremental;
mod loose;
pub use incremental::{IncrementalRepackLimits, IncrementalRepackReport};
pub use loose::LooseObjectsReport;

/// Per-task bounds for an explicit maintenance batch.
///
/// Defaults permit 100,000 reference inventory entries and 16 MiB of reference input,
/// two million commits and eight million parent edges, and 512 MiB of pack indexes.
/// Each task gets its own budget; graph loading/publication also uses the graph primitive's
/// fixed default bounds. Reference-backend metadata uses the repository opener's limits.
/// Loose-object defaults prune a verified prefix of at most 50,000 duplicates, then pack at most
/// 50,000 objects and 512 MiB of payload, retaining newly packed loose copies. Pruning and packing
/// have separate budgets. Automatic counting uses the loose-packing directory-entry budget.
/// This type is `Copy`, so future fields must also be `Copy`. Cancellation cannot interrupt an
/// individual bounded publication phase.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct MaintenanceLimits {
    /// Pack discovery, index and object decoding bounds.
    pub packs: PackLimits,
    /// Alternate-store discovery bounds.
    pub alternates: AlternateLimits,
    /// Reference-packing bounds, including verified tag peeling.
    pub pack_refs: PackRefsLimits,
    /// Graph traversal and per-object read bounds.
    pub history: HistoryLimits,
    /// Reference inventory directory-entry budget for graph roots.
    pub reference_entries: usize,
    /// Reference inventory aggregate input-byte budget for graph roots.
    pub reference_bytes: usize,
    /// Verified tag-peeling bounds for each graph root.
    pub peel: PeelLimits,
    /// Maximum bytes read from current shallow metadata before graph admission.
    pub shallow_bytes: usize,
    /// Verified loose-duplicate pruning bounds, including a fresh repository admission.
    pub prune_packed: PrunePackedLimits,
    /// Additive loose packing bounds; its entry budget also bounds automatic loose counting.
    pub loose_pack: LoosePackLimits,
    /// MIDX publication, expiry and additive batch bounds.
    pub incremental_repack: IncrementalRepackLimits,
}

impl Default for MaintenanceLimits {
    fn default() -> Self {
        let packs = PackLimits::default().with_max_index_bytes(512 * 1024 * 1024);
        let mut prune_packed =
            PrunePackedLimits::default().with_selection(PrunePackedSelection::Prefix {
                max_objects: 50_000,
            });
        prune_packed.packs = packs;
        Self {
            packs,
            alternates: AlternateLimits::default(),
            pack_refs: PackRefsLimits::default(),
            history: HistoryLimits::default()
                .with_max_commits(2_000_000)
                .with_max_parents(8_000_000),
            reference_entries: 100_000,
            reference_bytes: 16 * 1024 * 1024,
            peel: PeelLimits::default(),
            shallow_bytes: 16 * 1024 * 1024,
            prune_packed,
            loose_pack: LoosePackLimits::default(),
            incremental_repack: IncrementalRepackLimits::default(),
        }
    }
}

impl MaintenanceLimits {
    /// Replaces incremental-repack task bounds.
    #[must_use]
    pub fn with_incremental_repack(mut self, value: IncrementalRepackLimits) -> Self {
        self.incremental_repack = value;
        self
    }

    /// Replaces [`Self::packs`].
    #[must_use]
    pub fn with_packs(mut self, value: PackLimits) -> Self {
        self.packs = value;
        self
    }

    /// Replaces [`Self::alternates`].
    #[must_use]
    pub fn with_alternates(mut self, value: AlternateLimits) -> Self {
        self.alternates = value;
        self
    }

    /// Replaces [`Self::pack_refs`].
    #[must_use]
    pub fn with_pack_refs(mut self, value: PackRefsLimits) -> Self {
        self.pack_refs = value;
        self
    }

    /// Replaces [`Self::history`].
    #[must_use]
    pub fn with_history(mut self, value: HistoryLimits) -> Self {
        self.history = value;
        self
    }

    /// Replaces [`Self::reference_entries`].
    #[must_use]
    pub fn with_reference_entries(mut self, value: usize) -> Self {
        self.reference_entries = value;
        self
    }

    /// Replaces [`Self::reference_bytes`].
    #[must_use]
    pub fn with_reference_bytes(mut self, value: usize) -> Self {
        self.reference_bytes = value;
        self
    }

    /// Replaces [`Self::peel`].
    #[must_use]
    pub fn with_peel(mut self, value: PeelLimits) -> Self {
        self.peel = value;
        self
    }

    /// Replaces [`Self::shallow_bytes`].
    #[must_use]
    pub fn with_shallow_bytes(mut self, value: usize) -> Self {
        self.shallow_bytes = value;
        self
    }

    /// Replaces verified duplicate-pruning bounds.
    #[must_use]
    pub fn with_prune_packed(mut self, value: PrunePackedLimits) -> Self {
        self.prune_packed = value;
        self
    }

    /// Replaces additive packing and automatic loose-inventory bounds.
    #[must_use]
    pub fn with_loose_pack(mut self, value: LoosePackLimits) -> Self {
        self.loose_pack = value;
        self
    }
}

/// Why a selected task did no work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TaskSkip {
    /// The automatic threshold is disabled or has not been reached.
    NotDue,
    /// `core.commitGraph=false` disables graph use and writing.
    CommitGraphDisabled,
    /// Graph writing is unsupported for shallow repositories.
    Shallow,
    /// No reference reaches a commit.
    NoCommits,
}

/// Result of a selected task. The default means it has not been attempted.
#[derive(Debug, Default)]
#[non_exhaustive]
pub enum TaskOutcome {
    /// No attempt completed; inspect a failure's cause before retrying.
    #[default]
    NotStarted,
    /// The task was skipped for the stated reason.
    Skipped(TaskSkip),
    /// Controlled reference packing completed, including its retained-reference report.
    /// In automatic mode, `rewritten=false` reports that its threshold was not reached.
    PackRefs(PackRefsReport),
    /// A commit graph was published.
    CommitGraph(GraphInstallReport),
    /// Verified duplicate pruning followed by additive packing, including completed earlier phases
    /// when a later phase fails. The failure source retains the failed primitive's partial report.
    LooseObjects(Box<LooseObjectsReport>),
    /// Completed MIDX write, expiry and additive repack phases, retained on later failure.
    /// The failure source retains the failed primitive's partial report.
    IncrementalRepack(Box<IncrementalRepackReport>),
}

/// One selected task and its result, in execution order.
#[derive(Debug)]
#[non_exhaustive]
pub struct TaskReport {
    /// Explicitly selected task.
    pub task: MaintenanceTask,
    /// Completed or skipped work; initially `NotStarted`.
    pub outcome: TaskOutcome,
}

/// Batch results. The default contains no selected or attempted tasks.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct MaintenanceReport {
    /// All selected tasks, including those not reached after failure.
    pub tasks: Vec<TaskReport>,
}

/// A batch failed; earlier completed tasks remain authoritative.
///
/// The source may itself contain a primitive's partial-publication report. Isolation remains
/// held while the maintenance lock is released; a cleanup failure does not roll back storage.
#[derive(Debug, thiserror::Error)]
#[error("maintenance failed: {source}")]
#[non_exhaustive]
pub struct MaintenanceFailure {
    /// Results accumulated before failure.
    pub report: Box<MaintenanceReport>,
    /// Task whose execution failed; absent for preflight, isolation, lock or batch cancellation.
    pub failed_task: Option<MaintenanceTask>,
    /// A secondary lock-release failure when a task already failed.
    /// A primary lock-release failure is instead retained in `source`.
    pub cleanup_error: Option<Box<super::LockError>>,
    /// Failure category and underlying diagnostic.
    #[source]
    pub source: MaintenanceRunError,
}

/// Why explicit execution stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MaintenanceRunError {
    /// The selection includes an executor that is not implemented.
    #[error("maintenance task '{0}' cannot be executed yet")]
    Unsupported(MaintenanceTask),
    /// No explicit task was selected.
    #[error("maintenance requires explicit task selection")]
    Selection,
    /// Cancellation was observed between bounded phases.
    /// Cancellation within a primitive remains in `Primitive` to preserve its partial report.
    #[error("maintenance cancelled")]
    Cancelled,
    /// A task exceeded a runner-level work budget.
    #[error("maintenance limit exceeded: {0}")]
    Limit(&'static str),
    /// Caller isolation could not be acquired.
    #[error("cannot acquire maintenance isolation")]
    Isolation(#[source] std::io::Error),
    /// A composed primitive failed; inspect its typed source and partial report.
    #[error("maintenance primitive failed")]
    Primitive(#[source] Box<crate::Error>),
    /// A task's input metadata could not be inspected.
    #[error("cannot inspect maintenance input {path}")]
    InputIo {
        /// Input whose directory or metadata operation failed.
        path: PathBuf,
        /// Underlying filesystem diagnostic.
        #[source]
        source: std::io::Error,
    },
    /// A canonical loose-object name or fanout is not regular storage.
    #[error("maintenance refuses non-regular loose storage: {0}")]
    NonRegularLoose(PathBuf),
}

impl MaintenanceRunError {
    fn primitive(error: impl Into<crate::Error>) -> Self {
        Self::Primitive(Box::new(error.into()))
    }
}

/// An explicitly selected batch; construction performs no repository I/O.
///
/// Pack-refs, commit-graph, loose-objects and (on Unix) incremental-repack execute.
/// Other selected tasks are refused before
/// acquiring isolation or a lock. This never reads enabled flags, strategy or `maintenance.auto`,
/// starts background work, or inserts command triggers. The caller supplies a repository/config
/// snapshot and isolation excluding concurrent changes for the complete batch.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::maintenance::{MaintenanceRun, MaintenanceTask, TaskOutcome, TaskSelection};
/// use girt::retention::MaintenanceIsolation;
/// use girt::{InitKind, ObjectFormat, Repository};
/// struct ExclusiveFixture;
/// impl MaintenanceIsolation for ExclusiveFixture {
///     type Guard = ();
///     fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
///         // Only this example accesses the disposable repository.
///         Ok(())
///     }
/// }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let id = repo.loose_objects().write_blob(b"unreachable payload")?;
/// let tasks = TaskSelection::explicit(&[MaintenanceTask::LooseObjects])?;
/// let cancel = AtomicBool::new(false);
/// let report = MaintenanceRun::new(&repo, tasks.clone()).run(&mut ExclusiveFixture, &cancel)?;
/// let TaskOutcome::LooseObjects(phases) = &report.tasks[0].outcome else {
///     return Err("unexpected task outcome".into());
/// };
/// assert_eq!(phases.pack.as_ref().unwrap().selected, [id]);
/// assert_eq!(
///     repo.loose_objects().read_blob(id, 1024)?,
///     b"unreachable payload"
/// );
///
/// // A later pass removes the loose duplicate, keeping the verified packed copy.
/// MaintenanceRun::new(&repo, tasks).run(&mut ExclusiveFixture, &cancel)?;
/// assert!(repo.objects()?.read(id)?.is_some());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct MaintenanceRun<'a> {
    repo: &'a Repository,
    selection: TaskSelection,
    limits: MaintenanceLimits,
    auto: bool,
}

impl<'a> MaintenanceRun<'a> {
    /// Selects a batch with default finite limits and unconditional task execution.
    pub fn new(repo: &'a Repository, selection: TaskSelection) -> Self {
        Self {
            repo,
            selection,
            limits: MaintenanceLimits::default(),
            auto: false,
        }
    }

    /// Replaces the per-task work budgets.
    #[must_use]
    pub fn with_limits(mut self, limits: MaintenanceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Checks each selected task's automatic threshold when true (default false).
    #[must_use]
    pub fn with_auto(mut self, auto: bool) -> Self {
        self.auto = auto;
        self
    }

    /// Executes the selected tasks under one caller guard and `objects/maintenance.lock`.
    ///
    /// Graph roots come from references, including jj keep refs, rather than detached HEAD.
    /// Config-disabled graphs and shallow repositories are skipped. Existing object/ref data is
    /// never pruned by the graph task. Reference packing uses its existing controlled primitive.
    /// Loose-objects prunes verified duplicates before packing; newly packed loose copies survive
    /// until a later run. Its automatic inclusive threshold counts all canonical regular loose
    /// files, including indexed duplicates. Failed primitives retain partial reports in the source;
    /// the task outcome preserves completed earlier phases.
    ///
    /// # Errors
    ///
    /// Refuses unsupported/empty selections and pre-cancellation before isolation. Lock contention,
    /// invalid consulted settings, exhausted budgets and primitive failures return partial results.
    /// A failed task may have published data: inspect its source report before a fresh attempt.
    pub fn run<I: MaintenanceIsolation>(
        self,
        isolation: &mut I,
        cancel: &AtomicBool,
    ) -> Result<MaintenanceReport, MaintenanceFailure> {
        let mut report = MaintenanceReport {
            tasks: self
                .selection
                .tasks()
                .iter()
                .map(|&task| TaskReport {
                    task,
                    outcome: TaskOutcome::default(),
                })
                .collect(),
        };
        let mut failed_task = None;
        let mut cleanup_error = None;
        let mut operation = || -> Result<(), MaintenanceRunError> {
            if self.selection.tasks().is_empty() {
                return Err(MaintenanceRunError::Selection);
            }
            for &task in self.selection.tasks() {
                if (task == MaintenanceTask::IncrementalRepack && !cfg!(unix))
                    || !matches!(
                        task,
                        MaintenanceTask::PackRefs
                            | MaintenanceTask::CommitGraph
                            | MaintenanceTask::LooseObjects
                            | MaintenanceTask::IncrementalRepack
                    )
                {
                    return Err(MaintenanceRunError::Unsupported(task));
                }
            }
            checkpoint(cancel)?;
            let _isolation = isolation
                .acquire(self.repo)
                .map_err(MaintenanceRunError::Isolation)?;
            let lock = self
                .repo
                .try_lock_maintenance()
                .map_err(MaintenanceRunError::primitive)?;
            let result = (|| {
                for task in &mut report.tasks {
                    checkpoint(cancel)?;
                    failed_task = Some(task.task);
                    if task.task == MaintenanceTask::LooseObjects {
                        loose::run(&self, &_isolation, &mut task.outcome, cancel)?;
                    } else if task.task == MaintenanceTask::IncrementalRepack {
                        incremental::run(&self, &_isolation, &mut task.outcome, cancel)?;
                    } else {
                        task.outcome = self.task(task.task, cancel)?;
                    }
                    failed_task = None;
                }
                Ok(())
            })();
            match (result, lock.release()) {
                (Err(source), Err(cleanup)) => {
                    cleanup_error = Some(Box::new(cleanup));
                    Err(source)
                }
                (Err(source), Ok(())) => Err(source),
                (Ok(()), Err(cleanup)) => Err(MaintenanceRunError::primitive(cleanup)),
                (Ok(()), Ok(())) => Ok(()),
            }
        };
        match operation() {
            Ok(()) => Ok(report),
            Err(source) => Err(MaintenanceFailure {
                report: Box::new(report),
                failed_task,
                cleanup_error,
                source,
            }),
        }
    }

    fn task(
        &self,
        task: MaintenanceTask,
        cancel: &AtomicBool,
    ) -> Result<TaskOutcome, MaintenanceRunError> {
        if task == MaintenanceTask::CommitGraph {
            return self.graph(cancel);
        }
        let objects = self
            .repo
            .objects_controlled(self.limits.packs, self.limits.alternates, cancel)
            .map_err(MaintenanceRunError::primitive)?;
        let refs = self
            .repo
            .references()
            .map_err(MaintenanceRunError::primitive)?;
        let options = if self.auto {
            PackRefsOptions::AUTO
        } else {
            PackRefsOptions::ALL
        };
        refs.pack_controlled(&objects, options, self.limits.pack_refs, cancel)
            .map(TaskOutcome::PackRefs)
            .map_err(MaintenanceRunError::primitive)
    }

    fn graph(&self, cancel: &AtomicBool) -> Result<TaskOutcome, MaintenanceRunError> {
        if self
            .repo
            .config()
            .boolean("core", None, "commitGraph")
            .map_err(MaintenanceRunError::primitive)?
            == Some(false)
        {
            return Ok(TaskOutcome::Skipped(TaskSkip::CommitGraphDisabled));
        }
        let shallow = crate::ShallowRoots::read(
            self.repo.common_dir().join("shallow"),
            self.repo.object_format(),
            self.limits.shallow_bytes,
            cancel,
        )
        .map_err(MaintenanceRunError::primitive)?;
        if !shallow.is_empty() {
            return Ok(TaskOutcome::Skipped(TaskSkip::Shallow));
        }
        let threshold = if self.auto {
            MaintenanceConfig::new(self.repo.config())
                .auto_threshold(MaintenanceTask::CommitGraph)
                .map_err(MaintenanceRunError::primitive)?
        } else {
            None
        };
        if threshold == Some(AutoThreshold::Disabled) {
            return Ok(TaskOutcome::Skipped(TaskSkip::NotDue));
        }
        let refs = self
            .repo
            .references()
            .map_err(MaintenanceRunError::primitive)?;
        let listed = refs
            .list_controlled(
                self.limits.reference_entries,
                self.limits.reference_bytes,
                cancel,
            )
            .map_err(MaintenanceRunError::primitive)?;
        let objects = self
            .repo
            .objects_controlled(self.limits.packs, self.limits.alternates, cancel)
            .map_err(MaintenanceRunError::primitive)?;
        let mut roots = Vec::new();
        for reference in &listed {
            checkpoint(cancel)?;
            let mut target = &reference.target;
            let mut depth = 0;
            while let Target::Symbolic(name) = target {
                depth += 1;
                if depth > 32 {
                    return Err(MaintenanceRunError::Limit("symbolic reference depth"));
                }
                let Ok(position) = listed.binary_search_by(|entry| entry.name.cmp(name)) else {
                    break;
                };
                target = &listed[position].target;
            }
            if let Target::Direct(id) = target {
                let peeled = objects
                    .peel(*id, self.limits.peel, cancel)
                    .map_err(MaintenanceRunError::primitive)?;
                if peeled.kind == ObjectKind::Commit {
                    roots.push(peeled.target);
                }
            }
        }
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            return Ok(TaskOutcome::Skipped(TaskSkip::NoCommits));
        }
        if let Some(AutoThreshold::Count(count)) = threshold {
            let old = self
                .repo
                .commit_graph()
                .map_err(MaintenanceRunError::primitive)?;
            let mut pending = roots.clone();
            let mut seen = HashSet::new();
            let mut parents = 0usize;
            while let Some(id) = pending.pop() {
                checkpoint(cancel)?;
                if old.as_ref().is_some_and(|graph| graph.get(id).is_some()) || seen.contains(&id) {
                    continue;
                }
                if seen.len() == self.limits.history.max_commits {
                    return Err(MaintenanceRunError::Limit("commits"));
                }
                seen.insert(id);
                if seen.len() >= count as usize {
                    break;
                }
                let commit = objects
                    .read_commit_with(id, self.limits.history.read)
                    .map_err(MaintenanceRunError::primitive)?;
                parents = parents
                    .checked_add(commit.parents().len())
                    .ok_or(MaintenanceRunError::Limit("parents"))?;
                if parents > self.limits.history.max_parents {
                    return Err(MaintenanceRunError::Limit("parents"));
                }
                pending.extend_from_slice(commit.parents());
            }
            if seen.len() < count as usize {
                return Ok(TaskOutcome::Skipped(TaskSkip::NotDue));
            }
        }
        checkpoint(cancel)?;
        let graph =
            CommitGraph::from_objects_controlled(&objects, &roots, self.limits.history, cancel)
                .map_err(MaintenanceRunError::primitive)?;
        checkpoint(cancel)?;
        graph
            .install(self.repo.object_dir(), GraphWriteMode::Incremental)
            .map(TaskOutcome::CommitGraph)
            .map_err(MaintenanceRunError::primitive)
    }
}

fn checkpoint(cancel: &AtomicBool) -> Result<(), MaintenanceRunError> {
    if cancel.load(Ordering::Relaxed) {
        Err(MaintenanceRunError::Cancelled)
    } else {
        Ok(())
    }
}

// Only constructed from the batch's live guard, for the same repository's primitives.
struct BorrowedIsolation<'a, G> {
    _guard: &'a G,
}
impl<G> MaintenanceIsolation for BorrowedIsolation<'_, G> {
    type Guard = ();
    fn acquire(&mut self, _: &Repository) -> std::io::Result<()> {
        Ok(())
    }
}
