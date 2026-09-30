//! Git objects, repositories, references, and explicit transfer workflows for Rust applications.
//!
//! girt opens a repository at a chosen path and works with its Git data directly. The application
//! chooses which objects to write, which references to publish, and when to update a working tree.
//! The API is experimental, but its current operations document their accepted formats, effects,
//! limits, and recovery obligations.
//!
//! # First use
//!
//! Add `girt = "0.1"` to a project using Rust 1.97.1 or newer. Compute a Git blob identity
//! without opening a repository:
//!
//! ```rust
//! use girt::{ObjectFormat, ObjectKind};
//!
//! let id = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"hello");
//! assert_eq!(id.to_string(), "b6fc4c620b67d95f953a5c1c1230aaab5db5a1b0");
//! ```
//!
//! To store a blob, add `tempfile = "3"` for this disposable example so it does not change an
//! existing repository:
//!
//! ```rust
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repository = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("project"),
//!     InitKind::Worktree,
//! )?;
//! let id = repository.loose_objects().write_blob(b"hello\n")?;
//! assert_eq!(repository.loose_objects().read_blob(id, 1024)?, b"hello\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! Writing an object does not move a branch or populate the working tree. The temporary directory
//! is removed when `directory` is dropped. For an existing repository, start with
//! [`Repository::open`] and the read-only [`Objects`] interface.
//!
//! In the [source checkout](https://github.com/joshka/girt), run
//! `cargo run --example loose_blob` to write and read a blob in disposable storage. See
//! [`LooseObjects`] for storage assumptions and the
//! [contributor guide](https://github.com/joshka/girt/blob/joshka/platform-validation/CONTRIBUTING.md)
//! for setup and checks.
//!
//! # How the APIs fit together
//!
//! Read the task sections below in the order your application needs them:
//!
//! 1. [Read a repository](#reading-a-repository), then use [`ObjectId`], [`Tree`], [`Commit`], and
//!    [`Tag`] for object data. [`Repository`] chooses the repository format; an [`Objects`] reader
//!    holds a pack snapshot until it is reopened.
//! 2. Use [`refs`] to resolve or conditionally publish references when objects must become
//!    reachable. Object installation and reference updates are separate steps.
//! 3. Use [`remote`] to interpret explicit remote configuration, then [`fetch`], [`clone`], or
//!    [`push`] for transfer and publication. The application still owns endpoint selection,
//!    authorization, and retry decisions.
//! 4. Use [`index`], [`status`], and [`checkout`] when the application also manages a working tree.
//!    Status and checkout use raw byte and platform rules that differ from Git's default CLI.
//!
//! Follow a module link for its API map and the owning methods' contracts. The runnable
//! [examples](https://github.com/joshka/girt/tree/joshka/platform-validation/examples) show
//! full workflows; check each one's input and effects before running it. The
//! [compatibility record](https://github.com/joshka/girt/blob/joshka/platform-validation/docs/compatibility.md)
//! retains tested scope and historical evidence.
//!
//! # Features and platforms
//!
//! Local storage, references, and native local transfer need no optional feature. `http` adds
//! smart-HTTP(S) transfer; `ssh` adds system OpenSSH transfer on macOS/Linux. Both network paths
//! use a caller-owned Tokio runtime. Their fetch adapters return downloaded data for explicit
//! synchronous validation. `tracing` adds operation spans without installing a subscriber. Raw
//! working-tree status and checkout are supported on macOS/Linux; their module pages describe
//! accepted paths and mutation limits.
//!
//! # Object identities
//!
//! [`ObjectId`] carries SHA-1 or SHA-256 digest bytes. [`ObjectFormat::hash_object`] hashes exact
//! payloads with Git framing; it does not validate payload structure or translate embedded IDs.
//! Hex parsing accepts full 40/64-digit identities, while [`ObjectId::from_hex`] requires an
//! explicit format. Null IDs are format-specific sentinels. Codecs, loose/packed storage and
//! traversal support both formats. Tree construction and all decoded payload parsing take an
//! explicit format, including empty trees. Commit construction derives the format from its tree
//! and requires matching parents; tag construction derives it from the target. References,
//! reflogs and working-tree index v2/v3/v4 use the repository format. Remote discovery identifies
//! SHA-1/SHA-256 advertisements before transfer. Wire fetch and push support both formats;
//! native local transfer also supports both without invoking Git.
//!
//! # Creating and finding a repository
//!
//! [`Repository::init`] creates a bare or ordinary SHA-1 or SHA-256 repository with unborn `main`
//! and refuses reinitialization. [`Repository::discover`] searches physical ancestors from an
//! existing directory; [`Repository::discover_with_ceiling`] bounds that search to an inclusive
//! ancestor. Discovery stops at malformed or unsupported metadata rather than selecting an outer
//! repository.
//!
//! # Reading a repository
//!
//! Open an explicit path with [`Repository::open`], then retain an [`Objects`] reader with chosen
//! [`PackLimits`]. Loose objects are read live; packs form an immutable snapshot that survives
//! repacking. Reopen to discover new packs. [`Objects::read`] checks framing and identity under
//! per-read [`ReadLimits`]; parse its exact bytes with [`Commit`], [`Tree`], or [`Tag`] when
//! structured fields are needed. Repository history queries follow commit ancestry up to declared
//! shallow boundaries under [`HistoryLimits`] without relying on timestamps.
//!
//! # Comparing snapshots
//!
//! Pass explicit tree IDs to [`Objects::compare_trees`], using `None` for an empty side. Returned
//! [`TreeChange`] records preserve path bytes and old/new IDs and modes in raw full-path order.
//! Equal subtree IDs skip reads, so comparison does not establish their validity or existence.
//! Changed directories are traversed; gitlinks stay leaves and leaf targets are not read.
//! [`TreeCompareLimits`] bounds traversal and eager output; cancellation is cooperative between
//! synchronous processing steps. Run `cargo run --example compare_trees` for a disposable example.
//!
//! Load a change's regular, executable or symlink payloads with
//! [`content_diff::BlobContent::read`], then borrow them in [`content_diff::diff`]. The pure engine
//! preserves arbitrary bytes and LF boundaries, returning unchanged, binary-changed or shortest
//! line edits with explicit resource limits. Absence and mode changes remain on the original tree
//! record; empty and absent payloads compare equal. Gitlinks require a separate submodule policy.
//! Run `cargo run --example content_diff` for the composed operation.
//!
//! # Planning from remote configuration
//!
//! [`remote::Remote::find`] reads named URLs and fetch/push refspecs from [`Repository::config`].
//! Map explicit resolved sources with [`remote::Refspecs::map`], or select advertised fetch tips
//! with [`remote::Refspecs::map_advertisement`]. Plans preserve force intent but require separate
//! endpoint selection, update authorization and conditional publication. No transfer is initiated.
//!
//! # Fetching and publishing references
//!
//! [`fetch::FetchRequest::prepare`] captures local ref values with explicit force authorization and
//! reflog policy. Its local/HTTP/SSH adapters map the advertisement actually used for transfer.
//! [`fetch::FetchReady::finish`] installs objects before checking update rules and conditionally
//! publishing remote-tracking refs and tags. Local branch destinations are unsupported. Inspect
//! [`fetch::FetchFinishError`] for installed objects and possible partial transaction effects.
//! [`fetch::FetchRequest::with_prune`] opts into remote-tracking deletion; depth requests use
//! [`fetch::FetchRequest::with_depth`] with coordinated shallow publication. `FETCH_HEAD` and
//! implicit tag following remain caller policy; this is not full CLI fetch.
//! Run `cargo run --example fetch_remote` for a disposable workflow with named remote
//! configuration.
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
//! and retry contracts. Use [`refs::References::transaction`] to check a whole batch before
//! sequential publication and choose explicit reflog policy; inspect partial outcomes on
//! publication failure.
//!
//! # Cloning without checkout
//!
//! [`clone::CloneRequest::prepare_tracking`] selects a new destination, layout, stored origin URL,
//! branch policy and reflog policy. Receive from an explicit local/HTTP/SSH endpoint, validate any
//! owned network download on a caller-controlled worker, then call [`clone::CloneReady::finish`].
//! Both layouts retain all remote-tracking branches and tags, with one selected local branch or
//! detached HEAD. No index or working files are populated; an ordinary clone has Git's no-checkout
//! state. Inspect [`clone::CloneError`] for initialized, installed, configured and published state
//! after failure. Run `cargo run --example clone_repository` for the lifecycle.
//!
//! # Preparing and sending a push
//!
//! [`push::PreparedPush`] synchronously verifies selected history, proves required ancestry, and
//! builds bounded pack buffers. Optional receiver roots exclude only history proven within that
//! verified graph. Sending checks required advertised capabilities before attempting commands;
//! the server checks each command's exact expected old value when updating references.
//!
//! Inspect [`push::PushReport`] even after a successful send: individual references may be
//! rejected. [`push::PushError`] distinguishes failure before transmission from uncertain outcomes
//! that retain valid acknowledgements. Inspect remote references before retrying unknown outcomes.
//! Local storage, hashing, graph work, and compression remain synchronous; optional async
//! transports use the caller's runtime. Resource limits apply to the documented phase or read, not
//! total process memory or an operation-wide deadline.
//!
//! # Reading and replacing the index
//!
//! [`index::Index`] parses and encodes bounded SHA-1/SHA-256 v2/v3/v4 indexes with byte paths, stat
//! words and conflict stages. [`Repository::read_index`] distinguishes missing storage from an
//! empty index; [`Repository::edit_index`] holds `index.lock` while the caller derives and
//! publishes changes. Optional extensions round-trip; edits discard derived caches, retain
//! resolve-undo records and refuse unknown optional extensions. Index operations never create
//! working files or apply staging policy. Run `cargo run --example index` for a disposable
//! repository example.
//!
//! # Observing working-tree status
//!
//! [`Repository::raw_status`] separates staged tree/index changes, raw index/worktree changes,
//! conflicts and unchecked gitlinks. It verifies content instead of trusting cached stat data,
//! applies no normalization or ignores, and never refreshes the index. macOS/Linux traversal
//! avoids symlink ancestors and repository metadata. Reports are observations, not atomic
//! snapshots or checkout preconditions. Run `cargo run --example status` for a disposable fixture.
//!
//! # Checking out a selected tree
//!
//! [`Repository::checkout_tree`] materializes raw blob bytes and publishes a matching index on
//! macOS/Linux. Supply an explicit baseline matching the clean index, or `None` for an initial
//! no-checkout clone. The operation refuses staged/unstaged changes and untracked obstructions;
//! HEAD and refs remain unchanged. [`checkout`] documents caller exclusion, supported names,
//! preparation/mutation/publication phases and per-path failure reports. Run
//! `cargo run --example checkout` for a disposable lifecycle example.
//!
//! # Planning object retention
//!
//! [`Repository::plan_retention`] observes caller heads, references, imported reflogs, registered
//! worktree HEADs and indexes, recent loose objects, shallow boundaries, and protected packs.
//! [`retention::RetentionPolicy`] supplies resource limits and expiry cutoffs. Complete reports
//! distinguish every observed reachable object from objects still required after reflog expiry.
//! Incomplete reports retain recovered candidates and cannot justify deletion. A later maintenance
//! executor must exclude writers and rescan before acting; this library operation changes no files.
//! [`Repository::repack_retained`] performs a fresh scan and publishes a bounded pack/index pair
//! without removing existing storage. It cannot authorize pruning while external writers or pinned
//! readers may still depend on old data.
//!
//! # Optional operation tracing
//!
//! Enable the `tracing` feature for categorical operation spans on the `girt` target. DEBUG
//! spans describe workflows and phases; TRACE includes individual object reads and writes.
//! Spans record outcomes and bounded-work counts without formatting arguments or returned errors.
//! The caller owns subscribers, sinks, filtering, runtimes and scheduling; errors remain structured
//! return values. Run `cargo run --features tracing --example tracing` for a scoped subscriber.
//! HTTP/SSH downloads retain the initiating subscriber and span through synchronous worker
//! validation or drop. Their span lifetime therefore includes queue time. See `docs/tracing.md` for
//! coverage, overhead, async context propagation and guidance for extending instrumentation.
//!
//! # Current boundaries
//!
//! Git object and reference operations support SHA-1 and SHA-256. Wire fetch and push use
//! protocol v0 streams; native local transfer uses girt storage without a Git server process.
//! The supported working-tree operations use literal blob bytes and POSIX modes. They do not apply
//! attributes, filters, or EOL conversion. Branch switching, sparse checkout, and submodule
//! operations remain outside this API. Each operation's module and item docs state its narrower
//! accepted formats, resource limits, and failure effects.
pub mod checkout;
pub mod clone;
mod file_policy;
pub use file_policy::SharedPermissions;

mod commit;
pub mod config;
pub mod content_diff;
mod edges;
pub mod fetch;
mod history;
pub mod ignore;
pub mod index;
mod loose;
mod object;
mod objects;
pub mod pack;
mod packet;
mod peel;
pub mod push;
pub mod refs;
pub mod remote;
mod repository;
pub mod retention;
pub mod rewrites;
pub mod status;
mod tag;
pub mod transfer;
pub mod transport;
mod tree;
mod tree_compare;

pub use commit::{
    Commit, CommitError, CommitFields, CommitHeader, CommitHeaderRef, CommitPayload, IdentityDate,
    IdentityRef, Signature,
};
pub use config::{Config, ConfigError};
pub use history::{HistoryError, HistoryLimits};
pub use loose::{Error, LooseObjects};
pub use object::{ObjectFormat, ObjectFormatError, ObjectId, ParseObjectIdError, encode_blob};
pub use objects::{AlternateLimits, Object, ObjectReadError, Objects, PackLimits, ReadLimits};
pub use pack::{
    DeltaOptions, DeltaStats, PackCompression, PackObject, PackWriteError, PackWriteLimits,
    PackWritten, write_pack, write_pack_with_compression,
};
pub use peel::{PeelError, PeelFailure, PeelLimits, PeeledObject};
pub use repository::{
    ColocationEdit, ColocationError, CreateWorktreeError, InitError, InitKind, InitOptions,
    OpenError, OperationCleanupError, OperationError, OperationLimits, OperationState,
    OrphanWorktreeOptions, Repository, RepositoryLocation, RepositoryMetadata, ShallowError,
    ShallowRoots, Worktree, WorktreeAdminError, WorktreeDurability, WorktreeError,
    WorktreeLinkStyle, WorktreeRepair, WorktreeRetirement, WorktreeState,
};
pub use tag::{ObjectKind, Tag, TagError, TagFields};
pub use tree::{EntryMode, Tree, TreeEntry, TreeError};
pub use tree_compare::{TreeChange, TreeCompareError, TreeCompareLimits, TreeValue};

#[cfg(feature = "tracing")]
mod trace;
