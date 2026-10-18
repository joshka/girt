use std::sync::atomic::{AtomicBool, Ordering};

use super::{InitPlan, Update, UpdateMethod, UpdatePlan};
use crate::workflow::Environment;
use crate::{Repository, Signature, checkout, filter};

impl InitPlan {
    /// Publishes initialization assignments with a 1 MiB local-config limit.
    ///
    /// Uses [`Self::apply_with`]. Reopen the parent afterwards; this handle stays a snapshot.
    ///
    /// # Errors
    ///
    /// Returns the locking, stale-input, format and publication failures of [`Self::apply_with`].
    ///
    /// ```
    /// use girt::submodule::Modules;
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let parent = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("parent"),
    ///     InitKind::Worktree,
    /// )?;
    /// let modules = Modules::parse(b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\n")?;
    /// let plan = modules
    ///     .get(b"math")
    ///     .unwrap()
    ///     .plan_init(parent.config(), b"https://example.com/team/parent.git")?;
    /// assert!(plan.apply(&parent)?);
    /// let reopened = Repository::open(parent.worktree().unwrap())?;
    /// assert_eq!(
    ///     reopened.config().string("submodule", Some(b"math"), "url"),
    ///     Some(b"https://example.com/team/math.git".as_slice())
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn apply(&self, parent: &Repository) -> Result<bool, InitError> {
        self.apply_with(parent, 1024 * 1024)
    }

    /// Locks the parent's local config, checks the plan's effective inputs, and publishes edits.
    ///
    /// Returns whether config was published. Fresh resolution retains the exact explicitly
    /// selected opening inputs, including caller-selected local sources. Only URL/update and
    /// activation values participate in the plan precondition; unrelated edits are preserved.
    /// The definition and chosen remote remain explicit caller inputs, not reread metadata.
    /// Other configuration sources and metadata must remain stable under caller exclusion.
    /// `ConfigEdit` checks the direct file again before rename; no multi-file transaction or
    /// crash durability is promised. Neither `.gitmodules` nor the worktree is changed.
    ///
    /// # Errors
    ///
    /// Returns [`InitError::Changed`] when relevant effective values changed since planning.
    /// Failed preparation aborts its owned config lock, retaining any cleanup error. Publication
    /// uses [`crate::config::ConfigEdit`]'s conditional replacement and recovery contract.
    pub fn apply_with(
        &self,
        parent: &Repository,
        max_config_bytes: usize,
    ) -> Result<bool, InitError> {
        if parent.worktree().is_none() {
            return Err(InitError::NoWorktree);
        }
        let mut edit = parent.edit_config(max_config_bytes)?;
        let prepared = (|| {
            let fresh = parent.reopen()?;
            if !same_layout(parent, &fresh)
                || self.expected != super::plan::init_values(fresh.config(), self.name())
            {
                return Err(InitError::Changed);
            }
            let mut changed = false;
            for (key, value) in self.settings() {
                edit.document_mut()
                    .append("submodule", Some(self.name()), key, value)?;
                changed = true;
            }
            Ok(changed)
        })();
        match prepared {
            Ok(true) => {
                edit.commit()?;
                Ok(true)
            }
            Ok(false) => {
                edit.abort()?;
                Ok(false)
            }
            Err(operation) => match edit.abort() {
                Ok(()) => Err(operation),
                Err(cleanup) => Err(InitError::Cleanup {
                    operation: Box::new(operation),
                    cleanup: Box::new(cleanup),
                }),
            },
        }
    }
}

/// Initialization failed without publishing proposed assignments.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum InitError {
    /// A bare parent has no submodule working tree.
    #[error("submodule initialization requires a working tree")]
    NoWorktree,
    /// Layout or relevant configuration no longer matches the opening/plan; observe and plan again.
    #[error("submodule initialization inputs changed")]
    Changed,
    /// Fresh repository/configuration resolution failed.
    #[error(transparent)]
    Open(#[from] crate::OpenError),
    /// Proposed assignment encoding failed before publication.
    #[error(transparent)]
    Format(#[from] crate::ConfigError),
    /// Config locking, publication or cleanup failed; inspect its retained cause.
    #[error(transparent)]
    Edit(#[from] crate::config::EditError),
    /// Preparation failed and abort could not remove the owned config lock.
    #[error("initialization failed and config lock cleanup failed: {cleanup}")]
    Cleanup {
        /// Original preparation failure.
        #[source]
        operation: Box<InitError>,
        /// Failure removing the owned lock.
        cleanup: Box<crate::config::EditError>,
    },
}

/// Explicit checkout conversion, resource bounds and reflog identity for a submodule update.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct UpdateOptions {
    /// Identity recorded when detached HEAD is published.
    pub committer: Signature,
    /// Captured attribute and filter-process environment; entering the child replaces repository
    /// context while preserving user runtime configuration. Programs receive only this snapshot.
    pub environment: Environment,
    /// Filter-driver permission, independently of custom update-method permission.
    pub filter_commands: filter::Commands,
    /// Checkout/index/object bounds, applied to this child (there is no recursion).
    pub limits: checkout::Limits,
}
impl UpdateOptions {
    /// Uses bounded defaults, an empty environment and refused filter programs.
    pub fn new(committer: Signature) -> Self {
        Self {
            committer,
            environment: Environment::default(),
            filter_commands: filter::Commands::Refuse,
            limits: checkout::Limits::default(),
        }
    }
    /// Selects explicit attribute/filter environment values.
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }
    /// Authorizes or refuses child filter programs.
    pub fn with_filter_commands(mut self, commands: filter::Commands) -> Self {
        self.filter_commands = commands;
        self
    }
    /// Chooses bounded checkout/index/object budgets.
    pub fn with_limits(mut self, limits: checkout::Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// Completed child checkout and detached-HEAD publication, or a no-op.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct UpdateReport {
    /// File/index effects, absent when the method is none or HEAD already matches.
    pub checkout: Option<checkout::Report>,
    /// Stored HEAD was published as the recorded direct commit; no branch ref was moved.
    pub head_updated: bool,
}

impl UpdatePlan {
    /// Applies checkout/none to an existing child using [`Self::apply_with`] without cancellation.
    ///
    /// # Errors
    ///
    /// Returns the preparation, checkout and partial-publication failures of [`Self::apply_with`].
    pub fn apply(
        &self,
        parent: &Repository,
        options: &UpdateOptions,
    ) -> Result<UpdateReport, UpdateError> {
        self.apply_with(parent, options, &AtomicBool::new(false))
    }

    /// Checks fresh inputs, checks out the recorded commit, then publishes detached HEAD.
    ///
    /// This performs no fetch, clone, hook, recursion or custom update command. None is a no-op;
    /// rebase/merge/custom methods are explicitly refused. An already matching HEAD is left as-is,
    /// including branch attachment and dirty files, as Git update does without force.
    ///
    /// The caller must exclude parent index/config writers and all child metadata/worktree writers.
    /// Exact no-follow path checks protect traversal on macOS/Linux but do not create a sandbox
    /// against directory replacement or malicious child metadata. Child configuration inherits only
    /// caller-selected non-local inputs, then adds its own local config; parent-local config is
    /// excluded. Baseline and target use the child's own attributes and configured filters.
    ///
    /// HEAD/reference locks are acquired before checkout and retained through publication. File and
    /// index publication precedes HEAD; there is no rollback or multi-file atomic transaction.
    /// Reopen repositories after mutation. Bounds are those of `options.limits`; operation-state
    /// inspection uses its bounded defaults. Cancellation is checked before reads and publication,
    /// plus by checkout; a syscall, ref transaction or filter program can block. Dropped prepared
    /// reference transactions use their best-effort lock cleanup before publication.
    ///
    /// # Errors
    ///
    /// Refuses stale parent gitlinks/config, missing child/commit, active operations, unsupported
    /// platform/method and dirty/staged/conflicted content. Preparation failures leave worktree
    /// content unchanged. [`UpdateError::Checkout`] retains partial effects with HEAD unchanged;
    /// [`UpdateError::AfterCheckout`] retains completed checkout and any reference-transaction
    /// effects when later cancellation, stale index detection or HEAD publication fails.
    pub fn apply_with(
        &self,
        parent: &Repository,
        options: &UpdateOptions,
        cancel: &AtomicBool,
    ) -> Result<UpdateReport, UpdateError> {
        check(cancel)?;
        match self.method() {
            UpdateMethod::BuiltIn(Update::None) => return Ok(UpdateReport::default()),
            UpdateMethod::BuiltIn(Update::Checkout) => {}
            _ => return Err(UpdateError::UnsupportedMethod),
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            self.checkout_with(parent, options, cancel, || {})
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (parent, options);
            Err(UpdateError::UnsupportedPlatform)
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn checkout_with(
        &self,
        parent: &Repository,
        options: &UpdateOptions,
        cancel: &AtomicBool,
        after_checkout: impl FnOnce(),
    ) -> Result<UpdateReport, UpdateError> {
        self.checkout_initial(parent, options, cancel, false, after_checkout)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn checkout_initial(
        &self,
        parent: &Repository,
        options: &UpdateOptions,
        cancel: &AtomicBool,
        initial: bool,
        after_checkout: impl FnOnce(),
    ) -> Result<UpdateReport, UpdateError> {
        use crate::refs::{Dereference, Expected, RefEdit, RefName, Reflog, Target};
        let fresh = parent.reopen().map_err(prepare)?;
        if !same_layout(parent, &fresh) {
            return Err(UpdateError::Changed);
        }
        let parent = fresh;
        if self.expected != super::plan::update_values(parent.config(), self.name()) {
            return Err(UpdateError::Changed);
        }
        let index = parent.read_index(options.limits.index).map_err(prepare)?;
        let entries = index.as_ref().map_or(&[][..], |index| index.entries());
        let mut matching = entries.iter().filter(|entry| entry.path == self.path());
        let current = matching.next().ok_or(UpdateError::Changed)?;
        if current.mode != crate::index::Mode::Gitlink
            || current.stage != crate::index::Stage::Normal
            || current.id != self.recorded()
            || matching.next().is_some()
        {
            return Err(UpdateError::Changed);
        }
        let path = super::status::checkout_path(&parent, self.path())
            .map_err(prepare)?
            .ok_or(UpdateError::MissingCheckout)?;
        let child =
            Repository::open_with_config(&path, parent.inherited_config()).map_err(prepare)?;
        self.recorded()
            .require_format(child.object_format())
            .map_err(prepare)?;
        if child.is_bare() || child.worktree() != Some(path.as_path()) {
            return Err(UpdateError::MissingCheckout);
        }
        let refs = child.references().map_err(prepare)?;
        let stored = refs.read(&RefName::HEAD).map_err(prepare)?;
        let head = refs.resolve(&RefName::HEAD).map_err(prepare)?;
        if !initial && head.id == Some(self.recorded()) {
            return Ok(UpdateReport::default());
        }
        if !child
            .operation_state(Default::default())
            .map_err(prepare)?
            .is_empty()
        {
            return Err(UpdateError::OperationInProgress);
        }
        let expected = stored.clone().map_or(Expected::Absent, Expected::Value);
        let from = match &stored {
            Some(Target::Symbolic(name)) => name
                .as_bytes()
                .strip_prefix(b"refs/heads/")
                .unwrap_or(name.as_bytes())
                .to_vec(),
            Some(Target::Direct(id)) => id.to_string().into_bytes(),
            None => Vec::new(),
        };
        let message = [
            b"checkout: moving from ".as_slice(),
            &from,
            b" to ",
            self.recorded().to_string().as_bytes(),
        ]
        .concat();
        let edit = RefEdit::new(
            RefName::HEAD,
            Some(Target::Direct(self.recorded())),
            expected,
            Reflog::Append {
                committer: options.committer.clone(),
                message,
            },
        )
        .with_dereference(Dereference::Stored);
        let locked = refs.prepare_transaction(&[edit]).map_err(prepare)?;
        check(cancel)?;
        let head = refs.resolve(&RefName::HEAD).map_err(prepare)?;
        let objects = child.objects_with(options.limits.packs).map_err(prepare)?;
        let read = options.limits.read.with_max_object_bytes(
            options
                .limits
                .read
                .max_object_bytes
                .min(options.limits.max_object_bytes),
        );
        let baseline = head
            .id
            .filter(|_| !initial)
            .map(|id| objects.read_commit_with(id, read))
            .transpose()
            .map_err(prepare)?
            .map(|commit| commit.tree());
        let target = objects
            .read_commit_with(self.recorded(), read)
            .map_err(prepare)?
            .tree();
        let environment = super::child_environment(&child, &options.environment);
        let mut baseline_filters = child
            .filters(&environment, options.filter_commands)
            .map_err(prepare)?;
        let mut target_filters = child
            .filters_for_checkout(&environment, target, options.filter_commands)
            .map_err(prepare)?;
        baseline_filters.set_command_environment(&environment);
        target_filters.set_command_environment(&environment);
        check_parent(&parent, index.as_ref(), options.limits.index)?;
        let checkout = child
            .checkout_tree_with_filters(
                baseline,
                Some(target),
                &mut baseline_filters,
                &mut target_filters,
                options.limits,
                cancel,
            )
            .map_err(|error| UpdateError::Checkout(Box::new(error)))?;
        after_checkout();
        let finish: Result<(), UpdateError> = (|| {
            check(cancel)?;
            check_parent(&parent, index.as_ref(), options.limits.index)?;
            locked.publish().map_err(prepare)?;
            Ok(())
        })();
        match finish {
            Ok(()) => Ok(UpdateReport {
                checkout: Some(checkout),
                head_updated: true,
            }),
            Err(source) => Err(UpdateError::AfterCheckout {
                checkout: Box::new(checkout),
                source: Box::new(source),
            }),
        }
    }
}

pub(super) fn same_layout(original: &Repository, fresh: &Repository) -> bool {
    original.git_dir() == fresh.git_dir()
        && original.common_dir() == fresh.common_dir()
        && original.worktree() == fresh.worktree()
        && original.is_bare() == fresh.is_bare()
        && original.object_format() == fresh.object_format()
        && original.object_dir() == fresh.object_dir()
        && original.reference_backend() == fresh.reference_backend()
}

fn check(cancel: &AtomicBool) -> Result<(), UpdateError> {
    if cancel.load(Ordering::Relaxed) {
        Err(UpdateError::Cancelled)
    } else {
        Ok(())
    }
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prepare(error: impl Into<crate::Error>) -> UpdateError {
    UpdateError::Prepare(Box::new(error.into()))
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_parent(
    parent: &Repository,
    index: Option<&crate::index::Index>,
    limits: crate::index::Limits,
) -> Result<(), UpdateError> {
    if parent.index_unchanged(index, limits).map_err(prepare)? {
        Ok(())
    } else {
        Err(UpdateError::Changed)
    }
}

/// Update refusal or failure, preserving any completed worktree/index publication.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UpdateError {
    /// No child mutation was attempted; choose checkout or none in this increment.
    #[error("submodule update execution supports checkout and none only")]
    UnsupportedMethod,
    /// Checkout is supported only on macOS/Linux; nothing was changed.
    #[error("submodule checkout requires Linux/macOS")]
    UnsupportedPlatform,
    /// Parent layout, gitlink or relevant effective config no longer matches the opening/plan.
    #[error("submodule update inputs changed")]
    Changed,
    /// The exact child checkout is not populated; initialize/clone explicitly.
    #[error("submodule checkout is not populated")]
    MissingCheckout,
    /// Child operation metadata exists; finish or abort it explicitly before retrying.
    #[error("submodule operation is in progress")]
    OperationInProgress,
    /// Cancellation before mutation; late cancellation is wrapped in AfterCheckout.
    #[error("submodule update cancelled")]
    Cancelled,
    /// Preparation failed before worktree mutation; the original typed cause is retained.
    #[error(transparent)]
    Prepare(Box<crate::Error>),
    /// Checkout failed with its partial effects; HEAD was not published by this operation.
    #[error("submodule checkout failed")]
    Checkout(#[source] Box<checkout::Failure>),
    /// Checkout completed; inspect it and any reference effects before recovery. No rollback
    /// occurs.
    #[error("submodule checkout completed but HEAD was not fully published")]
    AfterCheckout {
        /// Completed child file/index publication.
        checkout: Box<checkout::Report>,
        /// Cancellation, stale-parent detection or reference publication cause.
        #[source]
        source: Box<UpdateError>,
    },
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
use crate::layout_git as oracle;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
