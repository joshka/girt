//! SHA-1/SHA-256 pack v2/v3 and index v1/v2 reading and caller-owned artifact writing.
//!
//! # Reading and writing packs
//!
//! - [`crate::Objects`] opens a bounded snapshot of loose objects and indexed packs. Refresh or
//!   reopen it to discover packs published after the snapshot was created.
//! - [`write_pack`] builds a pack and index from explicit [`PackObject`] values. The caller owns
//!   installation; writing the artifacts does not update a repository or its references.
//! - [`write_pack_with_compression`] optionally searches for deltas under [`DeltaOptions`].
//!   [`PackWriteLimits`] bounds the work and output; [`PackWritten`] describes the result.
//!
//! - [`rebuild_index`] regenerates a damaged or missing `.idx` from its `.pack` and replaces it
//!   atomically.
//!
//! Pack decoding limits belong to [`crate::PackLimits`] and [`crate::ReadLimits`]. Neither pack
//! reading nor writing selects refs or decides which objects should remain reachable.

mod compression;
pub(crate) mod delta;
mod file;
pub(crate) mod index;
pub(crate) mod reader;
mod rebuild;
mod reuse;
pub(crate) mod write;

pub use compression::{DeltaOptions, DeltaStats, PackCompression};
#[cfg(test)]
pub(crate) use file::counters;
pub(crate) use file::{BaseKinds, FilePack};
#[cfg(test)]
pub(crate) use reader::Pack;
pub use rebuild::{
    IndexRebuildError, IndexRebuildLimits, IndexRebuildOutcome, IndexRebuilt, rebuild_index,
};
pub(crate) use reuse::{Packed, ReuseError, write_reusing, write_transfer};
pub use write::{
    PackObject, PackWriteError, PackWriteLimits, PackWritten, write_pack,
    write_pack_with_compression,
};

#[cfg(test)]
mod tests;
