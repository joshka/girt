//! SHA-1/SHA-256 pack v2/v3 and index v1/v2 reading and caller-owned artifact writing.
//!
//! A pack stores many logical objects together and may encode an object as a delta against another
//! entry. The `.idx` maps IDs to offsets; it does not change object identity. Callers normally use
//! [`crate::Objects`] to read logical objects without choosing a physical representation. Delta
//! reconstruction can require more work than the final payload size, so per-read input, output,
//! depth, and decoding budgets are separate.
//!
//! # Reading and writing packs
//!
//! - [`crate::Objects`] retains a bounded snapshot of indexed packs and reads loose objects live.
//!   Refresh or reopen it to discover packs published after the snapshot was created.
//! - [`write_pack`] builds a pack and index from explicit [`PackObject`] values. The caller owns
//!   installation; writing the artifacts does not update a repository or its references.
//! - [`write_pack_with_compression`] optionally searches for deltas under [`DeltaOptions`].
//!   [`PackWriteLimits`] bounds the work and output; [`PackWritten`] describes the result.
//! - [`rebuild_index`] regenerates a damaged or missing `.idx` from its `.pack` and replaces it
//!   atomically.
//!
//! Pack decoding limits belong to [`crate::PackLimits`] and [`crate::ReadLimits`]. Neither pack
//! reading nor writing selects refs or decides which objects should remain reachable.
//!
//! # Failures and publication
//!
//! [`crate::ObjectReadError`] identifies unsupported versions, damaged artifacts, missing delta
//! bases, and exhausted limits. An `.idx` without its `.pack` is an opening error; a pack without
//! an index is not visible to ordinary object readers. Keep a reader alive across related reads to
//! reuse its opened indexes. [`PackWriteError`] can leave partial caller-owned output: discard both
//! artifacts on failure. Creating a pack never establishes graph completeness, reachability, or
//! permission to delete another copy; those are separate transfer or retention decisions.
//!
//! # Example
//!
//! Write a pack and its index, then read the object back through a fresh [`crate::Objects`]
//! snapshot. The example writes the final filenames directly, which is safe only because no other
//! process can see this private, disposable repository. Plain writes are not an atomic
//! installation: a concurrent reader could observe a partial index, and an existing name could be
//! truncated. Install into a shared repository through validated fetch installation
//! ([`crate::fetch::ReceivedFetch::install`]) or
//! [`Repository::repack_retained`](crate::Repository::repack_retained) instead.
//!
//! ```
//! use girt::{
//!     InitKind, ObjectFormat, ObjectKind, PackLimits, PackObject, PackWriteLimits, ReadLimits,
//!     Repository, write_pack,
//! };
//!
//! let directory = tempfile::tempdir()?;
//! let path = directory.path().join("repo.git");
//! let repository = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
//! let data = b"packed content\n";
//! let id = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, data);
//! let input = PackObject {
//!     id,
//!     kind: ObjectKind::Blob,
//!     data,
//! };
//!
//! let (mut pack, mut index) = (Vec::new(), Vec::new());
//! let written = write_pack(
//!     ObjectFormat::Sha1,
//!     &[input],
//!     &mut pack,
//!     &mut index,
//!     PackWriteLimits::default(),
//! )?;
//!
//! let base = repository
//!     .object_dir()
//!     .join("pack")
//!     .join(format!("pack-{}", written.checksum));
//! // Private fixture only: the index names a pack, so write the pack first.
//! std::fs::write(base.with_extension("pack"), &pack)?;
//! std::fs::write(base.with_extension("idx"), &index)?;
//!
//! let objects = repository.objects_with(PackLimits::default())?;
//! let object = objects
//!     .read_with(id, ReadLimits::default())?
//!     .expect("indexed pack is visible");
//! assert_eq!(object.data(), data);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod compression;
pub(crate) mod delta;
mod file;
#[cfg(feature = "tracing")]
pub(crate) use file::is_damage_io;
pub(crate) use file::{DamageHistory, ReadFailure};
pub(crate) mod index;
pub(crate) mod reader;
mod rebuild;
mod reuse;
pub(crate) mod write;

pub use compression::{DeltaOptions, DeltaStats, PackCompression};
#[cfg(test)]
pub(crate) use file::counters;
pub(crate) use file::{BaseKinds, FilePack, HeaderScope, read_exact_at};
#[cfg(test)]
pub(crate) use reader::Pack;
pub use rebuild::{
    IndexRebuildError, IndexRebuildLimits, IndexRebuildOutcome, IndexRebuilt, rebuild_index,
};
pub(crate) use reuse::{DeltaFraming, Packed, ReuseError, write_reusing, write_transfer};
pub use write::{
    PackObject, PackWriteError, PackWriteLimits, PackWritten, write_pack,
    write_pack_with_compression,
};

#[cfg(test)]
mod header_tests;
#[cfg(test)]
mod tests;

/// Index damage that framing, CRCs and checksums cannot see.
#[cfg(test)]
pub(crate) mod damage {
    use std::path::Path;

    use crate::ObjectFormat;
    use crate::object::Hasher;

    /// Swaps the offsets and CRCs of the two entries a v2 index places last in the pack, then
    /// rewrites the index checksum.
    ///
    /// Each identity then names the other's intact bytes: the index structure, every entry CRC
    /// and the pack checksum still verify, so only decoding with an identity check can tell.
    pub(crate) fn swap_last_entries(index: &Path, format: ObjectFormat) {
        let width = format.digest_len();
        let mut bytes = std::fs::read(index).unwrap();
        assert_eq!(&bytes[..8], b"\xfftOc\0\0\0\x02", "v2 index");
        let word =
            |bytes: &[u8], at: usize| u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap());
        let count = word(&bytes, 8 + 255 * 4) as usize;
        let crcs = 8 + 256 * 4 + count * width;
        let offsets = crcs + count * 4;
        let mut order: Vec<usize> = (0..count).collect();
        order.sort_by_key(|&position| word(&bytes, offsets + position * 4));
        let (first, second) = (order[count - 1], order[count - 2]);
        for table in [crcs, offsets] {
            for byte in 0..4 {
                bytes.swap(table + first * 4 + byte, table + second * 4 + byte);
            }
        }
        let end = bytes.len() - width;
        let mut hash = Hasher::new(format);
        hash.update(&bytes[..end]);
        bytes[end..].copy_from_slice(hash.finalize().as_bytes());
        // Published indexes are read-only; replace rather than rewrite in place.
        std::fs::remove_file(index).unwrap();
        std::fs::write(index, bytes).unwrap();
    }
}
