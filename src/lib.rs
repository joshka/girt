//! An incremental Rust library for Git's data formats and storage.
//!
//! # Reading a repository
//!
//! Open an explicit path with [`Repository::open`], then retain an [`Objects`] reader with chosen
//! [`PackLimits`]. Loose objects are read live; packs form an immutable snapshot that survives
//! repacking. Reopen to discover new packs. [`Objects::read`] checks framing and identity under
//! per-read [`ReadLimits`]; parse its exact bytes with [`Commit`], [`Tree`], or [`Tag`] when
//! structured fields are needed. Repository history queries follow complete commit ancestry under
//! [`HistoryLimits`] without relying on timestamps.
//!
//! # Creating and finding a repository
//!
//! [`Repository::init`] creates a bare or ordinary SHA-1 repository with unborn `main` and refuses
//! reinitialization. [`Repository::discover`] searches physical ancestors from an existing
//! directory; [`Repository::discover_with_ceiling`] bounds that search to an inclusive ancestor.
//! Discovery stops at malformed or unsupported metadata rather than selecting an outer repository.
//!
//! # Fetching and publishing references
//!
//! Build optional [`fetch::KnownHistory`] from verified local objects before negotiation. Local
//! and stream fetches return [`fetch::ReceivedFetch`]. HTTP/SSH downloads instead own unvalidated
//! bytes and the exact negotiation history; move them to a caller-managed blocking worker and
//! validate them there. The caller bounds queued downloads and active workers, then joins the
//! work even after requesting cancellation.
//!
//! [`fetch::ReceivedFetch::install`] reopens the destination under explicit snapshot limits and
//! rechecks local dependencies before publishing a pack/index pair. It leaves references unchanged.
//! Coordinate with pruning until a separate conditional [`refs::References::update_without_reflog`]
//! publishes the intended tip. Pack installation and each reference update have separate failure
//! and retry contracts; multiple reference updates are not a transaction.
//!
//! # Preparing and sending a push
//!
//! [`push::PreparedPush`] synchronously verifies selected history, proves required ancestry, and
//! builds bounded pack buffers. Optional receiver roots exclude only history proven within that
//! verified graph. Sending checks current advertised values before attempting commands; the
//! server checks those expected old values again when updating references.
//!
//! Inspect [`push::PushReport`] even after a successful send: individual references may be
//! rejected. [`push::PushError`] distinguishes failure before transmission from uncertain outcomes
//! that retain valid acknowledgements. Inspect remote references before retrying unknown outcomes.
//! Local storage, hashing, graph work, and compression remain synchronous; optional async
//! transports use the caller's runtime. Resource limits apply to the documented phase or read, not
//! total process memory or an operation-wide deadline.
//!
//! # Library contents
//!
//! - [`Repository`], [`OpenError`], [`InitKind`], and [`InitError`]: opening, upward discovery, and
//!   initialization of bare or ordinary SHA-1 repositories.
//! - [`refs`]: validated reference names, loose/packed enumeration and reads, symbolic resolution,
//!   and conditional single-reference updates/deletion explicitly without reflogs.
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
//! and push accept v0 streams or local Git server adapters. The optional `http` feature adds async
//! smart-HTTP(S) adapters; `ssh` adds system OpenSSH adapters on macOS/Linux. Both use a
//! caller-owned Tokio runtime; fetch pack validation remains an explicit synchronous step.
//! Files references support enumeration, reads, symbolic resolution, and explicit no-reflog updates
//! and deletion.
//! The working-tree index and working-tree conversion are not implemented.

mod commit;
pub mod config;
mod edges;
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
pub use pack::{
    DeltaOptions, DeltaStats, PackCompression, PackObject, PackWriteError, PackWriteLimits,
    PackWritten, write_pack, write_pack_with_compression,
};
pub use repository::{InitError, InitKind, OpenError, Repository};
pub use tag::{ObjectKind, Tag, TagError, TagFields};
pub use tree::{EntryMode, Tree, TreeEntry, TreeError};
