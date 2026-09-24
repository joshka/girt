//! SHA-1 pack v2 and index v2 decoding for the repository object reader.

mod delta;
mod index;
mod reader;

pub(crate) use reader::Pack;

#[cfg(test)]
mod tests;
