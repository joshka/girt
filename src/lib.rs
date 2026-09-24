//! An incremental Rust library for Git's data formats and storage.
//!
//! # Library contents
//!
//! - [`Repository`] and [`OpenError`]: explicit-path opening with local format detection.
//! - [`refs`]: validated reference names, loose/packed reads, symbolic resolution, and conditional
//!   single-reference updates explicitly without reflogs.
//! - [`Config`] and [`ConfigError`]: byte-oriented parsing of one configuration source.
//! - [`Objects`], [`Object`], [`PackLimits`], and [`ReadLimits`]: bounded loose/packed reads.
//! - [`ObjectReadError`]: packed storage corruption, unsupported formats, and resource failures.
//! - [`ObjectFormat`]: recognized Git object hash formats.
//! - [`ObjectId`]: SHA-1 object identity, hashing blob bytes, and hexadecimal parsing.
//! - [`Tree`], [`TreeEntry`], and [`EntryMode`]: in-memory tree payloads and identity.
//! - [`Commit`], [`CommitFields`], [`Signature`], and [`CommitHeader`]: commit payloads and
//!   identity.
//! - [`Tag`], [`TagFields`], [`ObjectKind`], and [`TagError`]: annotated tag payloads and identity.
//! - [`CommitError`]: commit parsing and construction failures.
//! - [`TreeError`]: tree parsing and structural validation failures.
//! - [`encode_blob`]: uncompressed Git blob encoding.
//! - [`LooseObjects`]: loose blob, tree, commit, and tag reads and writes, with a usage example and
//!   storage assumptions.
//! - [`Error`] and [`ParseObjectIdError`]: storage and identity-parsing failures.
//!
//! The current API is experimental and supports SHA-1 loose objects and pack/index v2 reads.
//! Files references support reads, symbolic resolution, and explicit no-reflog updates.
//! The working-tree index, pack writing, upward discovery, and working-tree conversion are
//! not implemented.

mod commit;
pub mod config;
mod loose;
mod object;
mod objects;
mod pack;
pub mod refs;
mod repository;
mod tag;
mod tree;

pub use commit::{Commit, CommitError, CommitFields, CommitHeader, Signature};
pub use config::{Config, ConfigError};
pub use loose::{Error, LooseObjects};
pub use object::{ObjectFormat, ObjectId, ParseObjectIdError, encode_blob};
pub use objects::{Object, ObjectReadError, Objects, PackLimits, ReadLimits};
pub use repository::{OpenError, Repository};
pub use tag::{ObjectKind, Tag, TagError, TagFields};
pub use tree::{EntryMode, Tree, TreeEntry, TreeError};
