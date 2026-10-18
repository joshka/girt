//! Git objects, repositories, and explicit workflows for Rust applications.
//!
//! girt is a Git library for applications that need to read and write Git data directly. Use it to
//! inspect a repository, construct snapshots, traverse history, or publish references. The
//! application chooses when each effect happens: storing a commit does not move a branch, and
//! moving a branch does not update a working tree. The API is experimental.
//!
//! # Design principles
//!
//! Git's on-disk formats and semantics define the supported behavior. girt preserves accepted
//! object bytes and their identities, including non-UTF-8 names and messages. Unsupported cases are
//! refused explicitly rather than silently approximated. Support is specific to each operation;
//! opening a repository does not establish that every Git feature used by that repository is
//! supported.
//!
//! The library models Git concepts directly. A repository locates storage and selects its hash
//! format; objects describe immutable content; references give that content mutable names.
//! Configuration, resource limits, cancellation, and publication policy are explicit where the
//! operation needs them. No application-wide runtime or tracing subscriber is installed.
//!
//! Use [`Repo`] for trusted local use with Git-default discovery, lazy loading, and default limits.
//! Use [`Repository`] for explicit control over configuration, limits, cancellation, and untrusted
//! input. [`Repo::repository`] exposes the underlying repository when an operation needs that
//! control; [`repo`] describes the convenience layer's policies and limitations.
//!
//! # Quick start
//!
//! Add girt to a Rust project with `cargo add girt`. The minimum Rust version is 1.97.1.
//! This example also uses `tempfile` (`cargo add tempfile`) to create a disposable repository:
//!
//! ```rust
//! use girt::{InitKind, ObjectFormat, ObjectKind, PackLimits, ReadLimits, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repository = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("project"),
//!     InitKind::Worktree,
//! )?;
//!
//! let id = repository.loose_objects().write_blob(b"hello Git\n")?;
//! let objects = repository.objects_with(PackLimits::default())?;
//! let blob = objects
//!     .read_with(id, ReadLimits::default())?
//!     .ok_or("blob missing")?;
//!
//! assert_eq!(blob.kind(), ObjectKind::Blob);
//! assert_eq!(blob.data(), b"hello Git\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! The repository is removed when `directory` is dropped. No commit or branch is created: a blob is
//! only bytes. For an existing repository use [`Repository::open`]; to find one above a directory
//! use [`Repository::discover`]. See [`repository`] for the distinction between selecting a
//! location, opening its metadata, and reading its storage.
//!
//! # The Git mental model
//!
//! An [`ObjectId`] identifies a kind and exact payload using Git's header and hash. It carries its
//! SHA-1 or SHA-256 format, but proves neither existence nor object kind. A blob contains file
//! bytes; a [`Tree`] maps byte names and modes to IDs; a [`Commit`] names a tree and ordered
//! parents; a [`Tag`] adds metadata to another object. Objects contain IDs rather than owned child
//! objects, so reading a commit does not recursively read its tree or parents.
//!
//! [`Objects::read`] verifies storage framing and identity. Parsing the returned bytes decodes an
//! object's structure; validation may impose further rules. These stages remain separate so
//! inspection tools can preserve and diagnose unusual Git data without rewriting it. Follow
//! [`tree`] and [`commit`] for exact parsing and construction guarantees.
//!
//! A reference can contain an ID or another reference's name. `HEAD` commonly points to a branch;
//! an unborn branch has a name but no ID yet. [`refs::References::resolve`] follows symbolic names;
//! [`Objects::peel`] follows annotated tag objects. These are different operations.
//!
//! # Where to go next
//!
//! | Task | API and explanation |
//! | --- | --- |
//! | Locate, open, or initialize a repository | [`repository`], [`RepositoryLocation`], [`Repository`] |
//! | Compute or parse full object identities | [`object`], [`ObjectFormat`], [`ObjectId`] |
//! | Decode or construct snapshots and metadata | [`tree`], [`commit`], [`tag`] |
//! | Read loose and packed objects together | [`objects`], [`Objects`], [`ReadLimits`] |
//! | Write individual objects | [`loose`], [`LooseObjects`] |
//! | Read or produce pack artifacts | [`pack`] |
//! | Walk ancestry, test reachability, find merge bases | [`history`] |
//! | Combine content, trees or commits and publish conflicts | [`merge`] |
//! | Cherry-pick, revert or rebase explicit commits in memory | [`replay`] |
//! | Read Git operation state and prepare explicit recovery | [`sequencer`] |
//! | Resolve names, conditionally update refs, inspect reflogs | [`refs`] |
//! | Parse, resolve, interpret, or edit configuration | [`config`] |
//! | Interpret named remotes and refspecs | [`remote`] |
//! | Fetch, push, or clone with explicit publication | [`fetch`], [`push`], [`clone`] |
//! | Use configured-remote convenience workflows | [`workflow`] |
//! | Compose everyday operations on a trusted local repository | [`Repo`], [`repo`] |
//! | Use HTTP/SSH and credentials | [`transport`], [`remote::CredentialStore`] |
//! | Compare snapshots and file contents | [`Objects::compare_trees`], [`content_diff`], [`rewrites`] |
//! | Write unified patches as `git diff` does | [`patch`], [`Objects::write_patch`] |
//! | Read `.gitattributes` and diff drivers | [`attributes`], [`patch::DiffAttributes`] |
//! | Print paths, modes, statuses and dates as Git does | [`display`] |
//! | Manage the index and working tree | [`index`], [`status`], [`checkout`], [`ignore`] |
//! | Plan retention, repack, or coordinate mutation | [`retention`], [`locks`] |
//!
//! The [runnable examples](https://github.com/joshka/girt/tree/main/examples) compose these APIs.
//! The [compatibility record](https://github.com/joshka/girt/blob/main/docs/compatibility.md) describes
//! verified scope. Each API's errors and limitations remain the contract for that operation.
//!
//! # Features and execution
//!
//! There are no default features. Repository metadata, object codecs, local storage, references,
//! configuration, history, and native local transfer work without optional features.
//!
//! | Feature | Adds | Execution requirements |
//! | --- | --- | --- |
//! | `http` | Smart HTTP(S) transport with Rustls | Caller-owned Tokio runtime for transport APIs |
//! | `ssh` | System OpenSSH transport on macOS/Linux | OpenSSH and a caller-owned Tokio runtime |
//! | `tracing` | Operation spans and bounded-work counts | Caller installs and configures a subscriber |
//!
//! Enable features with, for example, `cargo add girt --features http`. [`workflow`] owns
//! synchronous runtime and credential orchestration for its configured-remote convenience
//! operations. Lower-level HTTP/SSH adapters download data for explicit synchronous validation.
//! Local filesystem operations are synchronous; applications choose their scheduling and blocking
//! boundaries.
//!
//! Run these workflows from a source checkout; check each example's arguments and filesystem
//! effects before using existing data:
//!
//! ```sh
//! cargo run --example loose_blob
//! cargo run --example compare_trees
//! cargo run --example content_diff
//! cargo run --example index
//! cargo run --example status
//! cargo run --example checkout
//! ```
//!
//! With `tracing`, DEBUG spans describe operations and phases; TRACE makes individual object reads
//! and writes visible. Run `cargo run --features tracing --example tracing` for a scoped
//! subscriber. HTTP/SSH download spans remain alive through synchronous validation or drop,
//! including time queued for a worker. Spans record categorical outcomes and bounded-work counts;
//! returned errors and reports remain authoritative for recovery. See the
//! [tracing guide](https://github.com/joshka/girt/blob/main/docs/tracing.md) for coverage and overhead.
//!
//! # Limits, failures, and safety
//!
//! Choose [`PackLimits`] when opening a reader and [`ReadLimits`] for individual objects. History
//! adds [`HistoryLimits`] for graph work. Defaults bound untrusted input; a limit failure means the
//! requested work exceeded the selected budget, not necessarily that the repository is corrupt.
//! Increase only the relevant budget after considering the input's trust and available resources.
//! `trusted()` limits remove practical protections and are a caller policy decision.
//!
//! Missing objects are distinct from malformed storage. Object reads return `Ok(None)` for absence;
//! I/O, corruption, unsupported representations, and exceeded limits return errors. Preserve error
//! source chains when reporting failures. Each operation returns its module's typed error; every
//! one converts into the crate-level [`Error`], so application code can return [`Result`] and use
//! `?` across modules, then downcast to recover the typed error. Reference transactions can have
//! partial effects: inspect reported outcomes before retrying. Writing storage requires the
//! filesystem assumptions documented on the owning operation; a pathname alone is not a trust
//! boundary.
//!
//! Working-tree operations have narrower platform, byte-path, filter, and mutation contracts than
//! object storage. Consult [`checkout`] and [`status`] before applying Git data to a filesystem.

// Let library unit tests share the Git-generated integration fixtures.
#[cfg(test)]
extern crate self as girt;
#[cfg(test)]
#[path = "../tests/support/pack_git.rs"]
mod pack_git;

pub mod attributes;
pub mod blame;
mod bytes;
pub mod checkout;
pub mod clone;
mod file_policy;
pub use file_policy::SharedPermissions;

pub mod commit;
pub mod commit_graph;
pub mod config;
pub mod content_diff;
pub mod display;
mod edges;
mod error;
pub use error::{Error, Result};
pub mod fetch;
#[cfg(any(fuzzing, test))]
#[doc(hidden)]
pub mod fuzzing;
pub mod history;
pub mod ignore;
pub mod index;
pub mod locks;
pub mod loose;
pub mod merge;
pub mod object;
pub mod objects;
pub mod pack;
mod packet;
pub mod patch;
pub mod pathspec;
mod peel;
pub mod push;
pub mod refs;
pub mod remote;
pub mod replay;
pub mod repo;
pub mod repository;
pub mod retention;
pub mod revision;
pub mod rewrites;
pub mod sequencer;
mod source_files;
pub mod stash;
pub mod status;
pub mod tag;
pub mod transport;
pub mod tree;
mod tree_compare;
mod wildmatch;
pub mod workflow;

pub mod tree_diff;
pub use commit::{
    Commit, CommitError, CommitFields, CommitHeader, CommitHeaderRef, CommitPayload, IdentityDate,
    IdentityRef, Signature,
};
pub use config::{Config, ConfigError};
pub use history::{HistoryError, HistoryLimits, PathHistoryError, Simplification, SimplifyError};
pub use loose::LooseObjects;
pub(crate) use object::{IdMap, IdSet, PeerIdMap, PeerIdSet};
pub use object::{
    ObjectFormat, ObjectFormatError, ObjectId, ObjectIdPrefix, ObjectKind, ParseObjectIdError,
    ParseObjectIdPrefixError, encode_blob,
};
pub(crate) use objects::UnverifiedObject;
pub use objects::{
    AlternateLimits, IndexedPack, ListLimits, LooseIds, LooseObject, Object, ObjectHeader,
    ObjectReadError, ObjectStore, Objects, PackIds, PackLimits, ReadBlobError, ReadCommitError,
    ReadLimits, ReadTagError, ReadTreeError, ResolvePrefixError,
};
pub use pack::{
    DeltaOptions, DeltaStats, PackCompression, PackObject, PackWriteError, PackWriteLimits,
    PackWritten, write_pack, write_pack_with_compression,
};
pub use peel::{PeelError, PeelFailure, PeelLimits, PeeledObject};
pub use repo::Repo;
pub use repository::{
    ColocationEdit, ColocationError, CreateWorktreeError, InitError, InitKind, InitOptions,
    OpenError, OperationCleanupError, OperationError, OperationLimits, OperationState,
    OrphanWorktreeOptions, ReflogPolicyError, Repository, RepositoryLocation, RepositoryMetadata,
    ShallowError, ShallowRoots, Worktree, WorktreeAdminError, WorktreeDurability, WorktreeError,
    WorktreeLinkStyle, WorktreeRepair, WorktreeRetirement, WorktreeState,
};
pub use tag::{Tag, TagError, TagFields};
pub use tree::{EntryMode, Tree, TreeEntry, TreeError};
pub use tree_compare::{ChangeStatus, TreeChange, TreeCompareError, TreeCompareLimits, TreeValue};

#[cfg(feature = "tracing")]
mod trace;
