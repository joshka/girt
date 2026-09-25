use std::sync::atomic::{AtomicBool, Ordering};

use crate::{
    EntryMode, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits, TreeChange, TreeValue,
};

/// Owned blob sides loaded from a tree change, ready to borrow in [`super::diff`].
///
/// Missing sides become empty buffers. Existence and modes remain on the original [`TreeChange`]:
/// an absent side and a present empty blob compare as unchanged content. Symlinks are compared as
/// their stored target bytes, without dereferencing a filesystem path.
#[derive(Debug)]
pub struct BlobContent {
    /// Original payload, or empty bytes for an addition.
    pub old: Vec<u8>,
    /// Replacement payload, or empty bytes for a deletion.
    pub new: Vec<u8>,
}

impl BlobContent {
    /// Loads and verifies both blob sides without changing storage.
    ///
    /// Accepts regular, executable and symlink modes. Tree/gitlink modes are rejected before any
    /// read. Both sides are read even for equal IDs, so dangling or wrong-kind objects cannot be
    /// hidden by an identity skip. `read` bounds each individual read; `max_bytes` additionally
    /// bounds the combined retained payload bytes, constraining decoding before each read.
    /// Memory also includes [`Objects`]' pack snapshot and per-read reconstruction workspace.
    ///
    /// Storage is synchronous and subject to [`Objects::read`]'s trust/pruning assumptions.
    /// Cancellation is checked before mode validation, between reads and after the final read;
    /// an individual storage read cannot be interrupted. A read error takes precedence over a
    /// cancellation observed after it. Callers own scheduling and may borrow the returned buffers
    /// for repeated pure comparisons under different binary policies.
    ///
    /// # Errors
    ///
    /// Returns [`ContentReadError`] for unsupported modes, missing/wrong-kind objects, storage or
    /// decoding failures, or cancellation. Errors retain the offending side's ID or mode; the
    /// caller retains path and side context in `change`. No partial pair or writes are produced.
    pub fn read(
        objects: &Objects,
        change: &TreeChange,
        read: ReadLimits,
        max_bytes: usize,
        cancel: &AtomicBool,
    ) -> Result<Self, ContentReadError> {
        check(cancel)?;
        validate_mode(change.old)?;
        validate_mode(change.new)?;
        let old = read_side(objects, change.old, read, max_bytes)?;
        check(cancel)?;
        let new = read_side(objects, change.new, read, max_bytes - old.len())?;
        check(cancel)?;
        Ok(Self { old, new })
    }
}

fn check(cancel: &AtomicBool) -> Result<(), ContentReadError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ContentReadError::Cancelled)
    } else {
        Ok(())
    }
}

fn validate_mode(value: Option<TreeValue>) -> Result<(), ContentReadError> {
    if let Some(value) = value {
        match value.mode {
            EntryMode::Blob | EntryMode::Executable | EntryMode::Symlink => (),
            mode => return Err(ContentReadError::UnsupportedMode(mode)),
        }
    }
    Ok(())
}

fn read_side(
    objects: &Objects,
    value: Option<TreeValue>,
    mut limits: ReadLimits,
    remaining: usize,
) -> Result<Vec<u8>, ContentReadError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    limits.max_object_bytes = limits.max_object_bytes.min(remaining);
    let id = value.id;
    let object = objects
        .read(id, limits)
        .map_err(|source| ContentReadError::Read { id, source })?
        .ok_or(ContentReadError::Missing(id))?;
    if object.kind() != ObjectKind::Blob {
        return Err(ContentReadError::NotBlob {
            id,
            actual: object.kind(),
        });
    }
    Ok(object.into_data())
}

/// Loading blob sides failed without returning a partial pair or modifying storage.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ContentReadError {
    /// Trees and gitlinks are not blob payloads.
    #[error("content diff does not support entry mode {0:?}")]
    UnsupportedMode(EntryMode),
    /// A referenced blob is absent.
    #[error("missing blob {0}")]
    Missing(ObjectId),
    /// A leaf ID resolves to a non-blob object.
    #[error("expected blob {id}, found {actual:?}")]
    NotBlob {
        /// Referenced blob ID.
        id: ObjectId,
        /// Verified object kind actually found.
        actual: ObjectKind,
    },
    /// Storage, identity verification or decoding bounds failed.
    #[error("reading blob {id}: {source}")]
    Read {
        /// Blob being read.
        id: ObjectId,
        /// Underlying error, including the effective combined payload limit.
        #[source]
        source: ObjectReadError,
    },
    /// The caller's cancellation flag was observed between synchronous reads.
    #[error("content read cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests;
