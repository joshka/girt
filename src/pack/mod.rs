//! SHA-1 pack v2 and index v2 reading and caller-owned artifact writing.

mod compression;
mod delta;
mod import;
mod index;
mod reader;
mod write;

pub use compression::{DeltaOptions, DeltaStats, PackCompression};
pub(crate) use import::Imported;
pub(crate) use reader::Pack;
pub(crate) use write::write_controlled;
pub use write::{
    PackObject, PackWriteError, PackWriteLimits, PackWritten, write_pack,
    write_pack_with_compression,
};

#[cfg(test)]
mod tests;
