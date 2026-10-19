//! Controlled admission for ordinary fetch completion.
use std::sync::atomic::AtomicBool;

use super::{
    FetchFinishError, FetchFinishFailure, FetchPlanError, FetchPrepareLimits, FetchReady,
    FetchReport, FetchUpdateLimits,
};
use crate::{OpenLimits, OpenOptions, Repository};

/// Metadata bounds for controlled ordinary fetch completion.
///
/// Defaults use [`OpenLimits::default`] for each repository admission and
/// [`FetchPrepareLimits::default`] for each reference/HEAD admission: 100,000 reference entries,
/// 16 MiB of reference input, 4 KiB of HEAD data and 100,000 shallow roots. Admissions have
/// separate budgets before installation, before shallow publication and before reference
/// publication. Existing update limits govern primary pack snapshots and history verification;
/// alternate-store discovery uses [`crate::AlternateLimits::default`] for each object snapshot.
///
/// This type is `Copy`, so future fields must also be `Copy`. It does not own configuration inputs.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct FetchFinishLimits {
    /// Bounds each fresh repository open, including configuration and shallow metadata.
    pub open: OpenLimits,
    /// Bounds each reference inventory, HEAD chain and shallow-root admission.
    pub preparation: FetchPrepareLimits,
}

impl FetchFinishLimits {
    /// Replaces repository admission bounds.
    #[must_use]
    pub fn with_open(mut self, open: OpenLimits) -> Self {
        self.open = open;
        self
    }

    /// Replaces reference, HEAD and shallow-root admission bounds.
    #[must_use]
    pub fn with_preparation(mut self, preparation: FetchPrepareLimits) -> Self {
        self.preparation = preparation;
        self
    }
}

/// Explicit controlled completion of an already validated ordinary fetch.
///
/// Defaults combine [`FetchUpdateLimits::default`] and [`FetchFinishLimits::default`]. Preparation,
/// mapping and transfer have their own bounds; use [`super::FetchRequest::prepare_controlled`] and
/// bounded receive policies before this phase. Linked callers and any registered linked worktree
/// are refused with [`FetchPlanError::LinkedWorktrees`] before installation. The controlled opener
/// can also refuse layouts the trusted opener accepts; see [`OpenOptions`].
///
/// The caller must retain the GC and worktree/HEAD exclusion required by
/// [`super::FetchRequest`] throughout completion. Filesystem calls, bounded parsing and a started
/// reference transaction finish before cancellation can be observed. This builder starts no
/// transport or background work. Reference transactions retain their trusted metadata contract;
/// their files-backed reads and preparation are not bounded by these admission budgets. This
/// builder alone does not make prefetch execution bounded. Installation uses the prepared
/// request's repository/configuration handle. Controlled reopens admit metadata and supply
/// publication verification snapshots. Controlled reference publication is tracked in
/// [automatic maintenance](https://github.com/joshka/girt/issues/531). Retained completion
/// also keeps its existing trusted metadata contract.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::OpenLimits;
/// use girt::fetch::{
///     FetchFinishError, FetchFinishLimits, FetchFinishOptions, FetchReady, FetchReport,
/// };
///
/// fn complete(ready: FetchReady, cancel: &AtomicBool) -> Result<FetchReport, FetchFinishError> {
///     FetchFinishOptions::default()
///         .with_admission_limits(FetchFinishLimits::default().with_open(OpenLimits::default()))
///         .finish(ready, cancel)
/// }
/// # let _ = complete;
/// ```
#[derive(Clone, Debug, Default)]
pub struct FetchFinishOptions {
    limits: FetchUpdateLimits,
    admission: FetchFinishLimits,
}

impl FetchFinishOptions {
    /// Replaces installed-object and update-history bounds.
    #[must_use]
    pub fn with_limits(mut self, limits: FetchUpdateLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Replaces controlled repository and reference admission bounds.
    #[must_use]
    pub fn with_admission_limits(mut self, admission: FetchFinishLimits) -> Self {
        self.admission = admission;
        self
    }

    /// Admits metadata, installs validated objects and conditionally publishes references.
    ///
    /// # Errors
    ///
    /// Returns [`FetchFinishError`] with existing typed admission, installation and publication
    /// failures. Initial admission failures install nothing. After installation, objects and
    /// completed shallow/reference effects remain; inspect the report before retrying. No rollback
    /// or automatic cleanup occurs.
    pub fn finish(
        &self,
        ready: FetchReady,
        cancel: &AtomicBool,
    ) -> Result<FetchReport, FetchFinishError> {
        ready.finish_controlled(self.limits, self.admission, cancel)
    }
}

pub(super) fn admit(
    repository: &Repository,
    limits: FetchFinishLimits,
    cancel: &AtomicBool,
) -> Result<Repository, Box<FetchFinishFailure>> {
    super::check_cancelled(cancel).map_err(FetchFinishFailure::BeforePublication)?;
    super::worktree::admit_controlled(repository, cancel).map_err(FetchFinishFailure::Safety)?;
    let current = OpenOptions::default()
        .with_limits(limits.open)
        .open(repository.git_dir(), cancel)
        .map_err(FetchFinishFailure::Reopen)?;
    if current.object_dir() != repository.object_dir()
        || current.object_format() != repository.object_format()
    {
        return Err(
            FetchFinishFailure::BeforePublication(super::FetchError::Unsupported(
                "fetch repository layout changed",
            ))
            .into(),
        );
    }
    super::worktree::admit_controlled(&current, cancel).map_err(FetchFinishFailure::Safety)?;
    let refs = current
        .references()
        .map_err(FetchPlanError::Reference)
        .map_err(FetchFinishFailure::Safety)?;
    let refs = refs.with_reftable_limits(limits.preparation.reftable);
    let listed = refs
        .list_controlled(
            limits.preparation.max_ref_entries,
            limits.preparation.max_ref_bytes,
            cancel,
        )
        .map_err(FetchPlanError::Reference)
        .map_err(FetchFinishFailure::Safety)?;
    super::worktree::check_head_controlled(&current, &listed, limits.preparation, cancel)
        .map_err(FetchFinishFailure::Safety)?;
    if current.shallow_roots().iter().len() > limits.preparation.max_shallow_roots {
        return Err(
            FetchFinishFailure::Safety(FetchPlanError::PrepareLimit("shallow roots")).into(),
        );
    }
    Ok(current)
}
