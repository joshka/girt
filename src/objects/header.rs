//! Kind and size lookups that read object headers without decoding payloads.

use std::sync::atomic::AtomicBool;

use super::Objects;
use crate::{ObjectId, ObjectKind, ObjectReadError, ReadLimits};

/// Kind and payload size recorded in an object's stored header.
///
/// Header lookups do not inflate payloads or hash them, so this is a claim of the stored
/// representation, not identity-verified data: use [`Objects::read`] for that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHeader {
    kind: ObjectKind,
    size: u64,
}

impl ObjectHeader {
    /// Object kind; for a delta, the kind of its resolved base.
    pub fn kind(&self) -> ObjectKind {
        self.kind
    }

    /// Uncompressed payload size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl Objects {
    /// Reads `id`'s kind and size from its stored header without decoding the payload.
    ///
    /// Uses the same store and pack precedence as [`Self::read`]. Packed deltas follow their
    /// chain headers to the base kind; the size comes from the delta's own header. This is the
    /// lookup behind `git cat-file --batch-check`. The empty tree is reported even when not
    /// stored, as [`Self::read`] does.
    ///
    /// # Errors
    ///
    /// Returns corrupt or unsupported headers, I/O errors, missing delta bases, or
    /// [`ObjectReadError::Limit`] when the size exceeds [`ReadLimits::max_object_bytes`] or the
    /// chain exceeds [`ReadLimits::max_delta_depth`].
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, ObjectKind, PackLimits, ReadLimits, Repository};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let id = repo.loose_objects().write_blob(b"seven\n")?;
    /// let objects = repo.objects(PackLimits::default())?;
    /// let header = objects
    ///     .header(id, ReadLimits::default())?
    ///     .ok_or("missing")?;
    /// assert_eq!((header.kind(), header.size()), (ObjectKind::Blob, 6));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn header(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<ObjectHeader>, ObjectReadError> {
        Ok(self
            .locate(id, limits, &AtomicBool::new(false))?
            .map(|(kind, size)| ObjectHeader { kind, size }))
    }

    /// Reads the headers of many objects, visiting packed entries in pack order.
    ///
    /// Equivalent to calling [`Self::header`] for each identity, but shared delta-chain headers
    /// are read once per call and pack reads proceed in file order, which matters for listing a
    /// whole repository. Results pair each identity with its header, or `None` when absent;
    /// their order is unspecified. `cancelled` is checked between objects.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::header`] or [`ObjectReadError::Cancelled`].
    pub fn headers_controlled(
        &self,
        ids: impl IntoIterator<Item = ObjectId>,
        limits: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<Vec<(ObjectId, Option<ObjectHeader>)>, ObjectReadError> {
        Ok(self
            .locate_all(ids, limits, cancelled)?
            .into_iter()
            .map(|(id, found)| (id, found.map(|(kind, size)| ObjectHeader { kind, size })))
            .collect())
    }
}
