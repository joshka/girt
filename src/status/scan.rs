use std::sync::atomic::AtomicBool;

use super::{Baseline, Error, Limits, Report, UntrackedFiles};
use crate::ignore::WorktreeRules;
use crate::pathspec::Pathspec;
use crate::{Repository, filter, fsmonitor};

/// A configured status observation with optional filters, path selection and monitor queries.
///
/// This composes the same walk as [`Repository::status_with_filters`]. Existing status methods
/// remain read-only and never query a hook. Adding [`Self::with_monitor`] explicitly opts into a
/// monitor query under the monitor's own execution policy. Failed or reset checkpoints cause a
/// full scan; a valid checkpoint and clear FSMN bit permit trusting an unchanged tracked entry
/// without a stat call. A monitor that omits a changed path can therefore hide changes, as in Git.
/// The report is observational and never authorizes overwriting working files.
///
/// ```
/// use girt::status::{Baseline, Scan, UntrackedFiles};
/// use girt::workflow::Environment;
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Worktree,
/// )?;
/// let rules = repo.ignore_rules(&Environment::default())?;
///
/// let report = Scan::new(&repo, Baseline::Head, UntrackedFiles::Collapsed, &rules).run()?;
/// assert!(report.unstaged.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct Scan<'a> {
    repo: &'a Repository,
    baseline: Baseline,
    untracked: UntrackedFiles,
    ignore: &'a WorktreeRules,
    paths: Option<&'a Pathspec>,
    filters: Option<&'a mut filter::Pipeline>,
    monitor: Option<&'a fsmonitor::Monitor>,
}
impl<'a> Scan<'a> {
    /// Selects a repository, staged baseline, untracked listing and caller-loaded ignore rules.
    ///
    /// Construction performs no I/O. Paths default to all paths, filters and monitor queries to
    /// disabled. The operation does not write the index.
    pub fn new(
        repo: &'a Repository,
        baseline: Baseline,
        untracked: UntrackedFiles,
        ignore: &'a WorktreeRules,
    ) -> Self {
        Self {
            repo,
            baseline,
            untracked,
            ignore,
            paths: None,
            filters: None,
            monitor: None,
        }
    }
    /// Limits inspection and reporting to this pathspec, as in [`Repository::status_with`].
    #[must_use]
    pub fn with_pathspec(mut self, paths: &'a Pathspec) -> Self {
        self.paths = Some(paths);
        self
    }
    /// Converts working files through this pipeline when the walk must read them.
    #[must_use]
    pub fn with_filters(mut self, filters: &'a mut filter::Pipeline) -> Self {
        self.filters = Some(filters);
        self
    }
    /// Queries this worktree's monitor before inspecting working files.
    ///
    /// The monitor's execution policy remains authoritative. Dirty bits and changed paths override
    /// clean promises; a missing, corrupt, incompatible or reset checkpoint forces a full scan.
    /// Split indexes and non-exact name policies also use full stat checks.
    #[must_use]
    pub fn with_monitor(mut self, monitor: &'a fsmonitor::Monitor) -> Self {
        self.monitor = Some(monitor);
        self
    }
    /// Runs with default limits and a cancellation flag that is never set.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::run_with`] except cancellation.
    pub fn run(self) -> Result<Report, Error> {
        self.run_with(Limits::default(), &AtomicBool::new(false))
    }
    /// Runs the configured walk, preserving its path safety, filters and final storage checks.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Repository::status_with_filters`], plus [`Error::Fsmonitor`] when
    /// monitor execution is refused or the supplied monitor belongs to another working tree.
    /// Hook execution/output failures fall back to a full scan. Cancellation writes nothing.
    /// Non-macOS/Linux hosts return unsupported before starting a hook or accessing storage.
    pub fn run_with(self, limits: Limits, cancel: &AtomicBool) -> Result<Report, Error> {
        super::types::check(cancel)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let all = Pathspec::all();
            super::workflow::supported::run_with_checkpoint(
                self.repo,
                self.baseline,
                super::worktree::Listing::Ignoring {
                    rules: Box::new(self.ignore.base().clone()),
                    files: self.untracked,
                    names: super::workflow::supported::git_names(self.repo)?,
                    environment: Box::new(self.ignore.environment().clone()),
                    monitor: self.monitor.cloned().map(Box::new),
                    excludes: Some(self.ignore.untracked_excludes(self.repo.object_format())),
                },
                self.paths.unwrap_or(&all),
                self.filters,
                limits,
                cancel,
                || {},
            )
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (
                self.repo,
                self.baseline,
                self.untracked,
                self.ignore,
                self.paths,
                self.filters,
                self.monitor,
                limits,
            );
            Err(Error::Unsupported {
                path: Vec::new(),
                reason: "descriptor-relative worktree traversal requires macOS/Linux",
            })
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn monitor_clean(
    repo: &Repository,
    index: Option<&crate::index::Index>,
    monitor: &fsmonitor::Monitor,
    limits: crate::index::Limits,
    cancel: &AtomicBool,
) -> Result<Vec<bool>, Error> {
    use std::collections::HashSet;
    if repo.worktree() != Some(monitor.worktree()) {
        return Err(Error::Fsmonitor(Box::new(fsmonitor::Error::Unsupported(
            "monitor belongs to another working tree",
        ))));
    }

    // Hook paths use filesystem spellings. Until notification matching implements Git's
    // configured alias rules, only exact-byte names may carry clean promises. Split-index
    // bitmap positions likewise require a checkpoint bound to the merged index ordering.
    let exact =
        super::workflow::supported::git_names(repo)? == super::worktree::GitNames::default();
    let standalone = !index.is_some_and(|index| {
        index
            .extensions()
            .iter()
            .any(|extension| extension.signature() == *b"link")
    });
    let cache = index
        .filter(|_| exact && standalone)
        .and_then(|index| index.fsmonitor_cache(limits).ok().flatten());

    let changes = match monitor.query_with(
        cache.as_ref().map(crate::index::FsmonitorCache::token),
        Default::default(),
        cancel,
    ) {
        Ok(changes) => changes,
        Err(fsmonitor::Error::Cancelled) => return Err(Error::Cancelled),
        Err(
            error @ (fsmonitor::Error::Refused
            | fsmonitor::Error::Unsupported(_)
            | fsmonitor::Error::Config(_)),
        ) => return Err(Error::Fsmonitor(Box::new(error))),
        Err(
            fsmonitor::Error::Io(_)
            | fsmonitor::Error::Failed
            | fsmonitor::Error::Response(_)
            | fsmonitor::Error::Limit(_),
        ) => return Ok(Vec::new()),
    };

    let (Some(index), Some(cache)) = (index, cache) else {
        return Ok(Vec::new());
    };

    if changes.full_scan() {
        return Ok(Vec::new());
    }

    let paths: HashSet<&[u8]> = changes.paths().iter().map(Vec::as_slice).collect();

    let mut clean = vec![true; index.entries().len()];

    for (position, entry) in index.entries().iter().enumerate() {
        super::types::check(cancel)?;
        let changed = paths.contains(entry.path.as_slice())
            || entry.path.iter().enumerate().any(|(offset, byte)| {
                *byte == b'/'
                    && (paths.contains(&entry.path[..offset])
                        || paths.contains(&entry.path[..=offset]))
            });

        if changed || cache.dirty().binary_search(&position).is_ok() {
            clean[position] = false;
        }
    }

    Ok(clean)
}
