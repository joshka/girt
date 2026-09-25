//! SHA-1/SHA-256 pack v2/v3 and index v1/v2 reading and caller-owned artifact writing.

mod compression;
pub(crate) mod delta;
pub(crate) mod index;
pub(crate) mod reader;
pub(crate) mod write;

pub use compression::{DeltaOptions, DeltaStats, PackCompression};
pub(crate) use reader::Pack;
pub(crate) use write::write_controlled;
pub use write::{
    PackObject, PackWriteError, PackWriteLimits, PackWritten, write_pack,
    write_pack_with_compression,
};

#[cfg(test)]
mod tests;
