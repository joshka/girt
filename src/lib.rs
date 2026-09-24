//! An incremental Rust library for Git's data formats and storage.
//!
//! # Library contents
//!
//! - [`Repository`] and [`OpenError`]: explicit-path opening with local format detection.
//! - [`refs`]: validated reference names, loose/packed reads, symbolic resolution, and conditional
//!   single-reference updates explicitly without reflogs.
//! - [`Config`] and [`ConfigError`]: byte-oriented parsing of one configuration source.
//! - [`Objects`], [`Object`], [`PackLimits`], and [`ReadLimits`]: bounded loose/packed reads.
//! - [`fetch`]: upload-pack v0, a local server adapter, and validated object installation.
//! - [`push`]: bounded graph selection and conditional receive-pack v0 branch/tag publication.
//! - [`transport`]: owned transport cancellation, deadlines, and process lifetime contracts.
//! - [`write_pack`]: bounded pack/index v2 artifact generation from explicit objects.
//! - [`ObjectReadError`]: packed storage corruption, unsupported formats, and resource failures.
//! - [`HistoryLimits`] and [`HistoryError`]: bounded walks, ancestry queries, and merge bases.
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
//! The current API is experimental and supports SHA-1 loose objects, pack/index v2 reads,
//! caller-owned pack/index v2 exports, object-only fetch, and conditional branch/tag push. Fetch
//! and push accept v0 streams or local Git server adapters.
//! Files references support reads, symbolic resolution, and explicit no-reflog updates.
//! The working-tree index, upward discovery, and working-tree conversion are
//! not implemented.

mod commit;
pub mod config;
pub mod fetch;
mod history;
mod loose;
mod object;
mod objects;
pub mod pack;
mod packet;
pub mod push;
pub mod refs;
mod repository;
mod tag;
pub mod transport;
mod tree;

pub use commit::{Commit, CommitError, CommitFields, CommitHeader, Signature};
pub use config::{Config, ConfigError};
pub use history::{HistoryError, HistoryLimits};
pub use loose::{Error, LooseObjects};
pub use object::{ObjectFormat, ObjectId, ParseObjectIdError, encode_blob};
pub use objects::{Object, ObjectReadError, Objects, PackLimits, ReadLimits};
pub use pack::{PackObject, PackWriteError, PackWriteLimits, PackWritten, write_pack};
pub use repository::{OpenError, Repository};
pub use tag::{ObjectKind, Tag, TagError, TagFields};
pub use tree::{EntryMode, Tree, TreeEntry, TreeError};
