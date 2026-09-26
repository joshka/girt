use std::fs;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{BranchSelection, CloneHead, ClonePlan, ClonePlanError, config, plan};
use crate::fetch::{
    Advertisement, FetchError, FetchFinishError, FetchLimits, FetchPlanError, FetchReady,
    FetchReport, FetchRequest, FetchUpdateLimits, ReceivedFetch,
};
use crate::refs::{Expected, RefEdit, RefEditOutcome, RefName, Reflog, Target};
use crate::transport::TransportControl;
use crate::{InitKind, ObjectKind, Repository};

/// Destination and explicit policy for a full clone, without checkout.
///
/// The destination must not exist, even as an empty directory or dangling symlink; its parent
/// must exist. Preparation checks this without reserving the path. Finish exclusively reserves
/// it again. Callers must exclude path replacement, other writers, checkout and GC during finish.
/// Parent symlinks follow filesystem resolution; this is not an untrusted-path security boundary.
///
/// The stored URL is caller-selected metadata, separate from the actual transport endpoint and
/// credentials. Supply a credential-free URL usable by future callers; relative local paths are
/// interpreted relative to the new repository by Git, so prefer an absolute path. No endpoint
/// discovery, credential persistence/discovery, retry, pruning, implicit force, or runtime is
/// provided. `origin` is the fixed remote name. All branch/tag refspecs are unforced; subsequent
/// fetch callers supply any force intent and authorization explicitly.
///
/// Direct refs use the supplied reflog policy; symbolic HEAD uses `Reflog::Preserve` because
/// stored symbolic replacements cannot append logs. No `origin/HEAD` alias is created. No index,
/// checkout, shallow/filter, alternates, hardlinks, mirror or submodule behavior is supported.
/// Finish requires the existing reference backend. Native local and optional SSH adapters
/// require macOS/Linux; HTTP is feature-gated and uses the caller's runtime.
///
/// This example needs a local source repository and Git on PATH for upload-pack:
///
/// ```no_run
/// use std::ops::ControlFlow;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::InitKind;
/// use girt::clone::{BranchSelection, CloneRequest};
/// use girt::fetch::{FetchLimits, FetchUpdateLimits};
/// use girt::refs::Reflog;
/// use girt::transport::TransportControl;
///
/// let root = tempfile::tempdir()?;
/// let source = std::fs::canonicalize("/path/to/source")?;
/// let url = source.to_str().ok_or("example needs a UTF-8 path")?;
/// let request = CloneRequest::prepare_tracking(
///     root.path().join("copy"),
///     InitKind::Worktree,
///     url.as_bytes(),
///     BranchSelection::Default,
///     Reflog::Preserve,
/// )?;
/// let cancel = AtomicBool::new(false);
/// let ready = request.receive_local(
///     &source,
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
///     |_| ControlFlow::Continue(()),
/// )?;
/// let report = ready.finish(FetchUpdateLimits::default(), &cancel)?;
/// let repository = report.repository.as_ref().unwrap();
/// assert!(!repository.git_dir().join("index").exists());
/// // Only .git exists: checkout is a separate, later operation.
/// assert_eq!(
///     std::fs::read_dir(repository.worktree().unwrap())?.count(),
///     1
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub struct CloneRequest {
    pub(super) destination: PathBuf,
    pub(super) kind: InitKind,
    url: Vec<u8>,
    pub(super) selection: BranchSelection,
    reflog: Reflog,
}

impl std::fmt::Debug for CloneRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloneRequest")
            .field("destination", &self.destination)
            .field("kind", &self.kind)
            .field("selection", &self.selection)
            .finish_non_exhaustive()
    }
}

impl CloneRequest {
    /// Prepares a clone with all branches in `refs/remotes/origin/*`, all tags, and at most one
    /// local branch. Checks destination absence without creating files or connecting.
    ///
    /// `kind` selects metadata placement only. In particular, `InitKind::Bare` uses this tracking
    /// layout too: enumerating or serving `refs/heads/*` exposes only the selected local branch
    /// (none for detached or unborn HEAD). Conventional bare copies with every remote branch under
    /// `refs/heads/*` are unsupported. Subsequent fetch can update the configured tracking refs
    /// without allowing branch destinations or weakening worktree safeguards.
    ///
    /// `url` preserves bytes through quoted Git configuration. Empty values, NUL and carriage
    /// returns are rejected. The library does not infer whether supplied bytes contain secrets.
    ///
    /// # Errors
    ///
    /// Returns invalid branch/URL policy or filesystem/destination errors without mutation.
    pub fn prepare_tracking(
        destination: impl AsRef<Path>,
        kind: InitKind,
        url: &[u8],
        selection: BranchSelection,
        reflog: Reflog,
    ) -> Result<Self, CloneFailure> {
        if url.is_empty() || url.contains(&0) || url.contains(&b'\r') {
            return Err(CloneFailure::Url);
        }
        if let BranchSelection::Branch(name) = &selection {
            plan::branch(name)?;
        }
        let destination = destination.as_ref().to_path_buf();
        absent(&destination)?;
        Ok(Self {
            destination,
            kind,
            url: url.to_vec(),
            selection,
            reflog,
        })
    }

    /// Previews HEAD and mappings without I/O. Receive always replans from its own advertisement.
    ///
    /// # Errors
    ///
    /// Rejects invalid/missing selection, conflicting mappings or inconsistent symbolic HEAD.
    pub fn plan(&self, advertisement: &Advertisement) -> Result<ClonePlan, ClonePlanError> {
        plan::plan(&self.selection, advertisement)
    }

    /// Downloads and validates a full local upload-pack transfer without creating the destination.
    ///
    /// No local known history is offered. Selection uses this session's advertisement; later
    /// remote ref movement never substitutes another ID. Server refusal/lost objects fail transfer.
    /// See [`crate::fetch::receive_local_with_control`] for process, deadline and progress policy.
    ///
    /// # Errors
    ///
    /// Returns planning/transfer errors with no destination effects. Dropping the result likewise
    /// leaves no destination; completion requires [`CloneReady::finish`].
    pub fn receive_local(
        self,
        source: impl AsRef<Path>,
        limits: FetchLimits,
        control: TransportControl<'_>,
        progress: impl FnMut(&[u8]) -> ControlFlow<()>,
    ) -> Result<CloneReady, CloneTransferError> {
        let mut saved = None;
        let received = crate::fetch::receive_local_with_control(
            source,
            |ad| plan::select(self.plan(ad), &mut saved),
            limits,
            control,
            progress,
        );
        let (plan, received) = selected(saved, received)?;
        Ok(CloneReady {
            request: self,
            head: plan.head,
            received,
        })
    }
}

pub(super) fn selected<T>(
    saved: Option<Result<ClonePlan, ClonePlanError>>,
    transfer: Result<T, FetchError>,
) -> Result<(ClonePlan, T), CloneTransferError> {
    let plan = saved.transpose()?;
    let transfer = transfer?;
    let plan = plan.ok_or(FetchError::Protocol("selection callback was not invoked"))?;
    Ok((plan, transfer))
}

/// A validated full transfer and private advertisement-derived HEAD, ready for synchronous finish.
///
/// This workflow has not created the destination. Move to a caller-managed worker if needed;
/// dropping it only releases
/// memory. The destination/selection/transfer cannot be replaced through this value.
#[derive(Debug)]
pub struct CloneReady {
    pub(super) request: CloneRequest,
    pub(super) head: CloneHead,
    pub(super) received: ReceivedFetch,
}

impl CloneReady {
    /// Advertised initial HEAD; commit-kind checks happen after installation during finish.
    pub fn head(&self) -> &CloneHead {
        &self.head
    }

    /// Creates the destination, installs/publishes tracking refs and tags, writes remote/branch
    /// config, then conditionally publishes the local branch and HEAD (HEAD last).
    ///
    /// Both layouts contain all tracking branches/tags and at most one local branch. No index or
    /// worktree files are written. The returned repository has freshly reopened configuration.
    /// Branch/HEAD tips must be commits (not tags that peel to commits). Fetch validation rereads
    /// installed graphs before publication; all direct branch tips are checked before final HEAD.
    ///
    /// Cancellation is cooperative between phases, not inside config replacement or a reference
    /// transaction. Once publication starts, a completed operation may win cancellation. Join
    /// workers before releasing their resources. Limits retain [`FetchUpdateLimits`]' phase bounds;
    /// metadata transactions are not a hard memory/deadline bound.
    ///
    /// # Errors
    ///
    /// [`CloneError::report`] records destination reservation, initialization, fetch, config and
    /// final refs separately. Initialization failure can leave partial metadata. Fetch failure can
    /// leave an unindexed pack or partial refs/logs; inspect its nested report. Later failure keeps
    /// installed objects, tracking refs/tags and possibly config/local refs. No automatic cleanup,
    /// retry, rollback, whole-operation atomic visibility or crash durability is promised.
    pub fn finish(
        self,
        limits: FetchUpdateLimits,
        cancel: &AtomicBool,
    ) -> Result<CloneReport, CloneError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "clone.finish",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            let mut report = CloneReport {
                destination: self.request.destination.clone(),
                reserved: false,
                initialized: false,
                fetch: None,
                configured: false,
                references: Vec::new(),
                repository: None,
                head: self.head.clone(),
            };
            let result = self.create_finish(limits, cancel, &mut report);
            match result {
                Ok(()) => Ok(report),
                Err(source) => Err(CloneError {
                    report: Box::new(report),
                    source,
                }),
            }
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| {
            crate::trace::clone_finish(error, &span)
        });

        result
    }

    fn create_finish(
        self,
        limits: FetchUpdateLimits,
        cancel: &AtomicBool,
        report: &mut CloneReport,
    ) -> Result<(), CloneFailure> {
        cancelled(cancel)?;
        absent(&self.request.destination)?;
        fs::create_dir(&self.request.destination)?;
        report.reserved = true;
        let repo = Repository::init_reserved_clone(&self.request.destination, self.request.kind)?;
        report.initialized = true;
        self.finish_in(repo, limits, cancel, report)
    }

    fn finish_in(
        self,
        repo: Repository,
        limits: FetchUpdateLimits,
        cancel: &AtomicBool,
        report: &mut CloneReport,
    ) -> Result<(), CloneFailure> {
        let has_head = self
            .received
            .advertisement()
            .refs
            .iter()
            .any(|r| !r.peeled && r.name.as_bytes() == b"HEAD");
        let request = FetchRequest::prepare(
            repo,
            plan::specs(has_head),
            Default::default(),
            self.request.reflog.clone(),
        )?;
        let ready = FetchReady::for_clone(request, self.received)?;
        report.fetch = Some(ready.finish(limits, cancel)?);
        let repo = Repository::open(&self.request.destination)?;
        verify_branches(
            &repo,
            report.fetch.as_ref().unwrap(),
            &self.head,
            limits,
            cancel,
        )?;
        cancelled(cancel)?;
        let bytes = config::contents(self.request.kind, &self.request.url, &self.head);
        config::publish(&repo, self.request.kind, &bytes).map_err(CloneFailure::Configuration)?;
        report.configured = true;
        cancelled(cancel)?;
        report.references = repo
            .references()?
            .transaction(&head_edits(&self.head, &self.request.reflog))?;
        report.repository = Some(Repository::open(&self.request.destination)?);
        Ok(())
    }
}

fn verify_branches(
    repo: &Repository,
    fetch: &FetchReport,
    head: &CloneHead,
    limits: FetchUpdateLimits,
    cancel: &AtomicBool,
) -> Result<(), CloneFailure> {
    let objects = repo
        .objects(limits.snapshot)
        .map_err(FetchError::Destination)?;
    let detached = match head {
        CloneHead::Detached(id) => Some(*id),
        _ => None,
    };
    let branches = fetch
        .updates
        .iter()
        .filter_map(|u| u.mapping.source.as_ref())
        .filter(|s| s.name.as_bytes().starts_with(b"refs/heads/"))
        .map(|s| s.id);
    for id in branches.chain(detached) {
        cancelled(cancel)?;
        let object = objects
            .read(id, limits.verification.known_read)
            .map_err(FetchError::from)?
            .ok_or(FetchError::Missing(id))?;
        if object.kind() != ObjectKind::Commit {
            return Err(FetchError::Kind(id).into());
        }
    }
    Ok(())
}

fn head_edits(head: &CloneHead, reflog: &Reflog) -> Vec<RefEdit> {
    let mut edits = Vec::new();
    let target = match head {
        CloneHead::Branch { name, id } => {
            edits.push(RefEdit {
                name: name.clone(),
                dereference: false,
                target: Some(Target::Direct(*id)),
                expected: Expected::Absent,
                reflog: reflog.clone(),
            });
            Target::Symbolic(name.clone())
        }
        CloneHead::Detached(id) => Target::Direct(*id),
        CloneHead::Unborn(name) => Target::Symbolic(name.clone()),
    };
    let head_reflog = if matches!(target, Target::Direct(_)) {
        reflog.clone()
    } else {
        Reflog::Preserve
    };
    edits.push(RefEdit {
        name: RefName::new("HEAD").unwrap(),
        dereference: false,
        target: Some(target),
        expected: Expected::Value(Target::Symbolic(crate::repository::initial_branch())),
        reflog: head_reflog,
    });
    edits
}

fn absent(path: &Path) -> Result<(), CloneFailure> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(CloneFailure::Exists(path.into())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
fn cancelled(cancel: &AtomicBool) -> Result<(), CloneFailure> {
    if cancel.load(Ordering::Relaxed) {
        return Err(FetchError::Cancelled.into());
    }
    Ok(())
}

/// Completed phases, including on failure. No field implies rollback or crash durability.
#[derive(Debug)]
pub struct CloneReport {
    /// Requested path; inspect here for residual files after failure.
    pub destination: PathBuf,
    /// Whether this operation reserved the root; false does not authorize cleanup of any path.
    pub reserved: bool,
    /// Minimal initialization completed; otherwise partial metadata can remain if reserved.
    pub initialized: bool,
    /// Successful fetch installation/publication. On fetch failure consult the nested error.
    pub fetch: Option<FetchReport>,
    /// Complete remote/branch config replaced initialization config.
    pub configured: bool,
    /// Successful local branch/HEAD transaction. On error inspect the nested transaction outcomes.
    pub references: Vec<RefEditOutcome>,
    /// Reopened repository with persisted config, present only on complete success.
    pub repository: Option<Repository>,
    /// Planned HEAD, not evidence of its publication.
    pub head: CloneHead,
}

/// Transfer or planning failure with no destination effects.
#[derive(Debug, thiserror::Error)]
pub enum CloneTransferError {
    /// Advertisement/default branch could not be selected.
    #[error(transparent)]
    Plan(#[from] ClonePlanError),
    /// Protocol, transport, validation, cancellation or resource limit failure.
    #[error(transparent)]
    Transfer(#[from] FetchError),
}

/// Finish failure preserving completed phases and precise nested publication effects.
#[derive(Debug, thiserror::Error)]
#[error("{source}")]
pub struct CloneError {
    /// Completed phases; residual state is never automatically removed.
    pub report: Box<CloneReport>,
    /// Failed operation.
    #[source]
    pub source: CloneFailure,
}

/// Preparation or finish phase failure; URL contents are omitted from diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum CloneFailure {
    /// Existing files/directories/symlinks are not accepted.
    #[error("clone destination already exists: {0}")]
    Exists(PathBuf),
    /// Stored URL must be nonempty without NUL or carriage return.
    #[error("invalid stored clone URL")]
    Url,
    /// Branch/advertisement policy failed.
    #[error(transparent)]
    Plan(#[from] ClonePlanError),
    /// Destination reservation I/O failed.
    #[error("clone destination: {0}")]
    Io(#[from] std::io::Error),
    /// Minimal initialization failed; partial metadata may remain.
    #[error(transparent)]
    Initialization(#[from] crate::InitError),
    /// Repository reopening failed.
    #[error(transparent)]
    Open(#[from] crate::OpenError),
    /// Fetch preparation failed after initialization.
    #[error(transparent)]
    FetchPlan(#[from] FetchPlanError),
    /// Fetch install/publication failed; includes its own partial-state report.
    #[error(transparent)]
    Fetch(#[from] FetchFinishError),
    /// Cancellation or installed branch-kind verification failed.
    #[error(transparent)]
    Verification(#[from] FetchError),
    /// Config lock, precondition, write or replacement failed; clone did not replace the config.
    #[error("clone configuration: {0}")]
    Configuration(#[source] std::io::Error),
    /// Reference store could not be opened.
    #[error(transparent)]
    Reference(#[from] crate::refs::ReferenceError),
    /// Final branch/HEAD transaction failed, retaining per-ref/log partial effects.
    #[error(transparent)]
    Publication(#[from] crate::refs::TransactionError),
}

#[cfg(test)]
#[path = "workflow_tests.rs"]
mod tests;
