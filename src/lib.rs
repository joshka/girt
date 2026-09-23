//! An incremental Rust library for Git's data formats and storage.
//!
//! # Library contents
//!
//! - [`ObjectFormat`]: recognized Git object hash formats.
//! - [`ObjectId`]: SHA-1 object identity, hashing blob bytes, and hexadecimal parsing.
//! - [`Tree`], [`TreeEntry`], and [`EntryMode`]: in-memory tree payloads and identity.
//! - [`TreeError`]: tree parsing and structural validation failures.
//! - [`encode_blob`]: uncompressed Git blob encoding.
//! - [`LooseObjects`]: loose blob reads and writes, with a usage example and storage assumptions.
//! - [`Error`] and [`ParseObjectIdError`]: storage and identity-parsing failures.
//!
//! The current API is experimental and supports SHA-1 loose blobs and in-memory trees. Commits,
//! tags, references, the index, packfiles, repository discovery, and working-tree conversion are
//! not implemented.

mod loose;
mod object;
mod tree;

pub use loose::{Error, LooseObjects};
pub use object::{ObjectFormat, ObjectId, ParseObjectIdError, encode_blob};
pub use tree::{EntryMode, Tree, TreeEntry, TreeError};
