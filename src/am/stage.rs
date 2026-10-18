use std::sync::atomic::{AtomicBool, Ordering};

use super::{Error, State};
use crate::apply::{IndexLimits, TreeOptions, WorktreeLimits, WorktreeReport};
use crate::{OperationState, Repository, filter, pathspec, status};

/// Independent bounds for each preparation and verification pass.
///
/// Status runs twice against the full tracked tree. Existing planner bounds apply separately;
/// these are not a single aggregate memory/work allowance. Filter loading retains its own defaults.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct ApplyLimits {
    /// Extracted patch bounds. `max_input_bytes` includes framing; line/hunk bounds cover content.
    pub patch: crate::apply::Limits,
    /// Index draft and generated tree bounds.
    pub index: IndexLimits,
    /// Working-file snapshots, conversion and publication bounds.
    pub worktree: WorktreeLimits,
    /// Full tracked cleanliness scan bounds, per pass.
    pub status: status::Limits,
}
impl ApplyLimits {
    /// Sets [`Self::patch`].
    #[must_use]
    pub fn with_patch(mut self, value: crate::apply::Limits) -> Self {
        self.patch = value;
        self
    }
    /// Sets [`Self::index`].
    #[must_use]
    pub fn with_index(mut self, value: IndexLimits) -> Self {
        self.index = value;
        self
    }
    /// Sets [`Self::worktree`].
    #[must_use]
    pub fn with_worktree(mut self, value: WorktreeLimits) -> Self {
        self.worktree = value;
        self
    }
    /// Sets [`Self::status`].
    #[must_use]
    pub fn with_status(mut self, value: status::Limits) -> Self {
        self.status = value;
        self
    }
}

/// Staging failed, retaining every confirmed working-file mutation.
///
/// Preparation failures have an empty report. Additive immutable objects may remain after object
/// installation starts. A working publication failure can leave partial leaves; a later index
/// failure leaves completed working changes without installing the new index. HEAD and operation
/// metadata are never changed. Inspect actual files and this report before any recovery attempt.
#[derive(Debug, thiserror::Error)]
#[error("staging the current am patch failed")]
#[non_exhaustive]
pub struct ApplyFailure {
    /// Typed underlying preparation or publication failure.
    #[source]
    pub source: crate::Error,
    /// Confirmed working-file changes, including incomplete created leaves.
    pub worktree: WorktreeReport,
}

/// Stages the current extracted Git-mail patch into working files and the index.
///
/// Requires a repository-bound, unchanged operation snapshot with an active `am` mail and extracted
/// `patch`. Diffstat preambles and the signature after completed edits are ignored; only Git-style
/// `diff --git` patches are accepted. Signature-shaped hunk content remains data. No mail is
/// advanced and no commit or reference is written. After success Git can run `am --continue`.
///
/// The full tracked index/working state must be raw-clean against HEAD. Gitlinks, missing indexes,
/// nested boundaries and normalized-only cleanliness are refused. The existing working publisher
/// supplies platform/path/mode restrictions. Filters use a fixed attribute view with external
/// commands refused; patches do not reload changed attributes midway.
///
/// The index lock is held throughout. Callers must exclude other operation, HEAD and working-file
/// writers and ancestor renames for the entire call. Rechecks detect observed changes, not races
/// after checks. Objects are installed first, working files second, and index last. This is not a
/// batch transaction or crash-durable journal; no automatic rollback overwrites independent work.
///
/// # Errors
///
/// Returns [`ApplyFailure`] on cancellation, malformed/unsupported state or patch, dirty tracked
/// state, bounds, filters, path obstruction, changed snapshots or storage failures. Late failures
/// preserve exact working reports and leave am metadata for explicit recovery. A returned failure
/// does not authorize Git continuation.
pub fn apply_current(
    repo: &Repository,
    snapshot: &OperationState,
    limits: ApplyLimits,
    cancel: &AtomicBool,
) -> Result<WorktreeReport, ApplyFailure> {
    apply_with_hook(repo, snapshot, limits, cancel, || {})
}

pub(super) fn apply_with_hook(
    repo: &Repository,
    snapshot: &OperationState,
    limits: ApplyLimits,
    cancel: &AtomicBool,
    before_index: impl FnOnce(),
) -> Result<WorktreeReport, ApplyFailure> {
    let mut report = WorktreeReport::default();
    let result = (|| -> crate::Result<()> {
        check(cancel)?;
        snapshot.verify_for(repo)?;
        let state = State::from_operation(snapshot)?.ok_or(Error::NotReady("no am operation"))?;
        if state.mail.is_none() {
            return Err(Error::NotReady("completed mail range").into());
        }
        let bytes = state.patch.ok_or(Error::NotReady("patch not prepared"))?;
        let patch = crate::apply::parse::parse_mail(bytes, limits.patch)?;
        let mut edit = repo.edit_index(limits.index.index)?;
        clean(repo, limits.status, cancel)?;
        let applied = patch.apply_index_with(
            &repo.objects()?,
            edit.index(),
            TreeOptions::default(),
            limits.index,
            cancel,
        )?;
        let mut filters = repo.filters(&Default::default(), filter::Commands::Refuse)?;
        let working = patch.prepare_worktree_with(
            repo,
            &mut filters,
            TreeOptions::default(),
            limits.worktree,
            cancel,
        )?;
        let (index, tree) = applied.into_parts();
        edit.replace_index(index)?;
        snapshot.verify_for(repo)?;
        clean(repo, limits.status, cancel)?;
        check(cancel)?;
        tree.install_objects(&repo.loose_objects())?;
        report = match working.publish_with(cancel) {
            Ok(report) => report,
            Err(failure) => {
                report = failure.report;
                return Err((*failure.cause).into());
            }
        };
        before_index();
        edit.commit()?;
        Ok(())
    })();
    match result {
        Ok(()) => Ok(report),
        Err(source) => Err(ApplyFailure {
            source,
            worktree: report,
        }),
    }
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
fn clean(repo: &Repository, limits: status::Limits, cancel: &AtomicBool) -> crate::Result<()> {
    let report = repo.raw_status_with(
        status::Baseline::Head,
        status::Untracked::Omit,
        &pathspec::Pathspec::all(),
        limits,
        cancel,
    )?;
    if report.index_missing
        || !report.staged.is_empty()
        || !report.unstaged.is_empty()
        || !report.unmerged.is_empty()
        || !report.unchecked_gitlinks.is_empty()
        || !report.boundaries.is_empty()
    {
        return Err(Error::NotReady("tracked state is not raw-clean").into());
    }
    Ok(())
}
