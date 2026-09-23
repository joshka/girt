//! An incremental Rust library for Git's data formats and storage.
//!
//! # Library contents
//!
//! - [`ObjectId`]: SHA-1 object identity, hashing blob bytes, and hexadecimal parsing.
//! - [`encode_blob`]: uncompressed Git blob encoding.
//! - [`LooseObjects`]: loose blob reads and writes, with a usage example and storage assumptions.
//! - [`Error`] and [`ParseObjectIdError`]: storage and identity-parsing failures.
//!
//! The current API is experimental and supports SHA-1 loose blobs. Trees, commits, tags,
//! references, the index, packfiles, repository discovery, and working-tree conversion are not
//! implemented.

mod loose;
mod object;

pub use loose::{Error, LooseObjects};
pub use object::{ObjectId, ParseObjectIdError, encode_blob};
