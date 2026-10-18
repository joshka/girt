//! Verified reads across loose objects, indexed packs, and alternate stores.
//!
//! Open [`Objects`] through [`crate::Repository::objects`] and retain it while reading many
//! objects. It owns a snapshot of opened pack artifacts; loose reads observe current files. A pack
//! snapshot survives repacking, but new packs require [`Objects::refresh`] or a new reader. Refresh
//! does not refresh configuration or shallow boundaries. Choose [`PackLimits`] for opening and
//! [`ReadLimits`] for individual decoding work. These bounds are not a process-wide memory quota.
//!
//! Repository readers trust structurally validated commit-graph metadata for history queries by
//! default, honoring `core.commitGraph=false`. Missing or unusable graphs fall back to object
//! reads. Use [`crate::commit_graph::CommitGraph::verify`] for explicit object verification, or
//! [`Objects::without_commit_graph`] to read commits during each history query. Typed reads always
//! verify objects. Storage refresh retains the attached graph snapshot.
//!
//! # Default and explicit limits
//!
//! The short names use the `Default` limits and never cancel: [`crate::Repository::objects`],
//! [`Objects::read`], and the typed reads below. Each has a `_with` form that takes the limits,
//! such as [`Objects::read_with`]. The short form is the `_with` call with default arguments, so
//! it is as safe for untrusted repositories. Use a `_with` form to change a bound, and the
//! `_controlled` forms to cancel a read.
//!
//! # Typed reads
//!
//! [`Objects::read_commit`], [`Objects::read_tree`], [`Objects::read_tag`] and
//! [`Objects::read_blob`] check the kind and parse the payload, and treat absence as an error.
//! Use them for identities that came from a reference, a commit or a tree entry:
//!
//! ```
//! use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r.git"),
//!     InitKind::Bare,
//! )?;
//!
//! let loose = repo.loose_objects();
//! let blob = loose.write_blob(b"notes\n")?;
//! let entry = TreeEntry {
//!     name: b"notes.txt".to_vec(),
//!     mode: EntryMode::Blob,
//!     id: blob,
//! };
//! let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![entry])?)?;
//!
//! let objects = repo.objects()?;
//! let tree = objects.read_tree(tree)?;
//! let entry = &tree.entries()[0];
//! assert_eq!(objects.read_blob(entry.id)?, b"notes\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Read any object with an explicit absence branch
//!
//! ```
//! use girt::{InitKind, ObjectFormat, ObjectKind, PackLimits, ReadLimits, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r.git"),
//!     InitKind::Bare,
//! )?;
//!
//! let id = repo.loose_objects().write_blob(b"notes\n")?;
//! let objects = repo.objects_with(PackLimits::default())?;
//! let object = objects
//!     .read_with(id, ReadLimits::default())?
//!     .ok_or("missing blob")?;
//!
//! assert_eq!(object.kind(), ObjectKind::Blob);
//! assert_eq!(object.data(), b"notes\n");
//! assert_eq!(object.id(), id);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! [`Object`] owns verified, uncompressed bytes. Storage verification establishes kind, framing,
//! and identity, not payload syntax or validity of referenced objects. Check the kind before using
//! [`crate::Tree::parse`], [`crate::Commit::parse`], or [`crate::Tag::parse`], or use a typed read
//! that does both. Blob bytes need no codec. Packed payloads can share cache storage; borrowing
//! [`Object::data`] avoids copying it.
//!
//! # Enumerate stored objects
//!
//! [`Objects::stores`] yields the primary store and then its alternates. Each [`ObjectStore`]
//! lists its pinned [`IndexedPack`]s, whose [`IndexedPack::ids`] read only the in-memory index,
//! and scans live loose files with [`ObjectStore::loose_objects_controlled`].
//! [`Objects::object_ids`] collects every visible identity once, and
//! [`Objects::headers_controlled`] reads kinds and sizes without decoding payloads.
//!
//! # Missing, limited, and corrupt data
//!
//! [`Objects::read`] returns `Ok(None)` for absence. It never fetches promised objects
//! automatically. [`ObjectReadError`] distinguishes corruption and I/O from exhausted opening or
//! decoding limits. Increase a budget only when the input and available resources justify it. A
//! corrupt artifact needs inspection or replacement from a trusted source. Missing delta bases are
//! unsupported thin or cross-pack dependencies; ordinary complete pack/index pairs are required.
//! Multi-pack and bitmap indexes do not replace those pairs.
//!
//! Index files mark published packs; unindexed packs are ignored and an index without a pack fails
//! opening. Use validated installation APIs for shared storage rather than writing final filenames.
//! Alternates are borrowed storage: reading them does not give the caller ownership for pruning.
//! See [`crate::pack`] for artifact production and [`crate::history`] for ancestry queries.

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
mod alternates;
mod header;
mod listing;
mod prefix;
mod typed;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::{fs, io};

pub use alternates::AlternateLimits;
pub use header::ObjectHeader;
pub use listing::{IndexedPack, ListLimits, LooseIds, LooseObject, ObjectStore, PackIds};
pub(crate) use prefix::LooseListing;
pub use prefix::ResolvePrefixError;
pub use typed::{ReadBlobError, ReadCommitError, ReadTagError, ReadTreeError};

use crate::pack::FilePack;
use crate::pack::reader::check_cancelled;
use crate::{LooseObjects, ObjectFormat, ObjectId, ObjectKind};

/// An owned, identity-verified object kind and exact uncompressed payload.
///
/// Storage framing is validated; tree, commit, and tag payload syntax is not. Pass [`Self::data`]
/// to [`crate::Tree::parse`], [`crate::Commit::parse`], or [`crate::Tag::parse`] as appropriate.
///
/// A packed payload may be shared with the reader's delta base cache, and clones share it too, so
/// borrowing it through [`Self::data`] costs no copy. [`Self::into_data`] copies it only while
/// another `Object` or the cache shares it.
///
/// ```
/// use girt::{InitKind, ObjectFormat, PackLimits, ReadLimits, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
///
/// let id = repo.loose_objects().write_blob(b"payload\n")?;
/// let objects = repo.objects_with(PackLimits::default())?;
/// let object = objects
///     .read_with(id, ReadLimits::default())?
///     .ok_or("missing blob")?;
///
/// assert_eq!(object.data(), b"payload\n");
/// assert_eq!(object.id(), id);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Object {
    pub(crate) kind: ObjectKind,
    pub(crate) format: ObjectFormat,
    pub(crate) data: Arc<Vec<u8>>,
}

impl Object {
    /// The storage format in which the identity was verified.
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }

    /// The verified Git object type.
    pub fn kind(&self) -> ObjectKind {
        self.kind
    }

    /// Exact uncompressed payload, without the Git object header.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Takes ownership of the payload, copying it if another `Object` or the cache shares it.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, PackLimits, ReadLimits, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"payload")?;
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// let object = objects
    ///     .read_with(id, ReadLimits::default())?
    ///     .ok_or("blob absent")?;
    ///
    /// let retained = object.clone();
    /// let owned = object.into_data(); // A shared payload may be copied; the other owner stays usable.
    /// assert_eq!(owned, retained.data());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn into_data(self) -> Vec<u8> {
        Arc::unwrap_or_clone(self.data)
    }

    /// Hashes the kind, canonical header, and payload.
    pub fn id(&self) -> ObjectId {
        self.format.hash_object(self.kind, &self.data)
    }
}

/// An identity and, unless absent, its kind and payload size from [`Objects::locate_all`].
pub(crate) type Located = (ObjectId, Option<(ObjectKind, u64)>);

/// A structurally decoded payload whose identity was not hashed, for internal graph walks only.
///
/// Reading one checks everything [`Objects::read`] checks except that the payload hashes to the
/// requested identity: entry framing, v2 CRCs, zlib termination, declared sizes, delta programs
/// and [`ReadLimits`]. A walk may follow its edges to select objects. It cannot become an
/// [`Object`], so its content never reaches a caller, and it must not authorize deleting or
/// rewriting anything. See `docs/storage-safety.md`.
#[derive(Debug)]
pub(crate) struct UnverifiedObject {
    pub(crate) kind: ObjectKind,
    pub(crate) data: Arc<Vec<u8>>,
}

impl UnverifiedObject {
    pub(crate) fn kind(&self) -> ObjectKind {
        self.kind
    }

    pub(crate) fn data(&self) -> &[u8] {
        &self.data
    }
}

/// Bounds the aggregate pack snapshot across primary and borrowed stores retained by
/// [`crate::Repository::objects`].
///
/// Artifact bytes bound explicit verification work, not retained pack memory. Index input and
/// derived tables have a separate bound. These are per-reader bounds, not a process RSS quota. Zero
/// values permit no indexed packs. Unindexed files are ignored.
///
/// ```
/// use girt::PackLimits;
/// // Bound index work separately from pack artifact bytes for a small-repository tool.
/// let limits = PackLimits::default()
///     .with_max_index_bytes(8 * 1024 * 1024)
///     .with_max_open_files(32);
/// assert_eq!(limits.max_open_files, 32);
/// ```
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct PackLimits {
    /// Maximum sum of pinned `.idx` and `.pack` artifact sizes (default 64 GiB on 64-bit).
    pub max_bytes: usize,
    /// Maximum aggregate index input bytes (default 64 MiB).
    ///
    /// Opening parses and retains bounded index bytes. Input and derived allocations together
    /// are bounded by four times this value, excluding allocator overhead and path storage.
    pub max_index_bytes: usize,
    /// Maximum retained file handles (default 512); each pair needs two.
    pub max_open_files: usize,
    /// Maximum directory entries examined across all pack directories (default 1,000,000).
    pub max_directory_entries: usize,
    /// Maximum number of index/pack pairs (default 256).
    pub max_packs: usize,
}

impl PackLimits {
    /// Sets [`Self::max_bytes`].
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Sets [`Self::max_index_bytes`].
    #[must_use]
    pub fn with_max_index_bytes(mut self, max_index_bytes: usize) -> Self {
        self.max_index_bytes = max_index_bytes;
        self
    }

    /// Sets [`Self::max_open_files`].
    #[must_use]
    pub fn with_max_open_files(mut self, max_open_files: usize) -> Self {
        self.max_open_files = max_open_files;
        self
    }

    /// Sets [`Self::max_directory_entries`].
    #[must_use]
    pub fn with_max_directory_entries(mut self, max_directory_entries: usize) -> Self {
        self.max_directory_entries = max_directory_entries;
        self
    }

    /// Sets [`Self::max_packs`].
    #[must_use]
    pub fn with_max_packs(mut self, max_packs: usize) -> Self {
        self.max_packs = max_packs;
        self
    }
}

impl Default for PackLimits {
    fn default() -> Self {
        Self {
            max_bytes: usize::try_from(64_u64 * 1024 * 1024 * 1024).unwrap_or(usize::MAX),
            max_index_bytes: 64 * 1024 * 1024,
            max_open_files: 512,
            max_directory_entries: 1_000_000,
            max_packs: 256,
        }
    }
}

impl PackLimits {
    /// Returns generous finite bounds for caller-approved local storage.
    ///
    /// Use this only after deciding that the repository's size and cost are acceptable. It permits
    /// up to 100,000 packs and 4,096 retained file handles; byte and directory bounds retain
    /// arithmetic headroom. Operating-system limits and allocation failures can occur first. Prefer
    /// selected default-bound overrides when an application needs a predictable resource policy.
    ///
    /// ```
    /// use girt::PackLimits;
    /// let limits = PackLimits::trusted().with_max_open_files(64);
    /// assert_eq!(limits.max_open_files, 64);
    /// ```
    pub const fn trusted() -> Self {
        Self {
            max_bytes: usize::MAX / 4,
            max_index_bytes: usize::MAX / 8,
            max_open_files: 4096,
            max_directory_entries: usize::MAX / 4,
            max_packs: 100_000,
        }
    }
}

/// Per-read bounds for loose payloads and iterative pack reconstruction.
///
/// Every base and reconstructed payload must fit `max_object_bytes`. Delta programs have their
/// own limit. `max_decode_bytes` charges every inflated program/base and every reconstructed
/// result, including intermediate results, before allocating its buffer. It therefore also bounds
/// retained decoding bytes, but excludes index tables, allocator overhead, traversal bookkeeping,
/// and fixed zlib scratch space. Loose reads use the smaller of object and decode limits, plus a
/// small framing allowance. A delta base served from a pack's base cache is not charged again.
///
/// Customize selected bounds while retaining defaults for future fields:
///
/// ```
/// use girt::ReadLimits;
///
/// let limits = ReadLimits::default()
///     .with_max_object_bytes(1024 * 1024)
///     .with_max_delta_depth(16);
/// assert_eq!(limits.max_object_bytes, 1024 * 1024);
/// assert_eq!(limits.max_delta_depth, 16);
/// ```
///
/// External struct literals, including update syntax, are rejected:
///
/// ```compile_fail,E0639
/// use girt::ReadLimits;
///
/// let limits = ReadLimits {
///     max_object_bytes: 1024 * 1024,
///     ..ReadLimits::default()
/// };
/// ```
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ReadLimits {
    /// Maximum bytes in each object payload (default 64 MiB).
    pub max_object_bytes: usize,
    /// Maximum bytes in each inflated delta instruction stream (default 64 MiB).
    pub max_delta_bytes: usize,
    /// Maximum cumulative decoded/reconstructed bytes for one read (default 256 MiB).
    pub max_decode_bytes: usize,
    /// Maximum cumulative compressed entry bytes per packed read (default 256 MiB).
    ///
    /// Charges complete entry ranges before inflation, including bases; loose reads retain
    /// their existing output-size policy. This is a work bound, not retained input memory.
    pub max_input_bytes: usize,
    /// Maximum number of delta edges (default 64); zero permits ordinary objects only.
    pub max_delta_depth: usize,
}

impl ReadLimits {
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

    /// Sets [`Self::max_input_bytes`].
    #[must_use]
    pub fn with_max_input_bytes(mut self, max_input_bytes: usize) -> Self {
        self.max_input_bytes = max_input_bytes;
        self
    }

    /// Sets [`Self::max_delta_depth`].
    #[must_use]
    pub fn with_max_delta_depth(mut self, max_delta_depth: usize) -> Self {
        self.max_delta_depth = max_delta_depth;
        self
    }
}

impl Default for ReadLimits {
    fn default() -> Self {
        Self {
            max_object_bytes: 64 * 1024 * 1024,
            max_delta_bytes: 64 * 1024 * 1024,
            max_decode_bytes: 256 * 1024 * 1024,
            max_delta_depth: 64,
            max_input_bytes: 256 * 1024 * 1024,
        }
    }
}

impl ReadLimits {
    /// Returns generous finite bounds for reads whose resource cost the caller accepts.
    ///
    /// Byte limits keep arithmetic headroom and delta chains may have up to 10,000 edges. These
    /// values can permit allocations far beyond available memory; they do not reserve resources or
    /// prevent allocation failure. Prefer selected default-bound overrides for untrusted input.
    ///
    /// ```
    /// use girt::ReadLimits;
    /// // Accept large trusted payloads but retain an application-specific reconstruction bound.
    /// let limits = ReadLimits::trusted().with_max_delta_depth(128);
    /// assert_eq!(limits.max_delta_depth, 128);
    /// ```
    pub const fn trusted() -> Self {
        Self {
            max_object_bytes: usize::MAX / 4,
            max_delta_bytes: usize::MAX / 4,
            max_decode_bytes: usize::MAX / 4,
            max_input_bytes: usize::MAX / 4,
            max_delta_depth: 10_000,
        }
    }
}

/// Reads loose objects and validated, pinned local pack/index files.
///
/// # Common operations
///
/// Use [`Self::read`] for an object payload and [`Self::refresh`] to discover newly published
/// packs without replacing a valid reader on failure. [`Self::compare_trees`] reports structural
/// tree changes; [`Self::detect_rewrites`] infers content matches separately. Loose-object writes
/// belong to [`LooseObjects`], and reference updates belong to [`crate::refs`].
///
/// Obtain this synchronous, blocking reader through [`crate::Repository::objects`]. Opening streams
/// index bytes in the repository's object format in filename order and verifies index v1/v2
/// structure (table sizes, sorted identities, fanout, offsets that tile the pack), pack v2/v3
/// headers/counts, and index/pack trailer agreement. Like Git, it does not hash the whole index
/// at open. It reads only the pack header and trailer; body corruption is detected when used.
/// Reads check entry framing, v2 CRCs, zlib streams and delta programs throughout reconstruction,
/// and verify the requested object's decoded identity, so a damaged index triggers verified
/// fallback or an error rather than returning other content. Legacy v1 indexes have no entry CRCs.
/// [`Self::verify_packs`] explicitly checks index checksums, whole-pack checksums and CRCs; neither
/// opening nor whole-pack verification is a full fsck.
///
/// Pack bytes are never retained in full. Bounded index bytes and offset tables stay in memory
/// so identity lookups avoid repeated file seeks. Each pair pins two handles, shared internally
/// through `Arc`. Reads are positioned, so simultaneous reads share no cursor or lock; a typical
/// entry's header and compressed bytes come from one read. Cloning shares pack handles, offset
/// tables and delta base caches, while copying the bounded store topology. Each pack keeps
/// recently decoded entries, up to 96 MiB, as delta bases; a cached entry is still
/// identity-checked before it is returned. Failed packed entries are remembered and skipped by
/// subsequent content reads, shared by clones and preserved across refresh of unchanged pack
/// generations (pinned size, modification time, native identity and trailer checksum).
/// Metadata-only freshening of a still-pinned native artifact also preserves damage.
/// [`PackLimits`] bounds aggregate opening work, index memory and handles; [`ReadLimits`] bounds
/// each packed decode. Retention operations additionally share a single 256 KiB header window
/// across packs on the calling thread, dropped when the operation ends. Windows artifact handles
/// allow read/delete sharing and deny concurrent data writers.
/// Concurrent reads and caller-owned results multiply memory use. Cancellation is available through
/// [`crate::Repository::objects_controlled`] and [`Self::read_controlled`].
///
/// Within each store, packs are searched in index filename order, followed by fresh loose reads.
/// Stores are searched primary first, then alternates in discovery order. A corrupt packed entry
/// falls through to the remaining copies, each fully verified. A corrupt loose copy normally
/// fails immediately; after packed damage, it also falls through to the remaining stores.
/// Known damaged entries remain excluded for this handle's lifetime. Refresh retains small
/// generation records without retaining old artifact handles or index tables. These records grow
/// with distinct pack generations, including freshened metadata, opened by the handle and its
/// clones. Packs remain readable
/// after Git repacks/deletes the original files where the OS permits unlinking open files; refresh
/// to discover new packs. Artifacts must remain immutable while readers use them: pinned handles do
/// not defend against in-place writes (including truncation). Atomic path replacement leaves pinned
/// artifacts intact. Opening and refresh validate each pinned pair, but do not capture an atomic
/// directory or topology snapshot. Exclude writers when a point-in-time view is required.
/// [`Self::refresh`] makes one bounded attempt and leaves this reader unchanged on failure. Reads
/// never refresh implicitly. The object directory and its ancestors must be trusted, as with
/// [`LooseObjects`]. This is not a snapshot of loose files or repository references.
///
/// Overlapping readers may share validated index buffers and tables through a process-local weak
/// cache. Keys include native identity, size, mtime and Unix ctime or Windows ChangeTime. Shared
/// validation owns independent artifact pins while readers or in-flight opens use that generation.
/// Dropping its last reader releases those pins and buffers once any in-flight open finishes. Dead
/// weak cache records retain neither buffers nor handles, so they do not keep deleted packs alive
/// or Windows names delete-pending. Other snapshots or external processes can still pin the files.
/// Opens after all readers drop validate again. Metadata keys do not replace immutable publication
/// or caller-owned exclusion of in-place writers.
///
/// REF_DELTA bases must be indexed in the same pack. Thin packs and cross-pack/loose bases return
/// [`ObjectReadError::MissingBase`], even if the base exists elsewhere. Traversal is iterative,
/// detects cycles, and enforces [`ReadLimits`]. Multi-pack indexes accelerate verified reads when
/// usable; bitmap/reverse indexes are ignored; ordinary pack indexes remain required. Indexes are
/// publication markers: unindexed packs are ignored, while an index without its pack is an opening
/// error. A publisher must finish the pack before publishing its index. History queries use the
/// repository handle's immutable shallow boundaries; raw reads preserve commit parents. Reopen the
/// repository or refresh its shallow snapshot, then create a new reader after Git deepening. Loose
/// objects, packs and boundaries are not captured in one filesystem transaction; exclude concurrent
/// depth changes while opening. Local alternate stores use the same format and aggregate pack
/// limits; see [`crate::Repository::objects_with_alternates`] for topology and precedence. Complete
/// partial-clone stores are readable; no missing-object fetch is attempted. Loose writes
/// use [`LooseObjects`]; validated received-pack installation uses
/// [`crate::fetch::ReceivedFetch::install`].
///
/// The repository selects SHA-1 or SHA-256 for both artifacts. Their headers and filenames do
/// not select the format. Every `.idx` filename is a publication marker, including noncanonical
/// basenames; matching checksums and object identities validate contents independently of names.
///
/// # Example
///
/// Read an object by full identity, then parse its exact bytes when structured fields are needed.
/// Absence is `Ok(None)`, not an error:
///
/// ```
/// use girt::{InitKind, ObjectFormat, ObjectKind, PackLimits, ReadLimits, Repository, Tree};
///
/// let directory = tempfile::tempdir()?;
/// let repository = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("repo.git"),
///     InitKind::Bare,
/// )?;
/// let tree = Tree::new(ObjectFormat::Sha1, vec![])?;
/// let id = repository.loose_objects().write_tree(&tree)?;
///
/// let objects = repository.objects_with(PackLimits::default())?;
/// let object = objects
///     .read_with(id, ReadLimits::default())?
///     .expect("tree was just written");
/// assert_eq!(object.kind(), ObjectKind::Tree);
/// assert!(
///     Tree::parse(ObjectFormat::Sha1, object.data())?
///         .entries()
///         .is_empty()
/// );
///
/// let missing = ObjectFormat::Sha1.hash_object(ObjectKind::Blob, b"never written");
/// assert!(objects.read_with(missing, ReadLimits::default())?.is_none());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct Objects {
    format: ObjectFormat,
    stores: Vec<Store>,
    directory: PathBuf,
    pub(crate) shallow: crate::ShallowRoots,
    pub(crate) commit_graph: Option<Arc<crate::commit_graph::CommitGraph>>,
    /// Artifact-generation evidence survives refresh without retaining old handles/tables.
    damage_history: Arc<crate::pack::DamageHistory>,
    abbreviations: Arc<prefix::LooseAbbreviations>,
    pub(crate) multi_pack_index_enabled: bool,
}

#[derive(Debug, Clone)]
struct Store {
    directory: PathBuf,
    loose: LooseObjects,
    packs: Vec<Arc<FilePack>>,
    /// `.pack` path of each entry in `packs`, in the same order.
    pack_paths: Vec<PathBuf>,
    /// `.keep`/`.promisor` markers of each entry in `packs` observed while opening.
    pack_marks: Vec<listing::PackMarks>,
    midx: Option<crate::multi_pack_index::Lookup>,
}

fn share_pack_damage(
    stores: &mut [Store],
    history: &crate::pack::DamageHistory,
) -> Result<(), ObjectReadError> {
    for store in stores {
        for pack in &mut store.packs {
            // Only unpublished, freshly opened pairs enter this helper.
            let pack = Arc::get_mut(pack).ok_or(ObjectReadError::Corrupt(
                "pack shared before opening completed",
            ))?;
            history.share(pack);
        }
    }
    Ok(())
}

impl Objects {
    /// Returns canonical object directories in search order, primary first.
    ///
    /// Subsequent entries are alternates owned outside this repository. A maintenance planner
    /// must never treat their contents as owned pruning candidates.
    pub fn store_directories(&self) -> impl Iterator<Item = &Path> {
        self.stores.iter().map(|store| store.directory.as_path())
    }
    /// Fixed history boundaries inherited from the repository handle, unaffected by later refresh.
    pub fn shallow_roots(&self) -> &crate::ShallowRoots {
        &self.shallow
    }

    /// Format shared by all objects in this store.
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }

    #[cfg(test)]
    pub(crate) fn open(
        format: ObjectFormat,
        directory: &Path,
        limits: PackLimits,
        alternates: AlternateLimits,
    ) -> Result<Self, ObjectReadError> {
        Self::open_controlled(
            format,
            directory,
            limits,
            alternates,
            &AtomicBool::new(false),
        )
    }

    #[cfg(test)]
    pub(crate) fn open_controlled(
        format: ObjectFormat,
        directory: &Path,
        limits: PackLimits,
        alternates: AlternateLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, ObjectReadError> {
        Self::open_controlled_with_midx(format, directory, limits, alternates, cancelled, true)
    }

    pub(crate) fn open_controlled_with_midx(
        format: ObjectFormat,
        directory: &Path,
        limits: PackLimits,
        alternates: AlternateLimits,
        cancelled: &AtomicBool,
        multi_pack_index_enabled: bool,
    ) -> Result<Self, ObjectReadError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(
            target: "girt",
            "objects.open",
            object_format = %format,
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            check_cancelled(cancelled)?;
            let directories = alternates::discover(directory, alternates)?;
            check_cancelled(cancelled)?;
            let directory = directories
                .first()
                .cloned()
                .ok_or(ObjectReadError::Corrupt(
                    "object store has no primary directory",
                ))?;
            let mut budget = limits;
            let mut stores = Vec::new();
            for directory in directories {
                stores.push(Store::open(format, &directory, &mut budget, cancelled)?);
            }
            for store in stores.iter_mut().filter(|_| multi_pack_index_enabled) {
                check_cancelled(cancelled)?;
                store.midx = crate::multi_pack_index::Lookup::load(
                    format,
                    &store.directory.join("pack"),
                    &store.pack_paths,
                    budget.max_index_bytes,
                    cancelled,
                );
                check_cancelled(cancelled)?;
                if let Some(index) = &store.midx {
                    budget.max_index_bytes -= index.byte_len();
                }
            }
            let damage_history = Arc::new(crate::pack::DamageHistory::default());
            share_pack_damage(&mut stores, &damage_history)?;
            Ok(Self {
                format,
                stores,
                damage_history,
                abbreviations: Arc::default(),
                multi_pack_index_enabled,
                directory,
                shallow: crate::ShallowRoots::empty(format),
                commit_graph: None,
            })
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::object);

        result
    }

    /// Replaces this reader's pack and alternate view after one successful bounded scan.
    /// Also clears lazy loose-directory snapshots used by [`Self::abbreviate`].
    ///
    /// Rediscover from the original canonical primary directory using the supplied aggregate
    /// limits. Newly installed indexed packs and alternate topology become visible; removed packs
    /// disappear from this reader. Clones retain their previous pinned pairs and topology until
    /// they refresh or drop. Loose paths remain live in every reader, so an old reader can lose
    /// loose-only objects after GC or observe new loose objects without refresh. Shallow roots
    /// remain unchanged; refresh the repository's shallow snapshot separately when needed.
    ///
    /// Every pair is reopened and revalidated, even if its filename is unchanged. The old and new
    /// views coexist until success: peak handles and tables can reach their combined limits,
    /// multiplied by independently retained generations. Clone shares handles within a generation.
    /// Like [`crate::Repository::objects`], refresh validates index structure and pack
    /// headers/trailers without hashing indexes or scanning pack bodies. Use
    /// [`Self::verify_packs`] for index checksum, whole-pack checksum and CRC proof.
    ///
    /// # Errors
    ///
    /// Returns opening errors without changing this reader or its clones. An index whose pack is
    /// absent, a mismatched pair, malformed alternate metadata or corrupt index/header is an error,
    /// never absence. Pack body corruption is deferred to reads or explicit verification. There are
    /// no automatic retries, sleeps or filesystem changes. After a publisher finishes or a
    /// broken pair is repaired/removed, the caller may explicitly retry with its own finite
    /// attempt budget. Continuing publication may require another refresh even after success:
    /// directory enumeration and alternate discovery are not atomic snapshots.
    ///
    /// ```
    /// use girt::{
    ///     AlternateLimits, InitKind, ObjectFormat, ObjectId, ObjectKind, PackLimits, PackObject,
    ///     PackWriteLimits, ReadLimits, Repository, write_pack,
    /// };
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let mut objects = repo.objects_with(PackLimits::default())?;
    /// let retained = objects.clone();
    ///
    /// let data = b"published only in a new pack";
    /// let id = ObjectId::for_blob(ObjectFormat::Sha1, data);
    /// let input = PackObject {
    ///     id,
    ///     kind: ObjectKind::Blob,
    ///     data,
    /// };
    ///
    /// let (mut pack, mut index) = (Vec::new(), Vec::new());
    /// let written = write_pack(
    ///     ObjectFormat::Sha1,
    ///     &[input],
    ///     &mut pack,
    ///     &mut index,
    ///     PackWriteLimits::default(),
    /// )?;
    ///
    /// let base = repo
    ///     .object_dir()
    ///     .join("pack")
    ///     .join(format!("pack-{}", written.checksum));
    /// // Private fixture only: shared installation requires a coordinated publication path.
    /// std::fs::write(base.with_extension("pack"), pack)?;
    /// std::fs::write(base.with_extension("idx"), index)?;
    ///
    /// assert!(objects.read_with(id, ReadLimits::default())?.is_none());
    /// objects.refresh(PackLimits::default(), AlternateLimits::default())?;
    /// assert_eq!(
    ///     objects
    ///         .read_with(id, ReadLimits::default())?
    ///         .ok_or("missing")?
    ///         .data(),
    ///     data
    /// );
    ///
    /// // The clone retains the old generation.
    /// assert!(retained.read_with(id, ReadLimits::default())?.is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn refresh(
        &mut self,
        packs: PackLimits,
        alternates: AlternateLimits,
    ) -> Result<(), ObjectReadError> {
        self.refresh_controlled(packs, alternates, &AtomicBool::new(false))
    }

    /// Refreshes with the same cooperative cancellation checkpoints as opening.
    ///
    /// Cancellation before replacing the view releases all candidate handles and preserves the
    /// current view. Blocking OS calls cannot be interrupted. See [`Self::refresh`] for resource,
    /// publication, retained-reader and explicit retry contracts.
    ///
    /// # Errors
    ///
    /// Returns the errors from [`Self::refresh`] or [`ObjectReadError::Cancelled`].
    pub fn refresh_controlled(
        &mut self,
        packs: PackLimits,
        alternates: AlternateLimits,
        cancelled: &AtomicBool,
    ) -> Result<(), ObjectReadError> {
        let mut candidate = Self::open_controlled_with_midx(
            self.format,
            &self.directory,
            packs,
            alternates,
            cancelled,
            self.multi_pack_index_enabled,
        )?;
        check_cancelled(cancelled)?;
        share_pack_damage(&mut candidate.stores, &self.damage_history)?;
        self.stores = candidate.stores;
        self.abbreviations = candidate.abbreviations;
        Ok(())
    }

    pub(crate) fn retention_directories(directory: &Path) -> Result<Vec<PathBuf>, ObjectReadError> {
        alternates::discover(directory, AlternateLimits::default())
    }

    /// Retention-only lookup with the same store and pack precedence as `read_controlled`.
    /// A located blob is not an identity-verified `Object`.
    pub(crate) fn locate(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<(ObjectKind, u64)>, ObjectReadError> {
        self.locate_with(id, limits, cancel, None)
    }

    /// Locates like [`Self::locate`], opening a loose file only when `listing` names it.
    pub(crate) fn locate_listed(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
        listing: &LooseListing,
    ) -> Result<Option<(ObjectKind, u64)>, ObjectReadError> {
        self.locate_with(id, limits, cancel, Some(listing))
    }

    fn locate_with(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
        listing: Option<&LooseListing>,
    ) -> Result<Option<(ObjectKind, u64)>, ObjectReadError> {
        id.require_format(self.format)
            .map_err(crate::loose::Error::from)?;
        let listed = self.listed(listing, id)?;
        for store in &self.stores {
            check_cancelled(cancel)?;
            for pack in &store.packs {
                if let Some(position) = pack.find(id)? {
                    return pack.locate(position, limits, cancel).map(Some);
                }
            }
            if !listed {
                continue;
            }
            match store.loose.locate(id) {
                Ok(located) => {
                    if located.1 > limits.max_object_bytes as u64 {
                        return Err(ObjectReadError::Limit("object bytes"));
                    }
                    return Ok(Some(located));
                }
                Err(crate::loose::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok((id == self.format.empty_tree()).then_some((ObjectKind::Tree, 0)))
    }

    /// Locates every identity in `ids` as [`Self::locate`] would, visiting packed entries in pack
    /// order and reading each shared delta-chain header once per batch.
    ///
    /// Results pair each identity with its location; their order is unspecified.
    pub(crate) fn locate_all(
        &self,
        ids: impl IntoIterator<Item = ObjectId>,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<Located>, ObjectReadError> {
        let mut located = Vec::new();
        let mut packed = Vec::new();
        'ids: for id in ids {
            id.require_format(self.format)
                .map_err(crate::loose::Error::from)?;
            let mut pack_index = 0;
            for store in &self.stores {
                check_cancelled(cancel)?;
                for pack in &store.packs {
                    if let Some(position) = pack.find(id)? {
                        packed.push((pack_index, pack.offset(position)?, position, id));
                        continue 'ids;
                    }
                    pack_index += 1;
                }
                match store.loose.locate(id) {
                    Ok(loose) => {
                        if loose.1 > limits.max_object_bytes as u64 {
                            return Err(ObjectReadError::Limit("object bytes"));
                        }
                        located.push((id, Some(loose)));
                        continue 'ids;
                    }
                    Err(crate::loose::Error::Io(error))
                        if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            let empty = id == self.format.empty_tree();
            located.push((id, empty.then_some((ObjectKind::Tree, 0))));
        }
        packed.sort_unstable();
        let packs: Vec<_> = self.stores.iter().flat_map(|store| &store.packs).collect();
        let _ordered = crate::pack::HeaderScope::ordered();
        let mut known = (usize::MAX, crate::pack::BaseKinds::new());
        for (pack, _, position, id) in packed {
            check_cancelled(cancel)?;
            if known.0 != pack {
                known = (pack, crate::pack::BaseKinds::new());
            }
            #[expect(
                clippy::indexing_slicing,
                reason = "pack positions were enumerated from these same stores above"
            )]
            let found = packs[pack].locate_memoized(position, limits, cancel, &mut known.1)?;
            located.push((id, Some(found)));
        }
        Ok(located)
    }

    /// A located blob must not hide a readable graph copy at the same identity. Such a conflict
    /// could otherwise erase edges and let pruning delete that graph's only loose descendants.
    pub(crate) fn check_retention_blob_kinds(
        &self,
        limits: ReadLimits,
        max_entries: usize,
        cancel: &AtomicBool,
        is_blob: impl Fn(ObjectId) -> bool,
    ) -> Result<(), ObjectReadError> {
        if is_blob(self.format.empty_tree()) {
            return Err(ObjectReadError::Corrupt("blob claims canonical empty tree"));
        }
        let check_packs = self.stores.len() > 1
            || self
                .stores
                .first()
                .is_some_and(|store| store.packs.len() > 1);
        let mut entries = 0;
        for store in &self.stores {
            if check_packs {
                store.check_packed_blob_kinds(
                    limits,
                    max_entries,
                    cancel,
                    &is_blob,
                    &mut entries,
                )?;
            }
            store.check_loose_blob_kinds(max_entries, cancel, &is_blob, &mut entries)?;
        }
        Ok(())
    }

    /// Revalidates every pinned artifact before a retention mutation.
    pub(crate) fn check_packs_unchanged(&self, cancel: &AtomicBool) -> Result<(), ObjectReadError> {
        for store in &self.stores {
            for pack in &store.packs {
                check_cancelled(cancel)?;
                pack.check_unchanged()?;
            }
        }
        check_cancelled(cancel)
    }

    /// Verifies index checksums, whole-pack checksums and v2 entry CRCs in every pinned store,
    /// including alternates.
    ///
    /// Opening checks index structure, pack framing and trailer agreement without hashing the
    /// index or reading pack bodies. Use this operation when the complete raw-byte checksum proof
    /// is required. It streams each pack with bounded scratch memory and checks cancellation
    /// every 64 KiB. The artifact sizes already charged to [`PackLimits::max_bytes`] bound this
    /// work. It changes no files or reader state; failure and cancellation leave the reader
    /// usable for unaffected objects.
    ///
    /// This is not a full fsck: it does not decode objects, verify their identities, validate
    /// payload syntax or check connectivity. Legacy v1 indexes have no entry CRCs. Artifacts
    /// must remain immutable throughout verification, as throughout this reader's lifetime.
    ///
    /// # Errors
    ///
    /// Returns checksum/CRC corruption, artifact I/O errors or [`ObjectReadError::Cancelled`].
    /// Also refuses packed damage remembered by prior reads, including identity-only failures;
    /// that contextual error retains the path, offset and original corruption cause.
    ///
    /// ```
    /// use girt::{ObjectFormat, ObjectId, ObjectKind, PackObject, PackWriteLimits, write_pack};
    /// let input = PackObject {
    ///     id: ObjectId::for_blob(ObjectFormat::Sha1, b"manifest"),
    ///     kind: ObjectKind::Blob,
    ///     data: b"manifest",
    /// };
    ///
    /// let (mut bytes, mut index) = (Vec::new(), Vec::new());
    /// let written = write_pack(
    ///     ObjectFormat::Sha1,
    ///     &[input],
    ///     &mut bytes,
    ///     &mut index,
    ///     PackWriteLimits::default(),
    /// )?;
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = girt::Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     girt::InitKind::Bare,
    /// )?;
    ///
    /// let base = repo
    ///     .object_dir()
    ///     .join("pack")
    ///     .join(format!("pack-{}", written.checksum));
    /// // Private fixture only; shared repositories need a validated installation path.
    /// std::fs::write(base.with_extension("pack"), bytes)?;
    /// std::fs::write(base.with_extension("idx"), index)?;
    ///
    /// let objects = repo.objects_with(girt::PackLimits::default())?;
    /// objects.verify_packs(&std::sync::atomic::AtomicBool::new(false))?;
    /// assert!(
    ///     objects
    ///         .read_with(input.id, girt::ReadLimits::default())?
    ///         .is_some()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn verify_packs(&self, cancelled: &AtomicBool) -> Result<(), ObjectReadError> {
        check_cancelled(cancelled)?;
        for store in &self.stores {
            for pack in &store.packs {
                pack.verify(cancelled)?;
            }
        }
        check_cancelled(cancelled)
    }

    /// Verifies every pinned index's own checksum, including alternates, without reading packs.
    pub(crate) fn verify_indexes(&self, cancelled: &AtomicBool) -> Result<(), ObjectReadError> {
        for store in &self.stores {
            for pack in &store.packs {
                check_cancelled(cancelled)?;
                pack.verify_index()?;
            }
        }
        Ok(())
    }

    /// Reads an exact object by full identity in this store's format; returns `None` only when
    /// absent.
    ///
    /// The payload is uninterpreted. Parse it separately if structured fields are needed.
    /// Searches each store's pinned packs in filename order, then its live loose files, primary
    /// before alternates. Packed corruption retries remaining copies with full identity checks.
    /// Failed packed entries are remembered for this handle and its clones; refresh preserves
    /// evidence for the same artifacts. Reads never refresh storage implicitly.
    ///
    /// # Errors
    ///
    /// Returns storage errors for corrupt/unsupported objects, I/O failures, missing delta bases,
    /// cycles, incompatible identity formats, or exhausted resource limits. An invalid loose object
    /// fails immediately unless this read is already retrying packed corruption. If all copies are
    /// absent or damaged after a packed failure, the error names that pack; its source chain
    /// includes the entry offset and concrete corruption cause. Packed damage uses
    /// [`ObjectReadError::Path`] with an `InvalidData` I/O source. Cancellation,
    /// resource limits, unsupported headers, missing/external delta bases and ordinary I/O
    /// failures do not mark entries bad or trigger fallback. Only the first packed corruption
    /// cause is reported, including when a later loose copy is also corrupt. No filesystem changes
    /// occur.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, PackLimits, ReadLimits, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"payload\n")?;
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// let limits = ReadLimits::default().with_max_object_bytes(1024);
    /// assert_eq!(
    ///     objects.read_with(id, limits)?.ok_or("missing blob")?.data(),
    ///     b"payload\n"
    /// );
    ///
    /// let absent = girt::ObjectId::for_blob(ObjectFormat::Sha1, b"not written");
    /// assert!(objects.read_with(absent, limits)?.is_none());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_with(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<Object>, ObjectReadError> {
        self.read_controlled(id, limits, &AtomicBool::new(false))
    }

    /// Reports whether this snapshot holds `id`, without reading or verifying the stored copy.
    ///
    /// Searches the same stores in the same order as [`Self::read`]: each store's pinned pack
    /// indexes, then its live loose files, primary before alternates. The canonical empty tree is
    /// always present, as in [`Self::read`]. A pack published after this snapshot was taken is not
    /// seen, so a `false` can be out of date; [`Self::refresh`] picks up new packs.
    ///
    /// Use this to skip storing content the repository already has, as Git does when it writes an
    /// object that is already packed. A `true` is the store's claim, not proof that the copy
    /// decodes: [`Self::read`] verifies identity and reports a damaged copy when the object is
    /// read.
    ///
    /// # Errors
    ///
    /// Returns [`ObjectReadError::Loose`] wrapping a format error for a foreign-format `id`,
    /// and storage errors when a pack index or loose object directory cannot be read. A
    /// missing loose file is absence, not an error. No filesystem changes occur.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, ObjectId, PackLimits, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    /// let stored = repo.loose_objects().write_blob(b"payload\n")?;
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// assert!(objects.contains(stored)?);
    ///
    /// // Hash first, then store only what the repository lacks.
    /// let content = b"new content\n";
    /// let id = ObjectId::for_blob(ObjectFormat::Sha1, content);
    /// assert!(!objects.contains(id)?);
    /// assert_eq!(repo.loose_objects().write_blob(content)?, id);
    /// assert!(objects.contains(id)?);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn contains(&self, id: ObjectId) -> Result<bool, ObjectReadError> {
        id.require_format(self.format)
            .map_err(crate::loose::Error::from)?;
        for store in &self.stores {
            for pack in &store.packs {
                if pack.find(id)?.is_some() {
                    return Ok(true);
                }
            }
            if store.loose.contains(id)? {
                return Ok(true);
            }
        }
        Ok(id == self.format.empty_tree())
    }

    /// Reads an exact object by full identity with [`ReadLimits::default`]; returns `None` only
    /// when absent.
    ///
    /// Equivalent to `self.read_with(id, ReadLimits::default())`; see [`Self::read_with`] for store
    /// precedence and damage handling. The default limits refuse payloads over 64 MiB. Use
    /// [`Self::read_commit`], [`Self::read_tree`], [`Self::read_tag`] or
    /// [`Self::read_blob`] when the kind is known.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::read_with`].
    pub fn read(&self, id: ObjectId) -> Result<Option<Object>, ObjectReadError> {
        self.read_with(id, ReadLimits::default())
    }

    /// Reads with cooperative cancellation between stores, pack lookups and decode chunks.
    ///
    /// Loose reads, index lookups, object hashing and individual delta applications finish before
    /// the next checkpoint. Blocked filesystem calls cannot be interrupted. No partial object is
    /// returned; all per-read buffers are released when the call returns.
    ///
    /// # Errors
    ///
    /// Returns the same storage errors as [`Self::read`], or [`ObjectReadError::Cancelled`].
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, PackLimits, ReadLimits, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"payload")?;
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// let cancelled = std::sync::atomic::AtomicBool::new(true);
    /// assert!(matches!(
    ///     objects.read_controlled(id, ReadLimits::default(), &cancelled),
    ///     Err(girt::ObjectReadError::Cancelled)
    /// ));
    ///
    /// assert!(objects.read_with(id, ReadLimits::default())?.is_some());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_controlled(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<Option<Object>, ObjectReadError> {
        #[cfg(feature = "tracing")]
        let span = tracing::trace_span!(
            target: "girt",
            "objects.read",
            outcome = "incomplete",
            failure_class = tracing::field::Empty,
            effects = tracing::field::Empty,
        );

        let operation = || {
            check_cancelled(cancelled)?;
            let loose_limit = limits.max_object_bytes.min(limits.max_decode_bytes);
            let mut damage = None;
            for store in &self.stores {
                check_cancelled(cancelled)?;
                if let Some((pack, position)) = store
                    .midx
                    .as_ref()
                    .filter(|_| self.multi_pack_index_enabled)
                    .and_then(|midx| midx.find(id, &store.packs))
                    .and_then(|(ordinal, position)| {
                        store.packs.get(ordinal).map(|pack| (pack, position))
                    })
                {
                    match pack.read_candidate(position, limits, cancelled) {
                        Ok(object) => {
                            check_cancelled(cancelled)?;
                            return Ok(Some(object));
                        }
                        Err(crate::pack::ReadFailure::Damaged(_)) => {}
                        Err(crate::pack::ReadFailure::Other(error)) => return Err(error),
                    }
                }
                // Like Git, look in packs first: most objects are packed, and an object ID
                // determines its content wherever it is stored.
                for pack in &store.packs {
                    check_cancelled(cancelled)?;
                    if let Some(position) = pack.find(id)? {
                        match pack.read_candidate(position, limits, cancelled) {
                            Ok(object) => {
                                check_cancelled(cancelled)?;
                                return Ok(Some(object));
                            }
                            Err(crate::pack::ReadFailure::Damaged(error)) => {
                                damage.get_or_insert(error);
                            }
                            Err(crate::pack::ReadFailure::Other(error)) => return Err(error),
                        }
                    }
                }
                match store.loose.read_raw(id, loose_limit) {
                    Ok(object) => {
                        check_cancelled(cancelled)?;
                        return Ok(Some(object));
                    }
                    Err(crate::loose::Error::Io(error))
                        if error.kind() == io::ErrorKind::NotFound => {}
                    Err(crate::loose::Error::Corrupt(_)) if damage.is_some() => {}
                    Err(error) => return Err(error.into()),
                }
            }
            check_cancelled(cancelled)?;
            if let Some(error) = damage {
                return Err(error);
            }
            // Git treats the canonical empty tree as present in every repository, even when it
            // is not stored.
            if id == self.format.empty_tree() {
                return Ok(Some(Object {
                    kind: ObjectKind::Tree,
                    format: self.format,
                    data: Arc::default(),
                }));
            }
            Ok(None)
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = { operation }();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, crate::trace::object);

        result
    }
}

impl Objects {
    /// Reads `id` for a selection walk without hashing a packed payload; see [`UnverifiedObject`].
    ///
    /// Store precedence, limits and errors match [`Self::read_controlled`]. Loose objects are
    /// still identity-checked: their only integrity check is the hash. After a packed failure or
    /// known bad entry, every fallback uses full identity checks.
    pub(crate) fn read_unverified(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancelled: &AtomicBool,
    ) -> Result<Option<UnverifiedObject>, ObjectReadError> {
        check_cancelled(cancelled)?;
        let loose_limit = limits.max_object_bytes.min(limits.max_decode_bytes);
        for store in &self.stores {
            check_cancelled(cancelled)?;
            for pack in &store.packs {
                check_cancelled(cancelled)?;
                if let Some(position) = pack.find(id)? {
                    match pack.read_candidate_unverified(position, limits, cancelled) {
                        Ok(object) => {
                            check_cancelled(cancelled)?;
                            return Ok(Some(object));
                        }
                        Err(crate::pack::ReadFailure::Damaged(_)) => {
                            // Restart with full identity checks: known bad entries are skipped,
                            // and no unchecked fallback can supply selection edges.
                            return self.read_controlled(id, limits, cancelled).map(|object| {
                                object.map(|object| UnverifiedObject {
                                    kind: object.kind,
                                    data: object.data,
                                })
                            });
                        }
                        Err(crate::pack::ReadFailure::Other(error)) => return Err(error),
                    }
                }
            }
            match store.loose.read_raw(id, loose_limit) {
                Ok(object) => {
                    check_cancelled(cancelled)?;
                    return Ok(Some(UnverifiedObject {
                        kind: object.kind,
                        data: object.data,
                    }));
                }
                Err(crate::loose::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        check_cancelled(cancelled)?;
        Ok((id == self.format.empty_tree()).then(|| UnverifiedObject {
            kind: ObjectKind::Tree,
            data: Arc::default(),
        }))
    }

    /// Checks physical empty-tree availability, excluding the reader's synthetic fallback.
    pub(crate) fn has_stored_empty_tree(
        &self,
        cancel: &AtomicBool,
    ) -> Result<bool, ObjectReadError> {
        let id = self.format.empty_tree();
        let hex = id.to_string();
        for store in &self.stores {
            check_cancelled(cancel)?;
            for pack in &store.packs {
                check_cancelled(cancel)?;
                if pack.find(id)?.is_some() {
                    return Ok(true);
                }
            }
            let path = store.directory.join(&hex[..2]).join(&hex[2..]);
            if path
                .try_exists()
                .map_err(|source| ObjectReadError::Path { path, source })?
            {
                return Ok(true);
            }
        }
        check_cancelled(cancel)?;
        Ok(false)
    }

    /// Whether normal lookup selects a byte-bound received pack for an identity it contains.
    /// Primary loose storage still precedes alternate packs. Other copies never inherit proof.
    pub(crate) fn selects_received(
        &self,
        id: ObjectId,
        received: &FilePack,
        cancel: &AtomicBool,
    ) -> Result<bool, ObjectReadError> {
        for store in &self.stores {
            for pack in &store.packs {
                check_cancelled(cancel)?;
                if std::ptr::eq(pack.as_ref(), received) {
                    return Ok(true);
                }
                if pack.find(id)?.is_some() {
                    return Ok(false);
                }
            }
            let hex = id.to_string();
            let path = store.directory.join(&hex[..2]).join(&hex[2..]);
            if path
                .try_exists()
                .map_err(|source| ObjectReadError::Path { path, source })?
            {
                return Ok(false);
            }
        }
        Ok(false)
    }

    /// All pinned packs in the same primary/alternate lookup order as `read`.
    pub(crate) fn transfer_packs(&self) -> Vec<&FilePack> {
        self.stores
            .iter()
            .flat_map(|s| s.packs.iter().map(AsRef::as_ref))
            .collect()
    }

    /// Whether a loose file for `id` may exist: always without a listing, else as listed.
    fn listed(
        &self,
        listing: Option<&LooseListing>,
        id: ObjectId,
    ) -> Result<bool, ObjectReadError> {
        listing.map_or(Ok(true), |listing| self.loose_listed(listing, id))
    }

    /// Locates the representation normal lookup uses; loose data is checked and then released.
    pub(crate) fn transfer_object(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<(ObjectKind, usize, Option<crate::pack::Packed>)>, ObjectReadError> {
        self.transfer_object_with(id, limits, cancel, None)
    }

    /// Like [`Self::transfer_object`], opening a loose file only when `listing` names it.
    pub(crate) fn transfer_object_listed(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
        listing: &LooseListing,
    ) -> Result<Option<(ObjectKind, usize, Option<crate::pack::Packed>)>, ObjectReadError> {
        self.transfer_object_with(id, limits, cancel, Some(listing))
    }

    fn transfer_object_with(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
        listing: Option<&LooseListing>,
    ) -> Result<Option<(ObjectKind, usize, Option<crate::pack::Packed>)>, ObjectReadError> {
        Ok(match self.locate_transfer(id, limits, cancel, listing)? {
            None => None,
            Some(Transfer::Packed(pack, packed)) => {
                let (kind, length) = pack.object_header(packed.position, limits)?;
                Some((kind, length, Some(packed)))
            }
            Some(Transfer::Stored(kind, length)) => Some((kind, length, None)),
        })
    }

    /// Locates like [`Self::transfer_object`] without reading a packed entry's header.
    ///
    /// Returns `None` for a missing object and `Some(None)` for one that is not packed, which is
    /// still checked. Whoever copies a packed entry reads and limits its header.
    pub(crate) fn transfer_location(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
    ) -> Result<Option<Option<crate::pack::Packed>>, ObjectReadError> {
        Ok(self
            .locate_transfer(id, limits, cancel, None)?
            .map(|transfer| match transfer {
                Transfer::Packed(_, packed) => Some(packed),
                Transfer::Stored(..) => None,
            }))
    }

    fn locate_transfer(
        &self,
        id: ObjectId,
        limits: ReadLimits,
        cancel: &AtomicBool,
        listing: Option<&LooseListing>,
    ) -> Result<Option<Transfer<'_>>, ObjectReadError> {
        id.require_format(self.format)
            .map_err(crate::loose::Error::from)?;
        let listed = self.listed(listing, id)?;
        let mut pack_index = 0;
        for store in &self.stores {
            check_cancelled(cancel)?;
            for pack in &store.packs {
                check_cancelled(cancel)?;
                if let Some(position) = pack.find(id)? {
                    return Ok(Some(Transfer::Packed(
                        pack,
                        crate::pack::Packed {
                            id,
                            pack: pack_index,
                            position,
                        },
                    )));
                }
                pack_index += 1;
            }
            if !listed {
                continue;
            }
            match store
                .loose
                .read_raw(id, limits.max_object_bytes.min(limits.max_decode_bytes))
            {
                Ok(object) => {
                    return Ok(Some(Transfer::Stored(object.kind(), object.data().len())));
                }
                Err(crate::loose::Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        if id == self.format.empty_tree() {
            return Ok(Some(Transfer::Stored(ObjectKind::Tree, 0)));
        }
        Ok(None)
    }

    /// Primary-store packs with their `.pack` paths, in search order.
    pub(crate) fn primary_packs_with_paths(&self) -> Vec<(&Path, &FilePack)> {
        self.stores
            .iter()
            .take(1)
            .flat_map(|store| {
                store
                    .pack_paths
                    .iter()
                    .map(Path::new)
                    .zip(store.packs.iter().map(AsRef::as_ref))
            })
            .collect()
    }
}

/// Where a transfer finds an object: a pack entry it can copy, or a checked loose or implicit
/// object it must re-encode.
enum Transfer<'a> {
    Packed(&'a FilePack, crate::pack::Packed),
    Stored(ObjectKind, usize),
}

impl Store {
    fn check_packed_blob_kinds(
        &self,
        limits: ReadLimits,
        max_entries: usize,
        cancel: &AtomicBool,
        is_blob: &impl Fn(ObjectId) -> bool,
        entries: &mut usize,
    ) -> Result<(), ObjectReadError> {
        // Pack order lets the header window serve neighbouring entries and each delta family's
        // chain resolve once, instead of one scattered read per entry in identity order.
        let _ordered = crate::pack::HeaderScope::ordered();
        for pack in &self.packs {
            let mut known = crate::pack::BaseKinds::new();
            for position in pack.positions_by_offset()? {
                check_cancelled(cancel)?;
                charge_copy_entry(entries, max_entries)?;
                if is_blob(pack.identity_at(position))
                    && pack
                        .locate_memoized(position, limits, cancel, &mut known)?
                        .0
                        != ObjectKind::Blob
                {
                    return Err(ObjectReadError::Corrupt(
                        "reachable blob has a graph-bearing copy",
                    ));
                }
            }
        }
        Ok(())
    }

    fn check_loose_blob_kinds(
        &self,
        max_entries: usize,
        cancel: &AtomicBool,
        is_blob: &impl Fn(ObjectId) -> bool,
        entries: &mut usize,
    ) -> Result<(), ObjectReadError> {
        let at = |source| ObjectReadError::Path {
            path: self.directory.clone(),
            source,
        };
        for entry in fs::read_dir(&self.directory).map_err(at)? {
            check_cancelled(cancel)?;
            charge_copy_entry(entries, max_entries)?;
            let entry = entry.map_err(at)?;
            let name = entry.file_name();
            let Some(prefix) = name.to_str() else {
                continue;
            };
            if prefix.len() != 2 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                continue;
            }
            if !fs::symlink_metadata(entry.path()).map_err(at)?.is_dir() {
                return Err(ObjectReadError::Corrupt("non-directory loose fanout"));
            }
            for file in fs::read_dir(entry.path()).map_err(at)? {
                check_cancelled(cancel)?;
                charge_copy_entry(entries, max_entries)?;
                let file = file.map_err(at)?;
                let name = file.file_name();
                let Some(suffix) = name.to_str() else {
                    continue;
                };
                let Ok(id) =
                    ObjectId::from_hex(self.loose.object_format(), &format!("{prefix}{suffix}"))
                else {
                    continue;
                };
                if is_blob(id) && self.loose.locate(id)?.0 != ObjectKind::Blob {
                    return Err(ObjectReadError::Corrupt(
                        "reachable blob has a graph-bearing copy",
                    ));
                }
            }
        }
        Ok(())
    }

    fn open(
        format: ObjectFormat,
        directory: &Path,
        budget: &mut PackLimits,
        cancelled: &AtomicBool,
    ) -> Result<Self, ObjectReadError> {
        let loose = LooseObjects::new(directory, format);
        let pack_directory = directory.join("pack");
        let entries = match fs::read_dir(&pack_directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Self {
                    directory: directory.to_owned(),
                    loose,
                    packs: vec![],
                    pack_paths: vec![],
                    pack_marks: vec![],
                    midx: None,
                });
            }
            Err(source) => {
                return Err(ObjectReadError::Path {
                    path: pack_directory,
                    source,
                });
            }
        };
        let mut paths = Vec::new();
        let mut markers = std::collections::BTreeSet::new();
        for entry in entries {
            check_cancelled(cancelled)?;
            budget.max_directory_entries = budget
                .max_directory_entries
                .checked_sub(1)
                .ok_or(ObjectReadError::Limit("pack directory entries"))?;
            let path = entry
                .map_err(|source| ObjectReadError::Path {
                    path: pack_directory.clone(),
                    source,
                })?
                .path();
            if path
                .extension()
                .is_some_and(|extension| extension == "keep" || extension == "promisor")
            {
                markers.insert(path);
                continue;
            }
            if path.extension().is_some_and(|extension| extension == "idx") {
                budget.max_packs = budget
                    .max_packs
                    .checked_sub(1)
                    .ok_or(ObjectReadError::Limit("pack count"))?;
                paths.push(path);
            }
        }
        paths.sort();
        let pack_paths: Vec<PathBuf> = paths.iter().map(|p| p.with_extension("pack")).collect();
        let pack_marks = paths
            .iter()
            .map(|path| listing::PackMarks {
                keep: markers.contains(&path.with_extension("keep")),
                promisor: markers.contains(&path.with_extension("promisor")),
            })
            .collect();
        let mut packs = Vec::new();
        for path in paths {
            budget.max_open_files = budget
                .max_open_files
                .checked_sub(2)
                .ok_or(ObjectReadError::Limit("pack file handles"))?;
            packs.push(Arc::new(
                FilePack::open(format, &path, budget, cancelled).map_err(
                    |source| match source {
                        ObjectReadError::Limit(_)
                        | ObjectReadError::Cancelled
                        | ObjectReadError::Path { .. } => source,
                        source => ObjectReadError::PackArtifacts {
                            pack: path.with_extension("pack"),
                            index: path,
                            source: Box::new(source),
                        },
                    },
                )?,
            ));
        }
        Ok(Self {
            directory: directory.to_owned(),
            loose,
            packs,
            pack_paths,
            pack_marks,
            midx: None,
        })
    }
}

/// Failures opening or reading a repository object store; absence is `Ok(None)`.
///
/// [`Self::Limit`] reports the exhausted budget; increase that budget only after assessing input
/// trust and resource cost. [`Self::PackArtifacts`] adds the pack/index paths around an opening
/// failure. Packed read failures can retain corruption details inside an I/O source chain; keep
/// that chain in diagnostics rather than printing only the outer message. Unsupported storage and
/// missing delta bases require a complete supported representation, not relaxed verification.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ObjectReadError {
    /// A pinned artifact changed in place before a mutation boundary.
    /// Coordinate with in-place writers and reopen a verified snapshot before any mutation
    /// decision.
    #[error("pinned pack artifact changed: {path}")]
    PackChanged {
        /// Original path naming the pinned pack or index.
        path: PathBuf,
    },
    /// The caller requested cancellation; no partial object is returned.
    /// Retry only with a fresh cancellation decision; the current reader remains available.
    #[error("object storage cancelled")]
    Cancelled,
    /// An alternates record cannot be interpreted as a supported native path.
    /// Correct the indicated alternates record or supply a supported native path representation.
    #[error("invalid alternate path in {path}: {reason}")]
    Alternate {
        /// Alternates metadata file containing the record.
        path: PathBuf,
        /// Structural or platform restriction.
        reason: &'static str,
    },
    /// A recognized storage feature is outside the implemented subset.
    /// Select a supported storage representation; a larger budget does not add feature support.
    #[error("unsupported object storage: {0}")]
    Unsupported(&'static str),
    /// Filesystem access failed at this artifact or directory, or a damaged packed read exhausted
    /// its fallback copies. For packed corruption, the `InvalidData` source carries the offset
    /// and the original corruption error through its source chain.
    /// Inspect the original I/O source and path, including nested packed-corruption details.
    #[error("object storage at {path}")]
    Path {
        /// Artifact or directory being accessed.
        path: PathBuf,
        /// Concrete filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Validation of an index/pack pair failed during snapshot opening.
    /// Inspect both artifact paths and the nested opening failure; publish complete matching pairs.
    #[error("object storage pair {index} / {pack}")]
    PackArtifacts {
        /// Index identifying the pair.
        index: PathBuf,
        /// Pack paired with the index.
        pack: PathBuf,
        /// Concrete validation failure.
        #[source]
        source: Box<ObjectReadError>,
    },
    /// The existing loose reader rejected the object; preserves its concrete cause.
    /// Inspect the loose-object source, distinguishing absence, wrong kind, corruption, and size
    /// refusal.
    #[error("loose object")]
    Loose(#[from] crate::loose::Error),
    /// The index version is neither supported headerless v1 nor headered v2.
    /// Provide a supported index-v1 or index-v2 representation.
    #[error("unsupported pack index version {0}")]
    IndexVersion(u32),
    /// The pack version is neither 2 nor 3.
    /// Provide a supported pack-v2 or pack-v3 representation.
    #[error("unsupported pack version {0}")]
    PackVersion(u32),
    /// Reserved or unknown packed object type.
    /// Report the unsupported stored entry; do not guess an object kind.
    #[error("unsupported packed object type {0}")]
    ObjectType(u8),
    /// Structural validation, zlib completion, a checksum, or object identity failed.
    /// Recover verified artifacts from a trusted source; do not relax identity or checksum checks.
    #[error("corrupt packed storage: {0}")]
    Corrupt(&'static str),
    /// A named resource bound was exhausted; no partial object is returned.
    /// Identify the named opening or decoding budget and choose a justified value for a new
    /// attempt.
    #[error("object storage limit exceeded: {0}")]
    Limit(&'static str),
    /// REF_DELTA base is absent from its pack; external bases are unsupported.
    /// Supply a self-contained pack; external delta bases are not resolved by this stored-pack
    /// reader.
    #[error("missing or external delta base {0}")]
    MissingBase(ObjectId),
    /// A delta refers back to an object already in its reconstruction chain.
    /// Report the damaged dependency chain; increasing depth limits cannot resolve a cycle.
    #[error("cyclic delta chain")]
    DeltaCycle,
}

fn charge_copy_entry(entries: &mut usize, max: usize) -> Result<(), ObjectReadError> {
    *entries = entries
        .checked_add(1)
        .ok_or(ObjectReadError::Limit("retention copy entries"))?;
    if *entries > max {
        return Err(ObjectReadError::Limit("retention copy entries"));
    }
    Ok(())
}

#[cfg(test)]
#[path = "objects/tests.rs"]
mod tests;

impl Objects {
    pub(crate) fn verify_multi_pack_index(
        &self,
        index: &crate::multi_pack_index::MultiPackIndex<'_>,
    ) -> Result<(), crate::multi_pack_index::Error> {
        use crate::multi_pack_index::Error;
        for store in &self.stores {
            let selected = index
                .pack_names()
                .iter()
                .map(|name| {
                    let path = store
                        .directory
                        .join("pack")
                        .join(name)
                        .with_extension("pack");
                    store
                        .pack_paths
                        .iter()
                        .position(|candidate| *candidate == path)
                        .and_then(|ordinal| store.packs.get(ordinal))
                })
                .collect::<Option<Vec<_>>>();
            let Some(packs) = selected else {
                continue;
            };
            for pack in &packs {
                pack.verify_index()?;
            }
            index.verify_entries(&packs)?;
            return Ok(());
        }
        Err(Error::MissingPack)
    }
}
