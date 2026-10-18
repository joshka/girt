//! Caller-owned bitmap bytes computed from verified object contents, never existing bitmap claims.
use std::sync::atomic::AtomicBool;

use crate::{ObjectId, Objects, ReadLimits};

mod encode;
mod graph;
mod midx;
mod reachable;

/// Commit selection for a new bitmap index.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct WriteOptions {
    /// Explicit commit IDs. Empty selects tips first, then evenly spaced remaining commit IDs.
    pub commits: Vec<ObjectId>,
    /// Maximum selected commits, including explicit selections (default 64).
    pub max_commits: usize,
}
impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            commits: Vec::new(),
            max_commits: 64,
        }
    }
}

/// Bounds graph construction, selected-commit traversal and encoded output independently.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct WriteLimits {
    /// Maximum inventory object occurrences, including MIDX duplicates (5 million).
    /// Also bounds the graph's ID-position lookup table.
    pub max_objects: usize,
    /// Maximum primary packs in a MIDX (4096).
    pub max_packs: usize,
    /// Maximum graph edges retained (20 million).
    pub max_edges: usize,
    /// Maximum total decoded payload bytes used to build the graph (1 GiB).
    pub max_input_bytes: u64,
    /// Maximum ordering/traversal node and edge visits plus cached word unions (100 million).
    pub max_visits: usize,
    /// Maximum combined returned artifact bytes (64 MiB).
    /// Separately caps retained selected-closure words; a full cache falls back to walking.
    pub max_bytes: usize,
    /// Bounds each verified object read.
    pub read: ReadLimits,
}
impl Default for WriteLimits {
    fn default() -> Self {
        Self {
            max_objects: 5_000_000,
            max_packs: 4096,
            max_edges: 20_000_000,
            max_input_bytes: 1024 * 1024 * 1024,
            max_visits: 100_000_000,
            max_bytes: 64 * 1024 * 1024,
            read: ReadLimits::default(),
        }
    }
}

/// Complete bitmap and reverse-index bytes bound to one inventory checksum.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Written {
    /// Associated pack or MIDX checksum.
    pub anchor: ObjectId,
    /// Number of inventory objects.
    pub objects: usize,
    /// Number of selected commit records.
    pub commits: usize,
    /// Complete `.bitmap` file.
    pub bitmap: Vec<u8>,
    /// Complete `.rev` file; MIDX output also embeds the same order in its RIDX chunk.
    pub reverse_index: Vec<u8>,
}

/// A complete MIDX plus its checksum-associated bitmap/reverse-index artifacts.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct MultiPackWritten {
    /// Complete MIDX bytes including RIDX and BTMP chunks.
    pub index: Vec<u8>,
    /// Associated bitmap and reverse index.
    pub bitmap: Written,
}

/// Bitmap output could not be established from a complete verified inventory.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WriteError {
    /// The selected primary pack does not exist in this snapshot.
    #[error("bitmap pack {0} is unavailable")]
    Pack(ObjectId),
    /// A selected ID is absent or is not a commit.
    #[error("bitmap selection {0} is not an inventory commit")]
    Commit(ObjectId),
    /// Full graph closure would require an object outside the selected inventory.
    #[error("bitmap inventory does not contain reachable object {0}")]
    Closure(ObjectId),
    /// A bounded operation would exceed its allowance.
    #[error("bitmap writing limit exceeded: {0}")]
    Limit(&'static str),
    /// Shallow boundaries cannot establish full-DAG bitmap claims.
    #[error("bitmap writing requires an unshallow object graph")]
    Shallow,
    /// Graph decoding, parsing or edge-kind validation failed.
    #[error("validating bitmap graph")]
    Graph(#[from] super::Error),
    /// A pinned pack/index failed verification or changed.
    #[error("reading bitmap source")]
    Read(#[from] crate::ObjectReadError),
    /// MIDX encoding failed.
    #[error("encoding bitmap multi-pack index")]
    MultiPack(#[from] crate::multi_pack_index::WriteError),
    /// Encoded MIDX metadata failed structural validation.
    #[error("validating bitmap multi-pack index")]
    Index(#[from] crate::multi_pack_index::Error),
    /// The caller requested cancellation; no artifacts are returned.
    #[error("bitmap writing cancelled")]
    Cancelled,
}

impl Objects {
    /// Encodes a pack bitmap and reverse index from verified contents without changing storage.
    ///
    /// The selected primary pack must contain its entire graph closure, including unselected
    /// commits' dependencies. Existing bitmap claims are never trusted. Explicit selection is
    /// deduplicated; the default selects graph tips first and samples remaining commits by ID.
    /// Selection is deterministic, not Git's compression/commit-selection heuristic. Records use
    /// EWAH runs/literals without XOR dependencies and include a lookup table.
    ///
    /// Keep maintenance exclusion through encoding and installation: retained handles alone cannot
    /// prevent another process from retiring the named pack. Source generations are rechecked
    /// before returning. The returned artifacts are private bytes, not installed files.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError`] for unavailable packs, incomplete or shallow graphs, corrupt sources,
    /// invalid selections, cancellation or exhausted limits. No partial output is returned.
    pub fn encode_pack_bitmap(
        &self,
        pack: ObjectId,
        options: WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
    ) -> Result<Written, WriteError> {
        check(cancel)?;
        if !self.shallow_roots().is_empty() {
            return Err(WriteError::Shallow);
        }
        let packs = self.primary_packs_with_paths();
        let (_, pack) = packs
            .iter()
            .find(|(_, source)| source.checksum() == pack)
            .ok_or(WriteError::Pack(pack))?;
        let count = pack.object_count();
        bound_objects(count, limits)?;
        pack.verify_index()?;
        pack.check_unchanged()?;
        let ids: Vec<_> = (0..count)
            .map(|position| pack.identity_at(position))
            .collect();
        let order: Vec<_> = pack.positions_by_offset()?.collect();
        let graph = graph::Graph::read(
            self.object_format(),
            &ids,
            &order,
            limits,
            cancel,
            |position| pack.read(position, limits.read, cancel),
        )?;
        let written = encode::artifacts(
            pack.checksum(),
            &order,
            &graph,
            options,
            limits,
            false,
            cancel,
        )?;
        pack.check_unchanged()?;
        Ok(written)
    }

    /// Encodes a primary-store MIDX and associated bitmap/reverse index without changing files.
    ///
    /// Uses the same graph/selection contract as [`Self::encode_pack_bitmap`]. The preferred pack
    /// comes first in pseudo-pack order and wins duplicate IDs; absent an explicit preference, the
    /// largest pack wins, with checksum order breaking ties. Output includes RIDX and BTMP chunks.
    ///
    /// # Errors
    ///
    /// Returns [`WriteError`] for incomplete inventories, invalid inputs, cancellation or bounds.
    pub fn encode_multi_pack_bitmap(
        &self,
        midx: crate::multi_pack_index::WriteOptions,
        options: WriteOptions,
        limits: WriteLimits,
        cancel: &AtomicBool,
    ) -> Result<MultiPackWritten, WriteError> {
        midx::encode(self, midx, options, limits, cancel)
    }
}

fn bound_objects(count: usize, limits: WriteLimits) -> Result<(), WriteError> {
    if count > limits.max_objects || count > u32::MAX as usize {
        return Err(WriteError::Limit("objects"));
    }
    Ok(())
}
pub(super) fn check(cancel: &AtomicBool) -> Result<(), WriteError> {
    if super::check(cancel).is_err() {
        Err(WriteError::Cancelled)
    } else {
        Ok(())
    }
}
