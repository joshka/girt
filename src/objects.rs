mod alternates;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::{fs, io};

pub use alternates::AlternateLimits;

use crate::pack::FilePack;
use crate::pack::reader::check_cancelled;
use crate::{LooseObjects, ObjectFormat, ObjectId, ObjectKind};

/// An owned, identity-verified object kind and exact uncompressed payload.
///
/// Storage framing is validated; tree, commit, and tag payload syntax is not. Pass [`Self::data`]
/// to [`crate::Tree::parse`], [`crate::Commit::parse`], or [`crate::Tag::parse`] as appropriate.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Object {
    pub(crate) kind: ObjectKind,
    pub(crate) format: ObjectFormat,
    pub(crate) data: Vec<u8>,
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

    /// Takes ownership of the payload buffer.
    pub fn into_data(self) -> Vec<u8> {
        self.data
    }

    /// Hashes the kind, canonical header, and payload.
    pub fn id(&self) -> ObjectId {
        self.format.hash_object(self.kind, &self.data)
    }
}

/// Bounds the aggregate pack snapshot across primary and borrowed stores retained by
/// [`crate::Repository::objects`].
///
/// Artifact bytes bound validation work, not retained pack memory. Index input and derived tables
/// have a separate bound. These are per-reader bounds, not a process RSS quota. Zero values permit
/// no indexed packs. Unindexed files are ignored.
#[derive(Debug, Clone, Copy)]
pub struct PackLimits {
    /// Maximum sum of `.idx` and `.pack` file bytes validated (default 64 GiB on 64-bit).
    pub max_bytes: usize,
    /// Maximum aggregate index input bytes (default 64 MiB).
    ///
    /// Opening temporarily parses one bounded index. Input and derived allocations together
    /// are bounded by four times this value, excluding allocator overhead and path storage.
    pub max_index_bytes: usize,
    /// Maximum retained file handles (default 512); each pair needs two.
    pub max_open_files: usize,
    /// Maximum directory entries examined across all pack directories (default 1,000,000).
    pub max_directory_entries: usize,
    /// Maximum number of index/pack pairs (default 256).
    pub max_packs: usize,
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

/// Per-read bounds for loose payloads and iterative pack reconstruction.
///
/// Every base and reconstructed payload must fit `max_object_bytes`. Delta programs have their
/// own limit. `max_decode_bytes` charges every inflated program/base and every reconstructed
/// result, including intermediate results, before allocating its buffer. It therefore also bounds
/// retained decoding bytes, but excludes index tables, allocator overhead, traversal bookkeeping,
/// and fixed zlib scratch space. Loose reads use the smaller of object and decode limits, plus a
/// small framing allowance. No decoded objects are cached between reads.
#[derive(Debug, Clone, Copy)]
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

/// Reads loose objects and validated, pinned local pack/index files.
///
/// Obtain this synchronous, blocking reader through [`crate::Repository::objects`]. Opening streams
/// all `.idx`/`.pack` pairs in the repository's object format in filename order and verifies index
/// v1/v2 structure, checksums, pack v2/v3 headers/counts, offset ranges, and v2 entry CRCs.
/// Legacy v1 indexes have no entry CRCs. Entry framing, zlib streams, delta programs, and object
/// identities are checked on reads, including every base and intermediate delta; opening is not a
/// full pack fsck.
///
/// Pack bytes are never retained in full. Index identities are read from disk; bounded offset
/// tables stay in memory. Each pair pins two handles, shared internally through `Arc`. Short
/// seek/read operations serialize on each artifact; simultaneous reads have independent cursors.
/// Cloning shares pack handles and offset tables, while copying the bounded store topology.
/// No decoded-object or negative cache is retained. [`PackLimits`] bounds aggregate opening
/// work, index memory and handles; [`ReadLimits`] bounds each packed decode. Concurrent reads
/// and caller-owned results multiply memory use. Cancellation is available through
/// [`crate::Repository::objects_controlled`] and [`Self::read_controlled`].
///
/// Within each store, loose objects take precedence and are read fresh on each call. Corruption
/// never falls through to a duplicate packed copy. Packs remain readable after Git repacks/deletes
/// the original files where the OS permits unlinking open files; reopen to discover new packs.
/// Artifacts must remain immutable while readers use them: pinned handles do not defend against
/// in-place writes. Opening is not an atomic pair or topology snapshot. Exclude writers when a
/// consistent view is required. Refresh and concurrent publication contracts remain deferred. The
/// object directory and its ancestors must be trusted, as with [`LooseObjects`]. This is not a
/// snapshot of loose files or repository references.
///
/// REF_DELTA bases must be indexed in the same pack. Thin packs and cross-pack/loose bases return
/// [`ObjectReadError::MissingBase`], even if the base exists elsewhere. Traversal is iterative,
/// detects cycles, and enforces [`ReadLimits`]. Multi-pack indexes and bitmap/reverse indexes are
/// ignored; ordinary pack indexes remain required. Indexes are publication markers: unindexed packs
/// are ignored, while an index without its pack is an opening error. A publisher must finish the
/// pack before publishing its index. History queries use the repository handle's immutable shallow
/// boundaries; raw reads preserve commit parents. Reopen the repository or refresh its shallow
/// snapshot, then create a new reader after Git deepening. Loose objects, packs and boundaries are
/// not captured in one filesystem transaction; exclude concurrent depth changes while opening.
/// Local alternate stores use the same format and aggregate pack limits; see
/// [`crate::Repository::objects_with_alternates`] for topology and precedence. Complete
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
/// ```no_run
/// use girt::{ObjectId, PackLimits, ReadLimits, Repository};
/// let repository = Repository::open("/path/to/repository")?;
/// let objects = repository.objects(PackLimits::default())?;
/// let id: ObjectId = "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".parse()?;
/// if let Some(object) = objects.read(id, ReadLimits::default())? {
///     println!("{}: {} bytes", object.kind().as_str(), object.data().len());
/// }
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
pub struct Objects {
    format: ObjectFormat,
    stores: Vec<Store>,
    pub(crate) shallow: crate::ShallowRoots,
}

#[derive(Debug, Clone)]
struct Store {
    loose: LooseObjects,
    packs: Vec<Arc<FilePack>>,
}

impl Objects {
    /// Fixed history boundaries inherited from the repository handle, unaffected by later refresh.
    pub fn shallow_roots(&self) -> &crate::ShallowRoots {
        &self.shallow
    }

    /// Format shared by all objects in this store.
    pub fn object_format(&self) -> ObjectFormat {
        self.format
    }

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

    pub(crate) fn open_controlled(
        format: ObjectFormat,
        directory: &Path,
        limits: PackLimits,
        alternates: AlternateLimits,
        cancelled: &AtomicBool,
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
            let mut budget = limits;
            let mut stores = Vec::new();
            for directory in directories {
                stores.push(Store::open(format, &directory, &mut budget, cancelled)?);
            }
            Ok(Self {
                format,
                stores,
                shallow: crate::ShallowRoots::empty(format),
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

    /// Reads an exact object by full identity in this store's format; returns `None` only when
    /// absent.
    ///
    /// The payload is uninterpreted. Parse it separately if structured fields are needed.
    /// A missing loose path falls through to the snapshot's indexed packs in filename order.
    ///
    /// # Errors
    ///
    /// Returns storage errors for corrupt/unsupported objects, I/O failures, missing delta bases,
    /// cycles, incompatible identity formats, or exhausted resource limits. An invalid loose object
    /// or indexed candidate fails immediately rather than being treated as absent. No
    /// filesystem changes occur.
    pub fn read(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<Object>, ObjectReadError> {
        self.read_controlled(id, limits, &AtomicBool::new(false))
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
            for store in &self.stores {
                check_cancelled(cancelled)?;
                match store.loose.read_raw(id, loose_limit) {
                    Ok(object) => {
                        check_cancelled(cancelled)?;
                        return Ok(Some(object));
                    }
                    Err(crate::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                for pack in &store.packs {
                    check_cancelled(cancelled)?;
                    if let Some(position) = pack.find(id)? {
                        let object = pack.read(position, limits, cancelled)?;
                        check_cancelled(cancelled)?;
                        return Ok(Some(object));
                    }
                }
            }
            check_cancelled(cancelled)?;
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

impl Store {
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
                    loose,
                    packs: vec![],
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
            if path.extension().is_some_and(|extension| extension == "idx") {
                budget.max_packs = budget
                    .max_packs
                    .checked_sub(1)
                    .ok_or(ObjectReadError::Limit("pack count"))?;
                paths.push(path);
            }
        }
        paths.sort();
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
        Ok(Self { loose, packs })
    }
}

/// Failures opening or reading a repository object store; absence is `Ok(None)`.
#[derive(Debug, thiserror::Error)]
pub enum ObjectReadError {
    /// The caller requested cancellation; no partial object is returned.
    #[error("object storage cancelled")]
    Cancelled,
    /// An alternates record cannot be interpreted as a supported native path.
    #[error("invalid alternate path in {path}: {reason}")]
    Alternate {
        /// Alternates metadata file containing the record.
        path: PathBuf,
        /// Structural or platform restriction.
        reason: &'static str,
    },
    /// A recognized storage feature is outside the implemented subset.
    #[error("unsupported object storage: {0}")]
    Unsupported(&'static str),
    /// Filesystem access failed at this artifact or directory.
    #[error("object storage at {path}: {source}")]
    Path {
        /// Artifact or directory being accessed.
        path: PathBuf,
        /// Concrete filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Validation of an index/pack pair failed during snapshot opening.
    #[error("object storage pair {index} / {pack}: {source}")]
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
    #[error("loose object: {0}")]
    Loose(#[from] crate::Error),
    /// The index version is neither supported headerless v1 nor headered v2.
    #[error("unsupported pack index version {0}")]
    IndexVersion(u32),
    /// The pack version is neither 2 nor 3.
    #[error("unsupported pack version {0}")]
    PackVersion(u32),
    /// Reserved or unknown packed object type.
    #[error("unsupported packed object type {0}")]
    ObjectType(u8),
    /// Structural validation, zlib completion, a checksum, or object identity failed.
    #[error("corrupt packed storage: {0}")]
    Corrupt(&'static str),
    /// A named resource bound was exhausted; no partial object is returned.
    #[error("object storage limit exceeded: {0}")]
    Limit(&'static str),
    /// REF_DELTA base is absent from its pack; external bases are unsupported.
    #[error("missing or external delta base {0}")]
    MissingBase(ObjectId),
    /// A delta refers back to an object already in its reconstruction chain.
    #[error("cyclic delta chain")]
    DeltaCycle,
}
