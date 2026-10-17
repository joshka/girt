//! Download objects from another repository and publish remote-tracking references.
//!
//! A fetch has three phases, and this module exposes each of them:
//!
//! 1. **Transfer and validate.** Agree with the source on which objects to send, receive them,
//!    check every received object's framing, delta chain and identity, and walk the history of each
//!    selected tip to check that it is complete and well-formed. Nothing is written to the
//!    destination. See [Limits and trust](crate::transport#limits-and-trust) for exactly what is
//!    checked.
//! 2. **Install.** Publish the validated pack, then its index, in the destination's object storage.
//!    An interruption between the two leaves a pack that readers ignore.
//! 3. **Publish.** Update remote-tracking references and tags, each conditional on the value it had
//!    when the fetch began.
//!
//! [`crate::workflow::fetch`] runs all three for a remote named in the repository's
//! configuration. Use this module when the application chooses the endpoint, the limits, the
//! worker threads or the publication policy itself.
//!
//! # Choosing an entry point
//!
//! | Goal | Start with |
//! | --- | --- |
//! | Fetch a configured remote as `git fetch` does | [`crate::workflow::fetch`] |
//! | Update remote-tracking references and tags from an explicit endpoint | [`FetchRequest`], then [`FetchReady::finish`] |
//! | Get objects only, and name or publish them yourself | [`receive_local`], [`receive`] or a network `receive_*` function, then [`ReceivedFetch::install`] |
//! | List a remote's references and `HEAD` without transferring objects | [`discover_local`], `discover_http`, `discover_ssh` or [`discover_session`] |
//! | Keep a fetched pack safe from repacking while you check it | [`FetchReady::install_retained`] or [`ReceivedFetch::install_retained`] |
//!
//! # The `FetchRequest` lifecycle
//!
//! [`FetchRequest::prepare`] captures the destination's references, the refspecs, which forced
//! destinations the caller authorizes, and the reflog policy. Builder methods add pruning
//! ([`FetchRequest::with_prune`]), a depth limit ([`FetchRequest::with_depth`]), tag namespaces
//! and per-namespace reflog policy.
//!
//! A receive adapter then reads the source's advertisement, maps the refspecs over it, transfers
//! and validates. [`FetchRequest::receive_local`] returns a [`FetchReady`] directly.
//! [`FetchRequest::receive_session`] does the same over a caller-owned stream. The `http` and
//! `ssh` features add async `receive_http` and `receive_ssh` adapters, which return a
//! [`FetchDownload`] to validate synchronously into a `FetchReady`.
//!
//! [`FetchReady::finish`] installs the objects, publishes any new shallow boundaries, checks every
//! planned update against its rules, then prunes in one reference transaction and updates in a
//! second. It returns a [`FetchReport`].
//!
//! Destinations must be remote-tracking references, tags, or a tag namespace the caller adds with
//! [`FetchRequest::with_tag_destination_namespace`]. Branches are never fetch destinations, which
//! protects every worktree's checked-out branch. A `+` refspec records force intent, but a
//! non-fast-forward update or a tag replacement also needs the destination named in the
//! request's authorized set. `FETCH_HEAD` and following tags automatically remain caller policy.
//!
//! # Negotiation and known history
//!
//! A [`KnownHistory`] describes what the destination already has, so the source can leave it
//! out. [`KnownHistory::default`] knows nothing and requests full history.
//! [`KnownHistory::from_store`] offers commits reachable from the given tips as haves, up to
//! [`FetchLimits::max_haves`]; [`KnownHistory::new`] reads and verifies a bounded local graph.
//! Objects the received pack depends on, such as external delta bases in a thin pack, are checked
//! against this history, and installation checks them again. Known history describes the
//! destination at one moment: keep garbage collection away from it until the fetch publishes.
//!
//! # Network downloads and worker threads
//!
//! The HTTP and SSH adapters download asynchronously and return owned bytes without validating
//! them. Validation inflates and hashes every object, so move the download to a blocking worker
//! and validate it there. The caller bounds how many downloads wait and how many validations run,
//! and joins each worker even after cancelling it. Downloads can hold shared
//! `Arc<KnownHistory>` ownership so they outlive the scope that started them.
//!
//! Native local receive and pack validation each use up to [`FetchLimits::max_threads`] scoped
//! worker threads, joined before the call returns; one runs everything on the calling thread.
//! Validating a pack of at least 1,024 objects hashes and parses decoded objects on workers, with
//! at most 64 MiB of payloads and parsed tree edges waiting. Native local receive reads queued
//! source objects ahead of its walk, which still applies every check in queue order.
//!
//! # Depth and shallow history
//!
//! [`FetchRequest::with_depth`] fetches a limited number of commits from each tip, like `git fetch
//! --depth`, and publishes the resulting shallow boundaries together with the references. Native
//! local sources support a depth of one for a single branch. The lower-level
//! [`receive_with_known_depth`] and network depth functions report boundaries, but
//! [`ReceivedFetch::install`] refuses a shallow result: only the request workflow publishes them.
//!
//! # Progress
//!
//! [`LocalFetchObserver`] receives a local fetch's source messages, [`LocalFetchProgress`] while
//! the source's objects are read and packed, and [`ValidationProgress`] while they are checked.
//! Network adapters' `*_with_progress` variants deliver the server's progress messages as they
//! arrive; `validate_with_progress` reports validation of a download. Observers run on the
//! transferring thread and must return promptly; progress never implies installation or
//! publication.
//!
//! # Errors
//!
//! | Error | Returned by | Repository state |
//! | --- | --- | --- |
//! | [`FetchWorkflowError`] | `FetchRequest` receive adapters and download validation | unchanged |
//! | [`FetchFinishError`] | [`FetchReady::finish`] | possibly installed objects, shallow boundaries or some references; see its report |
//! | [`FetchError`] | lower-level receive, discovery and install functions | unchanged, except a failed install can leave an unindexed pack |
//! | [`RetainedFetchError`], [`RetainedFetchFinishError`] | retained installation | see each type |
//!
//! [`FetchError`] is the common cause: transport and protocol failures, limits
//! ([`FetchError::Limit`]), cancellation, deadlines, and invalid or disconnected objects.
//!
//! # Protocol support
//!
//! Wants must be advertised reference tips. Transfer requests `side-band-64k` and, when
//! available, `ofs-delta`, `thin-pack`, `multi_ack`, `shallow` and the SHA-256 object-format
//! capability. Thin packs are completed with verified local delta bases and rewritten as
//! self-contained packs before installation. Every adapter accepts protocol v0, v1 and v2 answers;
//! the network endpoints request v0 unless given another
//! [`RequestedProtocol`](crate::transport::RequestedProtocol), and a v2 stream uses one `ls-refs`
//! and one `fetch` command. Both SHA-1 and SHA-256 repositories are supported. Partial-clone
//! filters are not. Discovery reports peeled tag hints separately from selectable tips, and
//! never authorizes a later update.
//!
//! # Concurrency
//!
//! Other writers may update the destination's references during a fetch; publication never
//! overwrites a reference that changed. From [`FetchRequest::prepare`] until publication, the
//! caller must exclude garbage collection or pruning, checkout, changes to `HEAD` or checked-out
//! branches, and worktree registration changes, which reference locks do not prevent. See
//! [`FetchRequest`] for the full contract.
//!
//! # Example: fetching from a local repository
//!
//! Fetch `main` from a local source repository (setup hidden) into `refs/remotes/origin/main`.
//! Objects are installed before the conditional reference update:
//!
//! ```
//! use std::collections::BTreeSet;
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::fetch::{FetchLimits, FetchRequest, FetchUpdateLimits, KnownHistory};
//! use girt::refs::{RefName, Reflog, Target};
//! use girt::transport::TransportControl;
//! use girt::{InitKind, ObjectFormat, Repository};
//! # use girt::refs::Expected;
//! # use girt::{Commit, CommitFields, Signature, Tree};
//! # let directory = tempfile::tempdir()?;
//! # let source_path = directory.path().join("source.git");
//! # let source = Repository::init(ObjectFormat::Sha1, source_path, InitKind::Bare)?;
//! # let person = Signature {
//! #     name: b"A. Writer".to_vec(),
//! #     email: b"writer@example.com".to_vec(),
//! #     seconds: 1_700_000_000,
//! #     offset_minutes: 0,
//! # };
//! # let loose = source.loose_objects();
//! # let commit = loose.write_commit(&Commit::new(CommitFields {
//! #     tree: loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?,
//! #     parents: vec![],
//! #     author: person.clone(),
//! #     committer: person.clone(),
//! #     extra_headers: vec![],
//! #     message: b"Initial commit\n".to_vec(),
//! # })?)?;
//! # let main = RefName::new("refs/heads/main")?;
//! # source.references()?.update_without_reflog(&main, Target::Direct(commit), Expected::Absent)?;
//!
//! let destination =
//!     Repository::init(ObjectFormat::Sha1, directory.path().join("clone.git"), InitKind::Bare)?;
//! let config = girt::Config::parse(
//!     b"[remote \"origin\"]\nfetch = +refs/heads/*:refs/remotes/origin/*\n",
//! )?;
//! let origin = girt::remote::Remote::find(&config, b"origin")?.expect("origin is configured");
//!
//! let request = FetchRequest::prepare(
//!     destination,
//!     origin.fetch_refspecs().clone(),
//!     // `+` records force intent, but girt also requires each forced destination name here.
//!     // An empty set refuses non-fast-forward updates despite the `+`.
//!     BTreeSet::new(),
//!     Reflog::Append { committer: person, message: b"fetch origin".to_vec() },
//! )?;
//! let cancel = AtomicBool::new(false);
//! let ready = request.receive_local(
//!     source.git_dir(),
//!     &KnownHistory::default(),
//!     FetchLimits::default(),
//!     TransportControl::new(&cancel),
//!     girt::fetch::LocalFetchObserver::default(),
//! )?;
//! let report = ready.finish(FetchUpdateLimits::default(), &cancel)?;
//! assert_eq!(report.references.len(), 1);
//!
//! let destination = Repository::open(directory.path().join("clone.git"))?;
//! let tracking = RefName::new("refs/remotes/origin/main")?;
//! assert_eq!(destination.references()?.read(&tracking)?, Some(Target::Direct(commit)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
mod ssh;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub use ssh::{receive_ssh, receive_ssh_with_depth, receive_ssh_with_progress};

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
mod live_progress;

#[cfg(feature = "http")]
mod http;
#[cfg(feature = "http")]
pub use http::{
    receive_http, receive_http_with_depth, receive_http_with_depth_and_progress,
    receive_http_with_progress,
};

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
mod download;
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
pub use download::DownloadedFetch;

#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
mod workflow_network;
#[cfg(any(
    feature = "http",
    all(feature = "ssh", any(target_os = "macos", target_os = "linux"))
))]
pub use workflow_network::FetchDownload;
mod update;
mod verified;
pub use update::{FetchUpdateError, FetchUpdateLimits};
mod workflow;
mod worktree;
pub use workflow::{
    FetchFinishError, FetchFinishFailure, FetchPlanError, FetchReady, FetchReport, FetchRequest,
    FetchUpdate, FetchUpdateKind, FetchWorkflowError, RetainedFetchFinishError, RetainedFetchReady,
};

mod boundaries;
mod bytes;
mod connectivity;
mod discovery;
#[cfg(feature = "http")]
pub use discovery::discover_http;
pub(crate) use discovery::interpret_head;
pub use discovery::{
    ProtocolVersion, RemoteDiscovery, RemoteHead, discover, discover_local, discover_session,
    discover_session_with_prefixes,
};
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub use discovery::{discover_ssh, discover_ssh_with_diagnostics};
mod progress;
pub use progress::{LocalFetchProgress, ValidationProgress};

mod import;
pub(crate) use import::index_self_contained;
mod install;
mod retention;
pub use retention::{FetchRetention, RetainedFetchError};
mod shallow;
pub use shallow::FetchShallowError;
mod known;
mod local;
mod local_native;
mod protocol;
mod v2;

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) use bytes::Bytes;
pub(crate) use install::NativeContents;
#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
pub(crate) use install::PackInput;
pub use install::{FetchInstalled, ReceivedFetch};
pub use known::KnownHistory;
pub use local::receive_local;
mod local_request;
pub use local_request::{LocalFetchObserver, LocalFetchRequest};
pub(crate) use progress::ValidationObserver;
pub use protocol::{
    AdvertisedRef, Advertisement, AdvertisementKind, receive, receive_with_known,
    receive_with_known_depth,
};

/// Resource bounds and optional positive commit depth for one wire fetch.
///
/// `depth` counts commits from each selected tip. `None` requests no depth change. A positive
/// depth can return shallow boundaries; ordinary installation rejects such a result until
/// coordinated boundary publication is available.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct FetchOptions {
    /// Wire, decoding, and graph budgets.
    pub limits: FetchLimits,
    /// Requested commit depth, or no depth change.
    pub depth: Option<NonZeroU32>,
}

impl FetchOptions {
    /// Sets [`Self::limits`].
    #[must_use]
    pub fn with_limits(mut self, limits: FetchLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Sets [`Self::depth`].
    #[must_use]
    pub fn with_depth(mut self, depth: Option<NonZeroU32>) -> Self {
        self.depth = depth;
        self
    }
}

/// Bounds for one advertisement, transfer, import, and connectivity check.
///
/// Use [`FetchLimits::default`] when the remote is not under the user's control, and
/// [`FetchLimits::trusted`] for remotes trusted as Git trusts a user's own. Limits never relax
/// validation; see [Limits and trust](crate::transport#limits-and-trust) for the model. A
/// transfer that exceeds a bound while receiving or validating fails with [`FetchError::Limit`]
/// before anything is installed.
///
/// # Examples
///
/// Tighten the defaults for a service that fetches from user-supplied URLs:
///
/// ```
/// use girt::fetch::FetchLimits;
///
/// let limits = FetchLimits::default()
///     .with_max_pack_bytes(64 * 1024 * 1024)
///     .with_max_objects(200_000)
///     .with_max_object_bytes(16 * 1024 * 1024);
/// assert_eq!(limits.max_delta_depth, 64);
/// ```
///
/// A fetch that needs more than the limits allow fails without changing the repository:
///
/// ```
/// use std::collections::BTreeSet;
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{FetchError, FetchLimits, FetchRequest, FetchWorkflowError, KnownHistory};
/// use girt::refs::Reflog;
/// use girt::remote::Remote;
/// use girt::transport::TransportControl;
/// # let (_directory, upstream, repo, _, _) = include!("../doctest/remote.rs");
///
/// // `upstream`'s `main` is a commit and a tree: two objects.
/// let origin = Remote::find(repo.config(), b"origin")?.expect("configured");
/// let request = FetchRequest::prepare(
///     repo.clone(),
///     origin.fetch_refspecs().clone(),
///     BTreeSet::new(),
///     Reflog::Preserve,
/// )?;
/// let cancel = AtomicBool::new(false);
/// let result = request.receive_local(
///     upstream.git_dir(),
///     &KnownHistory::default(),
///     FetchLimits::default().with_max_objects(1),
///     TransportControl::new(&cancel),
///     girt::fetch::LocalFetchObserver::default(),
/// );
/// assert!(matches!(
///     result,
///     Err(FetchWorkflowError::Transfer(FetchError::Limit(_)))
/// ));
/// assert!(repo.references()?.list()?.is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// Wire transfers spool the response and pack to temporary files, each bounded by `max_wire_bytes`
/// and `max_pack_bytes` respectively. Decoded delta bases use a 96 MiB cache; live reconstruction
/// buffers are bounded by `max_decode_bytes` and per-object limits. Protocol prefixes and
/// advertisements are bounded by `max_shallow_roots` and `max_advertisement_bytes`. Entry/graph
/// metadata is
/// O(`max_objects` + `max_connectivity_edges`), plus separately retained [`KnownHistory`] payloads
/// bounded by `max_known_bytes` and metadata bounded by `max_known_objects`. Index generation needs
/// at most 48 bytes per object plus 1096 bytes. Native local construction spools pack bytes and
/// retains graph/index metadata. Graph parsing can temporarily copy a structured payload and its
/// fields. These input/work
/// bounds exclude allocator overhead, stream-owned buffers, fixed zlib scratch space, and server
/// memory; they are not a hard process heap or wall-clock limit. Zero bounds allow only the
/// corresponding empty operation. Counters include duplicate input occurrences where applicable.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct FetchLimits {
    /// Total received pkt-line bytes, framing included (default 512 MiB).
    pub max_wire_bytes: usize,
    /// Advertisement bytes including framing (default 4 MiB).
    pub max_advertisement_bytes: usize,
    /// Advertised entries, including peeled hints (default 100,000).
    pub max_refs: usize,
    /// Want occurrences accepted from the selection callback (default 100,000).
    pub max_wants: usize,
    /// Maximum have candidates retained (default 256). The single wire batch sends at most 32
    /// with multi-ACK, or one without it.
    /// Additional verified commits are retained for connectivity but not offered as haves.
    pub max_haves: usize,
    /// Shallow boundary IDs retained and sent in one negotiation (default 100,000).
    pub max_shallow_roots: usize,
    /// Objects retained while verifying local history (default one million).
    pub max_known_objects: usize,
    /// Aggregate local payload bytes retained (default 256 MiB).
    pub max_known_bytes: usize,
    /// Local history edge occurrences (default 4 million).
    pub max_known_edges: usize,
    /// Per-read decoding limits for local verification and installation rechecks.
    /// Total decoding work is bounded by this budget times `max_known_objects`.
    pub known_read: crate::ReadLimits,
    /// Received pack bytes, including header and trailer (default 256 MiB).
    pub max_pack_bytes: usize,
    /// Objects in the pack (default one million). Native selection also bounds distinct examined
    /// source IDs, including known boundaries; their descendants are skipped.
    pub max_objects: usize,
    /// Bytes in each decoded object (default 64 MiB).
    pub max_object_bytes: usize,
    /// Bytes in each inflated delta program (default 64 MiB).
    pub max_delta_bytes: usize,
    /// Aggregate inflated programs, ordinary payloads, delta results and appended external bases
    /// (default 512 MiB). Includes repeated decoding after cache eviction and connectivity reads.
    /// Native source selection and writing each apply a separate allowance of this size to the
    /// sum of selected logical payload lengths, including copied entries. Known boundaries are
    /// excluded; avoiding blob inflation does not exempt their declared result sizes.
    pub max_decode_bytes: usize,
    /// Delta edges from any object to an ordinary base (default 64).
    pub max_delta_depth: usize,
    /// Entry/edge visits and external-base probes during queued base resolution (default 10
    /// million).
    pub max_resolution_steps: usize,
    /// Reachable edge occurrences, including duplicates (default 4 million).
    pub max_connectivity_edges: usize,
    /// Worker threads one transfer may add for reading and validation (default 8). Zero or one
    /// runs everything on the calling thread. Fewer are used when the host has fewer.
    pub max_threads: usize,
    /// Bounds for opening a native local source repository's object store (default
    /// [`PackLimits::default`](crate::PackLimits::default), which admits 64 MiB of pack index).
    ///
    /// Git-scale sources exceed the default: a Linux kernel clone has a 300 MB index. Use
    /// [`PackLimits::trusted`](crate::PackLimits::trusted), as [`Self::trusted`] does, for
    /// repositories the caller trusts. Network fetches open no source store and ignore this.
    pub source_packs: crate::PackLimits,
}

impl FetchLimits {
    /// Sets [`Self::max_wire_bytes`].
    #[must_use]
    pub fn with_max_wire_bytes(mut self, max_wire_bytes: usize) -> Self {
        self.max_wire_bytes = max_wire_bytes;
        self
    }

    /// Sets [`Self::max_advertisement_bytes`].
    #[must_use]
    pub fn with_max_advertisement_bytes(mut self, max_advertisement_bytes: usize) -> Self {
        self.max_advertisement_bytes = max_advertisement_bytes;
        self
    }

    /// Sets [`Self::max_refs`].
    #[must_use]
    pub fn with_max_refs(mut self, max_refs: usize) -> Self {
        self.max_refs = max_refs;
        self
    }

    /// Sets [`Self::max_wants`].
    #[must_use]
    pub fn with_max_wants(mut self, max_wants: usize) -> Self {
        self.max_wants = max_wants;
        self
    }

    /// Sets [`Self::max_haves`].
    #[must_use]
    pub fn with_max_haves(mut self, max_haves: usize) -> Self {
        self.max_haves = max_haves;
        self
    }

    /// Sets [`Self::max_shallow_roots`].
    #[must_use]
    pub fn with_max_shallow_roots(mut self, max_shallow_roots: usize) -> Self {
        self.max_shallow_roots = max_shallow_roots;
        self
    }

    /// Sets [`Self::max_known_objects`].
    #[must_use]
    pub fn with_max_known_objects(mut self, max_known_objects: usize) -> Self {
        self.max_known_objects = max_known_objects;
        self
    }

    /// Sets [`Self::max_known_bytes`].
    #[must_use]
    pub fn with_max_known_bytes(mut self, max_known_bytes: usize) -> Self {
        self.max_known_bytes = max_known_bytes;
        self
    }

    /// Sets [`Self::max_known_edges`].
    #[must_use]
    pub fn with_max_known_edges(mut self, max_known_edges: usize) -> Self {
        self.max_known_edges = max_known_edges;
        self
    }

    /// Sets [`Self::known_read`].
    #[must_use]
    pub fn with_known_read(mut self, known_read: crate::ReadLimits) -> Self {
        self.known_read = known_read;
        self
    }

    /// Sets [`Self::max_pack_bytes`].
    #[must_use]
    pub fn with_max_pack_bytes(mut self, max_pack_bytes: usize) -> Self {
        self.max_pack_bytes = max_pack_bytes;
        self
    }

    /// Sets [`Self::max_objects`].
    #[must_use]
    pub fn with_max_objects(mut self, max_objects: usize) -> Self {
        self.max_objects = max_objects;
        self
    }

    /// Sets [`Self::max_object_bytes`].
    #[must_use]
    pub fn with_max_object_bytes(mut self, max_object_bytes: usize) -> Self {
        self.max_object_bytes = max_object_bytes;
        self
    }

    /// Sets [`Self::max_delta_bytes`].
    #[must_use]
    pub fn with_max_delta_bytes(mut self, max_delta_bytes: usize) -> Self {
        self.max_delta_bytes = max_delta_bytes;
        self
    }

    /// Sets [`Self::max_decode_bytes`].
    #[must_use]
    pub fn with_max_decode_bytes(mut self, max_decode_bytes: usize) -> Self {
        self.max_decode_bytes = max_decode_bytes;
        self
    }

    /// Sets [`Self::max_delta_depth`].
    #[must_use]
    pub fn with_max_delta_depth(mut self, max_delta_depth: usize) -> Self {
        self.max_delta_depth = max_delta_depth;
        self
    }

    /// Sets [`Self::max_resolution_steps`].
    #[must_use]
    pub fn with_max_resolution_steps(mut self, max_resolution_steps: usize) -> Self {
        self.max_resolution_steps = max_resolution_steps;
        self
    }

    /// Sets [`Self::max_connectivity_edges`].
    #[must_use]
    pub fn with_max_connectivity_edges(mut self, max_connectivity_edges: usize) -> Self {
        self.max_connectivity_edges = max_connectivity_edges;
        self
    }

    /// Sets [`Self::max_threads`].
    #[must_use]
    pub fn with_max_threads(mut self, max_threads: usize) -> Self {
        self.max_threads = max_threads;
        self
    }

    /// Sets [`Self::source_packs`].
    #[must_use]
    pub fn with_source_packs(mut self, source_packs: crate::PackLimits) -> Self {
        self.source_packs = source_packs;
        self
    }
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            max_wire_bytes: 512 * 1024 * 1024,
            max_advertisement_bytes: 4 * 1024 * 1024,
            max_refs: 100_000,
            max_wants: 100_000,
            max_haves: 256,
            max_shallow_roots: 100_000,
            max_known_objects: 1_000_000,
            max_known_bytes: 256 * 1024 * 1024,
            max_known_edges: 4_000_000,
            known_read: crate::ReadLimits::default(),
            max_pack_bytes: 256 * 1024 * 1024,
            max_objects: 1_000_000,
            max_object_bytes: 64 * 1024 * 1024,
            max_delta_bytes: 64 * 1024 * 1024,
            max_decode_bytes: 512 * 1024 * 1024,
            max_delta_depth: 64,
            max_resolution_steps: 10_000_000,
            max_connectivity_edges: 4_000_000,
            max_threads: 8,
            source_packs: crate::PackLimits::default(),
        }
    }
}

impl FetchLimits {
    /// Returns limits for a transfer the caller trusts as Git trusts the user's own remotes.
    ///
    /// [`crate::workflow`] uses these limits. Choose them when the user picked the remote, or when
    /// opening a large local source repository you control: the default
    /// [`Self::source_packs`] refuses Git-scale repositories.
    ///
    /// Git bounds none of these values. The defaults suit untrusted input; these are large enough
    /// that only available memory bounds the work, while keeping headroom for internal arithmetic.
    ///
    /// Wire pack payloads stay on disk, with a bounded delta-base cache. Metadata and individual
    /// object reconstruction still grow with the input. `max_haves` keeps its default: it bounds
    /// negotiation, not what can be fetched.
    pub const fn trusted() -> Self {
        const UNBOUNDED: usize = usize::MAX / 4;
        Self {
            max_wire_bytes: UNBOUNDED,
            max_advertisement_bytes: UNBOUNDED,
            max_refs: UNBOUNDED,
            max_wants: UNBOUNDED,
            max_haves: 256,
            max_shallow_roots: UNBOUNDED,
            max_known_objects: UNBOUNDED,
            max_known_bytes: UNBOUNDED,
            max_known_edges: UNBOUNDED,
            known_read: crate::ReadLimits::trusted(),
            max_pack_bytes: UNBOUNDED,
            max_objects: UNBOUNDED,
            max_object_bytes: UNBOUNDED,
            max_delta_bytes: UNBOUNDED,
            max_decode_bytes: UNBOUNDED,
            max_delta_depth: 10_000,
            max_resolution_steps: UNBOUNDED,
            max_connectivity_edges: UNBOUNDED,
            max_threads: 8,
            source_packs: crate::PackLimits::trusted(),
        }
    }
}

/// Transfer, validation, or installation failed. No references have been changed.
///
/// Inside [`FetchWorkflowError::Transfer`] these errors also mean nothing was installed. From
/// [`ReceivedFetch::install`], or inside a [`FetchFinishError`], installation may have been
/// partial; see those types.
///
/// # Examples
///
/// A local source path that is not a repository is reported as [`FetchError::Source`], which jj
/// shows as "no such repository":
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::fetch::{FetchError, FetchLimits, discover_local};
/// use girt::transport::TransportControl;
///
/// let directory = tempfile::tempdir()?;
/// let cancel = AtomicBool::new(false);
/// let result = discover_local(
///     directory.path().join("missing.git"),
///     FetchLimits::default(),
///     TransportControl::new(&cancel),
/// );
/// assert!(matches!(result, Err(FetchError::Source(_))));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FetchError {
    /// A supplied identity uses a different object format from this operation.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// Sanitized OpenSSH transport or service failure.
    #[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
    #[error(transparent)]
    Ssh(crate::transport::ssh::SshError),
    /// Sanitized smart HTTP exchange failure.
    #[cfg(feature = "http")]
    #[error(transparent)]
    Http(crate::transport::http::HttpError),
    /// Stream or filesystem failure; protocol I/O propagates interruption without retrying.
    #[error("fetch I/O")]
    Io(#[source] std::io::Error),
    /// Invalid or unexpected protocol framing or state.
    #[error("invalid upload-pack response: {0}")]
    Protocol(&'static str),
    /// A protocol feature needed by the peer is outside this slice.
    #[error("unsupported upload-pack feature: {0}")]
    Unsupported(&'static str),
    /// Peer ERR or fatal sideband, retained as bytes without assuming UTF-8.
    #[error("upload-pack reported an error: {0:?}")]
    Remote(Vec<u8>),
    /// An explicit want was not a non-peeled advertised reference tip.
    #[error("unadvertised want {0}")]
    Unadvertised(ObjectId),
    /// A resource budget was exhausted.
    #[error("fetch limit exceeded: {0}")]
    Limit(&'static str),
    /// The cancellation flag was set or the progress callback requested cancellation.
    #[error("fetch cancelled")]
    Cancelled,
    /// The owned transport reached its caller-supplied deadline.
    #[error("transport deadline expired")]
    Deadline,
    /// Pack framing, checksum, delta reconstruction, or storage validation failed.
    #[error("received pack")]
    Pack(#[from] crate::ObjectReadError),
    /// Native local pack construction failed before destination publication.
    #[error("local pack construction")]
    PackWrite(#[source] crate::PackWriteError),
    /// Local annotated-tag advertisement could not be verified.
    #[error("local tag peeling")]
    Peel(#[source] Box<crate::PeelError>),
    /// Reopening the destination failed before dependency checks or publication.
    #[error("destination object snapshot")]
    Destination(#[source] crate::ObjectReadError),
    /// The remote and local repositories use different object formats.
    #[error("the remote repository uses {remote} object IDs but this repository uses {local}")]
    FormatMismatch {
        /// Remote object format.
        remote: crate::ObjectFormat,
        /// Local object format.
        local: crate::ObjectFormat,
    },
    /// The local source repository could not be opened.
    #[error("cannot open source repository")]
    Source(#[source] Box<crate::OpenError>),
    /// The local source repository's object store could not be opened within
    /// [`FetchLimits::source_packs`].
    #[error("opening source object store")]
    SourceObjects(#[source] crate::ObjectReadError),
    /// A local dependency failed during history preparation or installation rechecking.
    #[error("local fetch dependency {id}")]
    LocalRead {
        /// Local object being read.
        id: ObjectId,
        /// Concrete storage failure.
        #[source]
        source: crate::ObjectReadError,
    },
    /// Received index encoding or thin-pack completion failed before installation.
    #[error("received pack/index construction")]
    Index(#[from] crate::PackWriteError),
    /// A selected tip or reachable object is absent from both received and verified local objects.
    #[error("missing reachable object {0}")]
    Missing(ObjectId),
    /// A reachable reference has the wrong logical object type.
    #[error("reachable object {0} has the wrong kind")]
    Kind(ObjectId),
    /// Reachable commit syntax is unsupported or invalid.
    #[error("reachable commit {id}")]
    Commit {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::CommitError,
    },
    /// Reachable tree syntax or entries are invalid.
    #[error("reachable tree {id}")]
    Tree {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::TreeError,
    },
    /// Reachable tag syntax is unsupported or invalid.
    #[error("reachable tag {id}")]
    Tag {
        /// Object whose payload failed validation.
        id: ObjectId,
        /// Concrete payload validation failure.
        #[source]
        source: crate::TagError,
    },
    /// A local upload-pack exited unsuccessfully after its protocol response.
    #[error("local upload-pack exited unsuccessfully: {0}")]
    Process(std::process::ExitStatus),
    /// A live object artifact with the same name contains different bytes.
    #[error("existing pack artifact differs: {0}")]
    Existing(std::path::PathBuf),
}

use crate::ObjectId;

pub(crate) fn check_cancelled(cancel: &AtomicBool) -> Result<(), FetchError> {
    if cancel.load(Ordering::Relaxed) {
        Err(FetchError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;

impl From<crate::packet::Error> for FetchError {
    fn from(error: crate::packet::Error) -> Self {
        match error {
            crate::packet::Error::Io(e) => Self::from(e),
            crate::packet::Error::Protocol(e) => Self::Protocol(e),
            crate::packet::Error::Limit(e) => Self::Limit(e),
            crate::packet::Error::Cancelled => Self::Cancelled,
            crate::packet::Error::Remote(e) => Self::Remote(e),
        }
    }
}

impl From<std::io::Error> for FetchError {
    fn from(error: std::io::Error) -> Self {
        match crate::transport::interruption(&error) {
            Some(crate::transport::Interruption::Cancelled) => Self::Cancelled,
            Some(crate::transport::Interruption::Deadline) => Self::Deadline,
            None => Self::Io(error),
        }
    }
}

#[cfg(feature = "http")]
impl From<crate::transport::http::HttpError> for FetchError {
    fn from(error: crate::transport::http::HttpError) -> Self {
        match error {
            crate::transport::http::HttpError::Cancelled => Self::Cancelled,
            crate::transport::http::HttpError::Deadline => Self::Deadline,
            error => Self::Http(error),
        }
    }
}

#[cfg(all(feature = "ssh", any(target_os = "macos", target_os = "linux")))]
impl From<crate::transport::ssh::SshError> for FetchError {
    fn from(error: crate::transport::ssh::SshError) -> Self {
        match error {
            crate::transport::ssh::SshError::Cancelled => Self::Cancelled,
            crate::transport::ssh::SshError::Deadline => Self::Deadline,
            error => Self::Ssh(error),
        }
    }
}

impl From<crate::edges::Error> for FetchError {
    fn from(error: crate::edges::Error) -> Self {
        match error {
            crate::edges::Error::Commit { id, source } => Self::Commit { id, source },
            crate::edges::Error::Tree { id, source } => Self::Tree { id, source },
            crate::edges::Error::Tag { id, source } => Self::Tag { id, source },
        }
    }
}
