use crate::Repository;
use crate::index::{IndexEdit, Limits, StorageError};
use crate::refs::{
    Expected, RefEdit, RefEditOutcome, RefName, ReferenceError, Reflog, Target, TransactionError,
};

/// A locked worktree index draft that can be published before a conditional stored HEAD edit.
///
/// Caller policy chooses entries, stat reuse, HEAD target and history. No working files change.
/// Index and HEAD publication cannot be atomic together; readers can observe the new index with
/// old HEAD. A HEAD failure retains the new index and reports its reference/reflog effects.
#[derive(Debug)]
pub struct ColocationEdit<'a> {
    repository: &'a Repository,
    index: IndexEdit,
}

/// Publication failure identifying whether the index has already changed.
#[derive(Debug, thiserror::Error)]
pub enum ColocationError {
    /// Index publication failed; HEAD was not attempted. The storage error retains cleanup causes.
    #[error("colocation index publication failed: {0}")]
    Index(#[source] StorageError),
    /// HEAD validation or locking failed before publication. Neither index nor HEAD changed.
    #[error("colocation HEAD preparation failed: {source}")]
    Prepare {
        /// Conditional reference preparation failure.
        #[source]
        source: TransactionError,
        /// Index lock cleanup failure, if any; recover this lock explicitly.
        cleanup: Option<Box<StorageError>>,
    },
    /// The index is published; HEAD publication failed. Do not blindly retry.
    #[error("index published but HEAD transaction failed: {0}")]
    Head(#[source] TransactionError),
    /// The reference store could not be opened; index publication was not attempted.
    #[error("cannot open colocation references: {source}")]
    References {
        /// Reference backend failure.
        #[source]
        source: ReferenceError,
        /// Index lock cleanup failure, if any.
        cleanup: Option<Box<StorageError>>,
    },
}

impl Repository {
    /// Locks and reads the index for a caller-defined colocation update.
    ///
    /// Use [`ColocationEdit::index_mut`] to derive changes from the locked current index.
    /// Keeping this guard alive excludes cooperating index writers. See [`Self::edit_index`]
    /// for resource, filesystem and cleanup contracts. Dropping abandons the index draft.
    ///
    /// # Errors
    ///
    /// Returns the same lock, format and read failures as [`Self::edit_index`].
    pub fn edit_colocation(&self, limits: Limits) -> Result<ColocationEdit<'_>, StorageError> {
        Ok(ColocationEdit {
            repository: self,
            index: self.edit_index(limits)?,
        })
    }
}

impl ColocationEdit<'_> {
    /// Borrows the held index guard for caller-defined entry/flag/stat edits.
    pub fn index_mut(&mut self) -> &mut IndexEdit {
        &mut self.index
    }

    /// Releases the index lock without publishing either index or HEAD.
    ///
    /// # Errors
    ///
    /// Reports lock cleanup failure; original index and HEAD contents remain unchanged.
    pub fn abort(self) -> Result<(), StorageError> {
        self.index.abort()
    }

    /// Prepares a conditional HEAD edit, publishes the index, then publishes stored HEAD.
    ///
    /// Direct targets detach HEAD without changing the former branch. Symbolic targets can name
    /// an unborn branch; this method never deletes that branch to make it unborn. Caller-supplied
    /// preconditions compare stored HEAD under reference locks held through both publications.
    /// Reflog policy is explicit and inherits [`crate::refs::References::transaction`]. Object
    /// existence/type is not checked. Operation-state cleanup remains a separate caller
    /// decision through [`super::OperationState`].
    ///
    /// # Errors
    ///
    /// [`ColocationError::Prepare`] leaves index and HEAD unchanged and releases owned locks.
    /// [`ColocationError::Index`] preserves old index bytes and leaves HEAD untouched. A
    /// [`ColocationError::Head`] means the index is already published; inspect the nested
    /// transaction's per-reference and per-log effects before recovery. No rollback or atomic
    /// multi-file visibility is promised. The caller must exclude working-file writers if its
    /// chosen draft relies on working-file observations.
    pub fn commit(
        self,
        target: Target,
        expected: Expected,
        reflog: Reflog,
    ) -> Result<Vec<RefEditOutcome>, ColocationError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "colocation.commit", outcome = "incomplete", failure_class = tracing::field::Empty, effects = tracing::field::Empty);
        let operation = || self.commit_with(target, expected, reflog, || {});
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| match error {
            ColocationError::Index(error) => crate::trace::index(error, &span),
            ColocationError::Prepare { source, .. } => crate::trace::transaction(source, &span),
            ColocationError::Head(source) => {
                let class = crate::trace::transaction(source, &span);
                span.record("effects", "index_published_head_may_have_effects");
                class
            }
            ColocationError::References { .. } => "unsupported",
        });
        result
    }

    fn commit_with(
        self,
        target: Target,
        expected: Expected,
        reflog: Reflog,
        after_index: impl FnOnce(),
    ) -> Result<Vec<RefEditOutcome>, ColocationError> {
        let refs = match self.repository.references() {
            Ok(refs) => refs,
            Err(source) => {
                return Err(ColocationError::References {
                    source,
                    cleanup: self.index.abort().err().map(Box::new),
                });
            }
        };
        let edit = RefEdit {
            name: RefName::new(b"HEAD").expect("HEAD is valid"),
            dereference: false,
            target: Some(target),
            expected,
            reflog,
        };
        let prepared = match refs.prepare_transaction(&[edit]) {
            Ok(prepared) => prepared,
            Err(source) => {
                return Err(ColocationError::Prepare {
                    source,
                    cleanup: self.index.abort().err().map(Box::new),
                });
            }
        };
        self.index.commit().map_err(ColocationError::Index)?;
        after_index();
        prepared.publish().map_err(ColocationError::Head)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use rstest::rstest;

    use super::*;
    use crate::refs::RefOutcome;
    use crate::{InitKind, ObjectFormat, ObjectId};

    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn head_publication_failure_reports_published_index(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
        let edit = repo.edit_colocation(Limits::default()).unwrap();
        let error = edit
            .commit_with(
                Target::Direct(ObjectId::for_blob(format, b"id")),
                Expected::Exists,
                Reflog::Preserve,
                || {
                    // Force rename failure after both preparations and successful index
                    // publication.
                    fs::remove_file(repo.git_dir().join("HEAD")).unwrap();
                    fs::create_dir(repo.git_dir().join("HEAD")).unwrap();
                },
            )
            .unwrap_err();
        let ColocationError::Head(TransactionError::Publish { outcomes, .. }) = error else {
            panic!("expected partial publication")
        };
        assert_eq!(outcomes[0].reference, RefOutcome::Unchanged);
        assert!(repo.read_index(Limits::default()).unwrap().is_some());
        assert!(!repo.git_dir().join("index.lock").exists());
        assert!(!repo.git_dir().join("HEAD.lock").exists());
    }
    #[rstest]
    #[case::sha1(ObjectFormat::Sha1)]
    #[case::sha256(ObjectFormat::Sha256)]
    fn log_failure_retains_published_head_and_index(#[case] format: ObjectFormat) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(format, root.path().join("repo"), InitKind::Worktree).unwrap();
        let target = ObjectId::for_blob(format, b"id");
        let reflog = Reflog::Append {
            committer: crate::Signature {
                name: b"Fixture".to_vec(),
                email: b"fixture@example.invalid".to_vec(),
                seconds: 1,
                offset_minutes: 0,
            },
            message: b"colocation".to_vec(),
        };
        let edit = repo.edit_colocation(Limits::default()).unwrap();
        let error = edit
            .commit_with(Target::Direct(target), Expected::Exists, reflog, || {
                fs::create_dir(repo.git_dir().join("logs/HEAD")).unwrap();
            })
            .unwrap_err();
        let ColocationError::Head(TransactionError::Publish { outcomes, .. }) = error else {
            panic!("expected partial publication")
        };
        assert_eq!(outcomes[0].reference, RefOutcome::Published);
        assert!(matches!(
            outcomes[0].logs[0].1,
            crate::refs::LogOutcome::Failed { bytes_written: 0 }
        ));
        assert_eq!(
            repo.references()
                .unwrap()
                .read(&RefName::new(b"HEAD").unwrap())
                .unwrap(),
            Some(Target::Direct(target))
        );
        assert!(repo.read_index(Limits::default()).unwrap().is_some());
    }
}
