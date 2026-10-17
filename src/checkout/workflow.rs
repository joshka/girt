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
    /// let cancel = AtomicBool::new(false);
    /// repo.checkout_tree(None, Some(tree), Default::default(), &cancel)?;
    /// let report = repo.checkout_tree(Some(tree), None, Default::default(), &cancel)?;
    /// assert!(report.index_published);
    /// assert!(!repo.worktree().unwrap().join("hello.txt").exists());
    /// assert!(loose.read_blob(blob, 1024).is_ok()); // Removing a file does not delete its object.
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn checkout_tree(
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
    /// Returns [`Self::checkout_tree`]'s failures and partial mutation reports.
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
            return super::worktree::run_observed(
                self,
                baseline,
                target,
                limits,
                cancel,
                &mut |_, _| Ok(()),
                &mut progress,
            );
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
}
