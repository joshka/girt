//! Validation for the internal, depth-free maintenance continuation.

use super::super::known::controlled::KnownRead;
use super::{Advertisement, Bytes, FetchError, FetchLimits, Imported, ObjectId, ReceivedFetch};
use crate::fetch::progress::ValidationObserver;
use crate::fetch::{check_cancelled, connectivity};

impl ReceivedFetch {
    /// Validates a self-contained native prefetch artifact with fallible local dependencies.
    ///
    /// Uses the receive's shared fallback-read budget. Optional tree-boundary discovery is omitted;
    /// connectivity performs full checks within that shared budget. No depth, filter or
    /// explicit-want policy is admitted by this continuation. Nothing is installed or published
    /// here.
    pub(in crate::fetch) fn validate_known_controlled<const CONTROLLED: bool>(
        advertisement: Advertisement,
        wants: Vec<ObjectId>,
        pack: impl Into<Bytes>,
        reads: &KnownRead<'_, CONTROLLED>,
        shallow: Vec<ObjectId>,
        limits: FetchLimits,
        observer: &mut ValidationObserver<'_>,
    ) -> Result<Self, FetchError> {
        if !CONTROLLED {
            return Err(FetchError::Unsupported("controlled known read context"));
        }
        let cancel = observer.cancel;
        check_cancelled(cancel)?;
        let format = advertisement.object_format()?;
        let mut pack = pack.into();
        if let Bytes::Memory(data) = pack {
            pack = Bytes::temporary()?;
            pack.append(&data)?;
        }
        let imported = Imported::read_owned(format, pack, reads, limits, observer)?;
        // Keep an empty boundary set: discovery would use trusted reads, so this continuation
        // performs full connectivity checks under the receive's shared read budget.
        let boundaries = super::super::boundaries::TreeBoundaries::new(reads.history(), limits);
        let dependencies = connectivity::validate_with_boundaries(
            &imported.objects,
            reads,
            &wants,
            &shallow,
            None,
            limits,
            cancel,
        )?;
        let complete = reads.complete_dependencies(&dependencies, &boundaries)?;
        let objects = imported.objects.len();
        check_cancelled(cancel)?;
        let received = Self {
            format,
            advertisement,
            wants,
            pack: if objects == 0 {
                Vec::new().into()
            } else {
                imported.objects.pack
            },
            verified: imported.objects.verified,
            index: imported.index,
            checksum: (objects != 0).then_some(imported.checksum),
            objects,
            dependencies,
            complete,
            shallow,
            limits,
            filter_applied: false,
            promisor: false,
            promisor_refs: false,
        };
        observer.complete();
        Ok(received)
    }
}
