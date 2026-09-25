//! An incremental Rust library for Git's data formats and storage.
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
//! reflogs and working-tree index v2 use the repository format. Transport negotiation remains
//! SHA-1-only and refuses SHA-256 operations before mutation.
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
//! # Reading and replacing the index
//!
//! [`index::Index`] parses and encodes bounded SHA-1/SHA-256 v2 indexes with byte paths, stat words
//! and conflict stages. [`Repository::read_index`] distinguishes missing storage from an empty
//! index; [`Repository::edit_index`] holds `index.lock` while the caller derives and publishes
//! changes. Optional extensions round-trip, but edits reject extensions other than the
//! invalidatable `TREE` cache. Index operations never create working files or apply staging policy.
//! Run `cargo run --example index` for a disposable repository example.
//!
//! # Observing working-tree status
//!
//! [`Repository::raw_status`] separates staged tree/index changes, raw index/worktree changes,
//! conflicts and unchecked gitlinks. It verifies content instead of trusting cached stat data,
//! applies no normalization or ignores, and never refreshes the index. macOS/Linux traversal
//! avoids symlink ancestors and repository metadata. Reports are observations, not atomic
//! snapshots or checkout preconditions. Run `cargo run --example status` for a disposable fixture.
//!
//! # Creating and finding a repository
//!
//! [`Repository::init`] creates a bare or ordinary SHA-1 or SHA-256 repository with unborn `main`
//! and refuses reinitialization. [`Repository::discover`] searches physical ancestors from an
//! existing directory; [`Repository::discover_with_ceiling`] bounds that search to an inclusive
//! ancestor. Discovery stops at malformed or unsupported metadata rather than selecting an outer
//! repository.
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
//! `FETCH_HEAD`, pruning and implicit tag following are deferred; this is not full CLI fetch.
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
//! # Library contents
//!
//! - [`Repository`], [`OpenError`], [`InitKind`], and [`InitError`]: opening, upward discovery, and
//!   initialization of bare or ordinary SHA-1 or SHA-256 repositories.
//! - [`refs`]: validated reference names, loose/packed enumeration and reads, symbolic resolution,
//!   conditional transactions with explicit reflogs, and single-reference operations without
//!   reflogs.
//! - [`Config`] and [`ConfigError`]: byte-oriented parsing of one configuration source.
//! - [`remote`]: named raw remote URLs and pure, direction-aware refspec mapping.
//! - [`Objects`], [`Object`], [`PackLimits`], and [`ReadLimits`]: bounded loose/packed reads.
//! - [`clone`]: bare and ordinary no-checkout creation with persistent origin configuration.
//! - [`fetch`]: upload-pack v0, validated object installation, and conditional fetch publication.
//! - [`push`]: bounded graph selection and conditional receive-pack v0 branch/tag publication.
//! - [`transport`]: owned transport cancellation, deadlines, and process lifetime contracts.
//! - [`write_pack`]: bounded pack/index v2 artifact generation from explicit objects.
//! - [`ObjectReadError`]: packed storage corruption, unsupported formats, and resource failures.
//! - [`HistoryLimits`] and [`HistoryError`]: bounded walks, ancestry queries, and merge bases.
//! - [`ObjectFormat`]: recognized Git object hash formats.
//! - [`ObjectId`]: SHA-1/SHA-256 identity, hashing blob bytes, and hexadecimal parsing.
//! - [`TreeChange`], [`TreeValue`], [`TreeCompareLimits`], and [`TreeCompareError`]: recursive
//!   structural tree comparison.
//! - [`content_diff`]: bounded byte-preserving line edits and separate tree-change blob loading.
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
//! The current API is experimental and supports SHA-1/SHA-256 loose objects and SHA-1/SHA-256
//! pack/index v2 reads, caller-owned pack/index v2 exports, object transfer and fetch
//! orchestration, and conditional branch/tag push. Fetch and push remain SHA-1-only and accept v0
//! streams or local Git server adapters. The optional `http` feature adds async smart-HTTP(S)
//! adapters; `ssh` adds system OpenSSH adapters on macOS/Linux. Both use a caller-owned Tokio
//! runtime; fetch pack validation remains an explicit synchronous step. Files references support
//! enumeration, reads, symbolic resolution, and explicit no-reflog updates and deletion, plus
//! conditional batches and caller-controlled reflog appends. SHA-1/SHA-256 working-tree index v2,
//! raw status and conservative raw tree checkout are available. Attribute/filter/EOL conversion,
//! branch switching, sparse checkout and submodules are deferred.

pub mod checkout;
pub mod clone;
mod commit;
pub mod config;
pub mod content_diff;
mod edges;
pub mod fetch;
mod history;
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
pub mod status;
mod tag;
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
pub use objects::{Object, ObjectReadError, Objects, PackLimits, ReadLimits};
pub use pack::{
    DeltaOptions, DeltaStats, PackCompression, PackObject, PackWriteError, PackWriteLimits,
    PackWritten, write_pack, write_pack_with_compression,
};
pub use peel::{PeelError, PeelFailure, PeelLimits, PeeledObject};
pub use repository::{InitError, InitKind, OpenError, Repository};
pub use tag::{ObjectKind, Tag, TagError, TagFields};
pub use tree::{EntryMode, Tree, TreeEntry, TreeError};
pub use tree_compare::{TreeChange, TreeCompareError, TreeCompareLimits, TreeValue};

#[cfg(feature = "tracing")]
mod trace;
