use std::sync::atomic::AtomicBool;

use super::{Failure, Limits, Report};
use crate::{ObjectId, Repository};

impl Repository {
    /// Materializes raw bytes and POSIX modes, then publishes the index from a verified clean
    /// baseline.
    ///
    /// `None` means an empty tree for either input. For a no-checkout clone, use an empty baseline
    /// and the desired commit's tree as target. For updates, supply the tree represented by the
    /// current index. HEAD and references remain unchanged, so Git may report staged changes
    /// relative to HEAD after success. See [`crate::checkout`] for raw semantics, caller exclusion,
    /// platform limits and failure recovery. No previous status report authorizes this operation.
    ///
    /// # Errors
    ///
    /// Returns [`Failure`] on dirty/staged/conflicted state, obstruction, unsupported input,
    /// cancellation, exceeded limits or I/O. Its report records prior mutations and publication;
    /// cleanup failures are separate. Preparation errors leave worktree content unchanged.
    ///
    /// # Replace a clean checkout, then remove its tracked content
    ///
    /// This example owns the disposable repository exclusively. The second call supplies the tree
    /// represented by the index as its baseline; supplying `None` again would lose that contract.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ObjectFormat, Repository};
    /// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Worktree,
    /// )?;
    ///
    /// use girt::{EntryMode, Tree, TreeEntry};
    ///
    /// let loose = repo.loose_objects();
    /// let blob = loose.write_blob(b"hello\n")?;
    /// let tree = loose.write_tree(&Tree::new(
    ///     ObjectFormat::Sha1,
    ///     vec![TreeEntry {
    ///         name: b"hello.txt".to_vec(),
    ///         mode: EntryMode::Blob,
    ///         id: blob,
    ///     }],
    /// )?)?;
    ///
    /// let cancel = AtomicBool::new(false);
    /// repo.checkout_tree_with(None, Some(tree), Default::default(), &cancel)?;
    /// let report = repo.checkout_tree_with(Some(tree), None, Default::default(), &cancel)?;
    ///
    /// assert!(report.index_published);
    /// assert!(!repo.worktree().unwrap().join("hello.txt").exists());
    /// assert!(loose.read_blob(blob, 1024).is_ok()); // Removing a file does not delete its object.
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn checkout_tree_with(
        &self,
        baseline: Option<ObjectId>,
        target: Option<ObjectId>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Failure> {
        self.checkout_tree_with_progress(baseline, target, limits, cancel, |_, _| {})
    }

    /// Materializes a tree with synchronous completed-operation counts `(done, total)`.
    ///
    /// Reports zero after preparation, then each applied file or directory operation. The total
    /// includes directories, not just files. The final notification follows index publication;
    /// reaching the operation total alone does not establish publication. Observers must return
    /// promptly and must not access the repository. A panic may leave the partial mutations
    /// described by [`crate::checkout`]'s recovery contract.
    ///
    /// # Errors
    ///
    /// Returns [`Self::checkout_tree_with`]'s failures and partial mutation reports.
    pub fn checkout_tree_with_progress(
        &self,
        baseline: Option<ObjectId>,
        target: Option<ObjectId>,
        limits: Limits,
        cancel: &AtomicBool,
        progress: impl FnMut(u64, u64),
    ) -> Result<Report, Failure> {
        let _lock_operation = crate::locks::operation();
        for id in baseline.into_iter().chain(target) {
            id.require_format(self.object_format())
                .map_err(|error| Failure {
                    cause: Box::new(error.into()),
                    report: Report::default(),
                    cleanup: vec![],
                })?;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let mut progress = progress;
            super::worktree::run_observed(
                self,
                baseline,
                target,
                None,
                limits,
                cancel,
                &mut |_, _| Ok(()),
                &mut progress,
            )
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (baseline, target, limits, progress);
            let cause = if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                super::Error::Cancelled
            } else {
                super::types::refused(b"", "checkout requires Linux/macOS")
            };
            Err(Failure {
                cause: Box::new(cause),
                report: Report::default(),
                cleanup: vec![],
            })
        }
    }

    /// Checks out a tree as [`Self::checkout_tree_with`] does, converting files through `filters`.
    ///
    /// Each regular file of the target is written as `target_filters` converts its blob with
    /// [`Pipeline::to_worktree`]: line endings, `ident` and filter drivers such as Git LFS. Each
    /// baseline file must equal what `baseline_filters` converts its blob to, to count as clean.
    /// Load `baseline_filters` with [`Self::filters`], which reads the `.gitattributes` files
    /// currently in the working tree, and `target_filters` with [`Self::filters_for_checkout`],
    /// which reads the target's own attributes as Git reads those of the index it checks out.
    /// Publication verifies the converted bytes. Symlinks are never converted. All conversion
    /// happens during preparation, before any working-tree change, so a failing required filter
    /// changes nothing; converted contents of the target are held in memory until publication.
    /// Paths written unconverted because a non-required driver failed are listed in
    /// [`Report::unfiltered`](super::Report::unfiltered).
    ///
    /// Index entries keep zero cached stat words, so a later status reads and converts each file.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`Self::checkout_tree_with`], plus
    /// [`Error::Filter`](super::Error::Filter) when converting a blob fails, including a command
    /// refused under [`Commands::Refuse`](crate::filter::Commands::Refuse).
    ///
    /// [`Pipeline::to_worktree`]: crate::filter::Pipeline::to_worktree
    pub fn checkout_tree_with_filters(
        &self,
        baseline: Option<ObjectId>,
        target: Option<ObjectId>,
        baseline_filters: &mut crate::filter::Pipeline,
        target_filters: &mut crate::filter::Pipeline,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Failure> {
        self.checkout_filtered_with_progress(
            baseline,
            target,
            baseline_filters,
            target_filters,
            limits,
            cancel,
            &mut |_, _| {},
        )
    }

    /// [`Self::checkout_tree_with_filters`], reporting progress as
    /// [`Self::checkout_tree_with_progress`] does.
    #[allow(
        clippy::too_many_arguments,
        reason = "the public call's inputs plus progress"
    )]
    pub(crate) fn checkout_filtered_with_progress(
        &self,
        baseline: Option<ObjectId>,
        target: Option<ObjectId>,
        baseline_filters: &mut crate::filter::Pipeline,
        target_filters: &mut crate::filter::Pipeline,
        limits: Limits,
        cancel: &AtomicBool,
        progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Report, Failure> {
        let _lock_operation = crate::locks::operation();
        for id in baseline.into_iter().chain(target) {
            id.require_format(self.object_format())
                .map_err(|error| Failure {
                    cause: Box::new(error.into()),
                    report: Report::default(),
                    cleanup: vec![],
                })?;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            super::worktree::run_observed(
                self,
                baseline,
                target,
                Some(super::worktree::Filters {
                    baseline: baseline_filters,
                    target: target_filters,
                }),
                limits,
                cancel,
                &mut |_, _| Ok(()),
                &mut |done, total| progress(done, total),
            )
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (
                baseline,
                target,
                baseline_filters,
                target_filters,
                limits,
                progress,
            );
            let cause = if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                super::Error::Cancelled
            } else {
                super::types::refused(b"", "checkout requires Linux/macOS")
            };
            Err(Failure {
                cause: Box::new(cause),
                report: Report::default(),
                cleanup: vec![],
            })
        }
    }

    /// Materializes a tree with [`Limits::default`] and no cancellation.
    ///
    /// Equivalent to `self.checkout_tree_with(baseline, target, Limits::default(), &never)` with a
    /// flag that is never set; see [`Self::checkout_tree_with`] for the baseline contract, recovery
    /// and platform limits. Use [`Self::checkout_tree_with_progress`] to observe progress or
    /// cancel.
    ///
    /// # Errors
    ///
    /// Returns the failures of [`Self::checkout_tree_with`] except cancellation.
    pub fn checkout_tree(
        &self,
        baseline: Option<ObjectId>,
        target: Option<ObjectId>,
    ) -> Result<Report, Failure> {
        self.checkout_tree_with(baseline, target, Limits::default(), &AtomicBool::new(false))
    }
}
