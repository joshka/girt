//! SHA-1 pack v2 and index v2 reading and caller-owned artifact writing.

mod delta;
mod index;
mod reader;
mod write;

pub(crate) use reader::Pack;
pub use write::{PackObject, PackWriteError, PackWriteLimits, PackWritten, write_pack};

#[cfg(test)]
mod tests;
