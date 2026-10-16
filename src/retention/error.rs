//! Typed reasons why a retention observation cannot authorize maintenance.

/// A failed inventory, graph observation, or bound check.
///
/// Limit failures require a larger caller bound; malformed or missing inputs require repair.
/// Changed inputs require a fresh observation under isolation. Read failures retain their source
/// type and chain, so jj can render diagnostics and inspect the owning storage error.
/// Sources are thread-safe but do not promise [`std::panic::UnwindSafe`] or
/// [`std::panic::RefUnwindSafe`]; a plan owning these diagnostics has the same restriction.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RetentionError {
    /// A stored payload or name could not be parsed; repair it before maintenance.
    #[error("malformed retention input")]
    Malformed(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A pseudoref could not be opened at this path.
    #[error("reading pseudoref at {path}")]
    PseudorefIo {
        /// Pseudoref being accessed.
        path: std::path::PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// An underlying repository, reference, object, or filesystem read failed.
    #[error("reading retention input")]
    Read(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Gc.recentObjectsHook is unsupported by retention planning.
    #[error("gc.recentObjectsHook is unsupported by retention planning")]
    RecentObjectsHook,
    /// Precious-object repository forbids deletion planning.
    #[error("precious-object repository forbids deletion planning")]
    PreciousObjects,
    /// Partial clone has an incomplete local object inventory.
    #[error("partial clone has an incomplete local object inventory")]
    PartialClone,
    /// Aggregate reflog byte limit.
    #[error("aggregate reflog byte limit")]
    AggregateReflogByteLimit,
    /// Aggregate reflog record limit.
    #[error("aggregate reflog record limit")]
    AggregateReflogRecordLimit,
    /// Index entry limit.
    #[error("index entry limit")]
    IndexEntryLimit,
    /// Aggregate root limit.
    #[error("aggregate root limit")]
    AggregateRootLimit,
    /// Symbolic reference cycle.
    #[error("symbolic reference cycle")]
    SymbolicReferenceCycle,
    /// Symbolic reference depth limit.
    #[error("symbolic reference depth limit")]
    SymbolicReferenceDepthLimit,
    /// Object directory entry limit.
    #[error("object directory entry limit")]
    ObjectDirectoryEntryLimit,
    /// Non-directory object entry.
    #[error("non-directory object entry")]
    NonDirectoryObjectEntry,
    /// Pack entry limit.
    #[error("pack entry limit")]
    PackEntryLimit,
    /// Non-UTF-8 object fanout.
    #[error("non-UTF-8 object fanout")]
    NonUtf8ObjectFanout,
    /// Unknown object directory entry.
    #[error("unknown object directory entry")]
    UnknownObjectDirectoryEntry,
    /// Loose object entry limit.
    #[error("loose object entry limit")]
    LooseObjectEntryLimit,
    /// Non-file loose object.
    #[error("non-file loose object")]
    NonFileLooseObject,
    /// Non-UTF-8 loose object.
    #[error("non-UTF-8 loose object")]
    NonUtf8LooseObject,
    /// Reachable object limit.
    #[error("reachable object limit")]
    ReachableObjectLimit,
    /// Reachable byte limit.
    #[error("reachable byte limit")]
    ReachableByteLimit,
    /// Reachable edge limit.
    #[error("reachable edge limit")]
    ReachableEdgeLimit,
    /// Pack directory entry limit.
    #[error("pack directory entry limit")]
    PackDirectoryEntryLimit,
    /// Missing pinned plan store.
    #[error("missing pinned plan store")]
    MissingPinnedPlanStore,
    /// Non-directory object fanout.
    #[error("non-directory object fanout")]
    NonDirectoryObjectFanout,
    /// Reflog byte limit.
    #[error("reflog byte limit")]
    ReflogByteLimit,
    /// Repository layout changed.
    #[error("repository layout changed")]
    RepositoryLayoutChanged,
    /// Retention inputs changed.
    #[error("retention inputs changed")]
    RetentionInputsChanged,
    /// Retention output changed.
    #[error("retention output changed")]
    RetentionOutputChanged,
    /// Non-file retention input target.
    #[error("non-file retention input target")]
    NonFileRetentionInputTarget,
    /// Input entry limit.
    #[error("input entry limit")]
    InputEntryLimit,
    /// Non-file retention input.
    #[error("non-file retention input")]
    NonFileRetentionInput,
    /// Input byte limit.
    #[error("input byte limit")]
    InputByteLimit,
    /// Invalid caller head.
    #[error("invalid caller head")]
    InvalidCallerHead,
    /// Caller head limit.
    #[error("caller head limit")]
    CallerHeadLimit,
    /// Protected pack has no readable index.
    #[error("protected pack has no readable index")]
    MissingProtectedIndex,
    /// Missing protected object.
    #[error("missing protected object")]
    MissingProtectedObject,
    /// Protected packed object limit.
    #[error("protected packed object limit")]
    ProtectedPackedObjectLimit,
    /// Retention scan cancelled.
    #[error("retention scan cancelled")]
    Cancelled,
    /// Input entry overflow.
    #[error("input entry overflow")]
    InputEntryOverflow,
    /// Input byte overflow.
    #[error("input byte overflow")]
    InputByteOverflow,
    /// Reachable byte overflow.
    #[error("reachable byte overflow")]
    ReachableByteOverflow,
    /// Reflog disappeared during discovery.
    #[error("reflog disappeared during discovery: {0:?}")]
    ReflogDisappeared(crate::refs::RefName),
    /// Reflog budget was exhausted; the payload names the budget for diagnostics only.
    #[error("reflog inventory is incomplete: {0:?}")]
    ReflogLimit(&'static str),
    /// Reflog records are corrupt despite reaching EOF.
    #[error("reflog records are incomplete")]
    ReflogIncomplete,
    /// Pseudoref exceeds its byte bound.
    #[error("pseudoref exceeds its byte bound: {0:?}")]
    PseudorefByteLimit(std::path::PathBuf),
    /// Pseudoref cannot be interpreted.
    #[error("pseudoref cannot be interpreted: {0:?}")]
    MalformedPseudoref(std::path::PathBuf),
    /// Pack entry is not a file.
    #[error("pack entry is not a file: {0:?}")]
    NonFilePackEntry(std::path::PathBuf),
    /// Reachable object is missing.
    #[error("reachable object is missing: {0}")]
    MissingObject(crate::ObjectId),
    /// Reachable object has the wrong kind.
    #[error("reachable object has the wrong kind: {0}")]
    WrongObjectKind(crate::ObjectId),
    /// Object contains a null edge.
    #[error("object contains a null edge: {0}")]
    NullEdge(crate::ObjectId),
    /// Graph edges disagree about object kind.
    #[error("graph edges disagree about object kind: {0}")]
    ConflictingObjectKind(crate::ObjectId),
}

/// Recovery category for an incomplete retention observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum RetentionErrorKind {
    /// Raise the relevant `RetentionPolicy` bound before observing again.
    Limit,
    /// This repository needs support beyond the implemented maintenance subset.
    Unsupported,
    /// Repair or recover missing/malformed inventory before maintenance.
    Malformed,
    /// Observe again under isolation; independent state changed.
    Changed,
    /// The caller requested cancellation.
    Cancelled,
    /// Inspect the source storage error before choosing recovery; do not blindly retry.
    Read,
}

impl RetentionError {
    /// Groups detailed conditions by caller recovery without parsing diagnostics.
    ///
    /// Read failures retain a storage-specific cause; that cause may report corruption,
    /// unsupported features, limits, or I/O and must be inspected before retrying.
    pub fn kind(&self) -> RetentionErrorKind {
        match self {
            Self::AggregateReflogByteLimit
            | Self::AggregateReflogRecordLimit
            | Self::IndexEntryLimit
            | Self::AggregateRootLimit
            | Self::SymbolicReferenceDepthLimit
            | Self::ObjectDirectoryEntryLimit
            | Self::PackEntryLimit
            | Self::LooseObjectEntryLimit
            | Self::ReachableObjectLimit
            | Self::ReachableByteLimit
            | Self::ReachableEdgeLimit
            | Self::PackDirectoryEntryLimit
            | Self::ReflogByteLimit
            | Self::InputEntryLimit
            | Self::InputByteLimit
            | Self::CallerHeadLimit
            | Self::ProtectedPackedObjectLimit
            | Self::InputEntryOverflow
            | Self::InputByteOverflow
            | Self::ReachableByteOverflow
            | Self::ReflogLimit(..)
            | Self::PseudorefByteLimit(..) => RetentionErrorKind::Limit,
            Self::RecentObjectsHook | Self::PreciousObjects | Self::PartialClone => {
                RetentionErrorKind::Unsupported
            }
            Self::RepositoryLayoutChanged
            | Self::RetentionInputsChanged
            | Self::RetentionOutputChanged
            | Self::ReflogDisappeared(..) => RetentionErrorKind::Changed,
            Self::SymbolicReferenceCycle
            | Self::NonDirectoryObjectEntry
            | Self::NonUtf8ObjectFanout
            | Self::UnknownObjectDirectoryEntry
            | Self::NonFileLooseObject
            | Self::NonUtf8LooseObject
            | Self::MissingPinnedPlanStore
            | Self::NonDirectoryObjectFanout
            | Self::NonFileRetentionInputTarget
            | Self::NonFileRetentionInput
            | Self::InvalidCallerHead
            | Self::MissingProtectedIndex
            | Self::MissingProtectedObject
            | Self::ReflogIncomplete
            | Self::MalformedPseudoref(..)
            | Self::NonFilePackEntry(..)
            | Self::MissingObject(..)
            | Self::WrongObjectKind(..)
            | Self::NullEdge(..)
            | Self::ConflictingObjectKind(..) => RetentionErrorKind::Malformed,
            Self::Cancelled => RetentionErrorKind::Cancelled,
            Self::Malformed(..) => RetentionErrorKind::Malformed,
            Self::PseudorefIo { .. } => RetentionErrorKind::Read,
            Self::Read(..) => RetentionErrorKind::Read,
        }
    }

    pub(super) fn reflog(log: crate::refs::ImportedReflog) -> Self {
        match log.into_end() {
            crate::refs::ReflogReadEnd::Eof => Self::ReflogIncomplete,
            crate::refs::ReflogReadEnd::Limit(bound) => Self::ReflogLimit(bound),
            crate::refs::ReflogReadEnd::Cancelled => Self::Cancelled,
            crate::refs::ReflogReadEnd::Io(error) => Self::read(error),
        }
    }

    pub(super) fn malformed(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Malformed(Box::new(error))
    }

    pub(super) fn from_prune(error: super::PruneCause) -> Self {
        match error {
            super::PruneCause::Incomplete(error) => error,
            super::PruneCause::Cancelled => Self::Cancelled,
            super::PruneCause::Io(error) => Self::read(error),
            error => Self::read(error),
        }
    }

    pub(super) fn read(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Read(Box::new(error))
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::RetentionError;

    #[test]
    fn prune_inventory_conditions_keep_their_category() {
        let error = RetentionError::from_prune(super::super::PruneCause::Incomplete(
            RetentionError::LooseObjectEntryLimit,
        ));
        assert!(matches!(error, RetentionError::LooseObjectEntryLimit));
        assert_eq!(error.kind(), super::RetentionErrorKind::Limit);
        let cancelled = RetentionError::from_prune(super::super::PruneCause::Cancelled);
        assert_eq!(cancelled.kind(), super::RetentionErrorKind::Cancelled);
    }

    #[test]
    fn read_preserves_source_type() {
        let error = RetentionError::read(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        let source = error
            .source()
            .unwrap()
            .downcast_ref::<std::io::Error>()
            .unwrap();
        assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(error.to_string(), "reading retention input");
    }
}
