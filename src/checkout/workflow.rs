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
    pub fn checkout_tree(
        &self,
        baseline: Option<ObjectId>,
        target: Option<ObjectId>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Failure> {
        for id in baseline.into_iter().chain(target) {
            id.require_format(self.object_format())
                .map_err(|error| Failure {
                    cause: Box::new(error.into()),
                    report: Report::default(),
                    cleanup: vec![],
                })?;
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return super::worktree::run(self, baseline, target, limits, cancel, &mut |_, _| Ok(()));
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (baseline, target, limits);
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
