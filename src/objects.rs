use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::pack::Pack;
use crate::{LooseObjects, ObjectFormat, ObjectId, ObjectKind};

/// An owned, identity-verified object kind and exact uncompressed payload.
///
/// Storage framing is validated; tree, commit, and tag payload syntax is not. Pass [`Self::data`]
/// to [`crate::Tree::parse`], [`crate::Commit::parse`], or [`crate::Tag::parse`] as appropriate.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Object {
    pub(crate) kind: ObjectKind,
    pub(crate) data: Vec<u8>,
}

impl Object {
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
        ObjectId::for_object(self.kind.as_str(), &self.data)
    }
}

/// Bounds the pack snapshot retained by [`crate::Repository::objects`].
///
/// Index-derived tables require additional memory proportional to index bytes. These are input
/// bounds, not a total heap limit. Zero values permit no indexed packs. Unindexed files are
/// ignored.
#[derive(Debug, Clone, Copy)]
pub struct PackLimits {
    /// Maximum sum of `.idx` and `.pack` file bytes (default 512 MiB).
    pub max_bytes: usize,
    /// Maximum number of index/pack pairs (default 256).
    pub max_packs: usize,
}

impl Default for PackLimits {
    fn default() -> Self {
        Self {
            max_bytes: 512 * 1024 * 1024,
            max_packs: 256,
        }
    }
}

/// Per-read bounds for loose payloads and iterative pack reconstruction.
///
/// Every base and reconstructed payload must fit `max_object_bytes`. Delta programs have their
/// own limit. `max_decode_bytes` charges every inflated program/base and every reconstructed
/// result, including intermediate results, before allocating its buffer. It therefore also bounds
/// retained decoding bytes, but excludes pack snapshots, allocator overhead, traversal bookkeeping,
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
        }
    }
}

/// Reads loose objects and an immutable, validated snapshot of local packs.
///
/// Obtain this synchronous, blocking reader through [`crate::Repository::objects`]. Opening loads
/// all `.idx`/`.pack` pairs in filename order and verifies index v2 structure, SHA-1 checksums,
/// pack v2 headers/counts, offset ranges, and entry CRCs. Pack v3 and index v1 are explicitly
/// unsupported. Entry framing, zlib streams, delta programs, and object identities are checked on
/// reads, including every base and intermediate delta; opening is not a full pack fsck.
///
/// Loose objects take precedence and are read fresh on each call. Corruption never falls through
/// to a duplicate packed copy. Packs remain readable after Git repacks/deletes the original files;
/// reopen the reader to discover new packs. Concurrent repacking during opening can cause an I/O
/// error: retry by opening a new reader. The object directory and its ancestors must be trusted,
/// as with [`LooseObjects`]. This is not a snapshot of loose files or repository references.
///
/// REF_DELTA bases must be indexed in the same pack. Thin packs and cross-pack/loose bases return
/// [`ObjectReadError::MissingBase`], even if the base exists elsewhere. Traversal is iterative,
/// detects cycles, and enforces [`ReadLimits`]. Multi-pack indexes and bitmap/reverse indexes are
/// ignored; ordinary pack indexes remain required. Indexes are publication markers: unindexed packs
/// are ignored, while an index without its pack is an opening error. A publisher must finish the
/// pack before publishing its index. Alternates and partial/shallow repositories are outside the
/// repository opener's supported scope. Loose writes use [`LooseObjects`]; validated received-pack
/// installation uses [`crate::fetch::ReceivedFetch::install`].
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
#[derive(Debug)]
pub struct Objects {
    loose: LooseObjects,
    packs: Vec<Pack>,
}

impl Objects {
    pub(crate) fn open(directory: &Path, limits: PackLimits) -> Result<Self, ObjectReadError> {
        let loose = LooseObjects::new(directory, ObjectFormat::Sha1)?;
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
            let path = entry
                .map_err(|source| ObjectReadError::Path {
                    path: pack_directory.clone(),
                    source,
                })?
                .path();
            if path.extension().is_some_and(|extension| extension == "idx") {
                if paths.len() == limits.max_packs {
                    return Err(ObjectReadError::Limit("pack count"));
                }
                paths.push(path);
            }
        }
        paths.sort();
        let mut remaining = limits.max_bytes;
        let mut packs = Vec::new();
        for path in paths {
            let index = read_bounded(&path, &mut remaining)?;
            let data = read_bounded(&path.with_extension("pack"), &mut remaining)?;
            packs.push(Pack::open(&index, data).map_err(|source| {
                ObjectReadError::PackArtifacts {
                    pack: path.with_extension("pack"),
                    index: path,
                    source: Box::new(source),
                }
            })?);
        }
        Ok(Self { loose, packs })
    }

    /// Reads an exact object by full SHA-1 identity; returns `None` only when absent.
    ///
    /// The payload is uninterpreted. Parse it separately if structured fields are needed.
    /// A missing loose path falls through to the snapshot's indexed packs in filename order.
    ///
    /// # Errors
    ///
    /// Returns storage errors for corrupt/unsupported objects, I/O failures, missing delta bases,
    /// cycles, or exhausted resource limits. An invalid loose object or indexed candidate fails
    /// immediately rather than being treated as absent. No filesystem changes occur.
    pub fn read(
        &self,
        id: ObjectId,
        limits: ReadLimits,
    ) -> Result<Option<Object>, ObjectReadError> {
        let loose_limit = limits.max_object_bytes.min(limits.max_decode_bytes);
        match self.loose.read_raw(id, loose_limit) {
            Ok(object) => return Ok(Some(object)),
            Err(crate::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        for pack in &self.packs {
            if let Some(position) = pack.find(id) {
                return pack.read(position, limits).map(Some);
            }
        }
        Ok(None)
    }
}

fn read_bounded(path: &Path, remaining: &mut usize) -> Result<Vec<u8>, ObjectReadError> {
    let at_path = |source| ObjectReadError::Path {
        path: path.to_owned(),
        source,
    };
    let file = File::open(path).map_err(at_path)?;
    if file.metadata().map_err(at_path)?.len() > *remaining as u64 {
        return Err(ObjectReadError::Limit("pack snapshot bytes"));
    }
    let mut bytes = Vec::new();
    file.take((*remaining as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(at_path)?;
    if bytes.len() > *remaining {
        return Err(ObjectReadError::Limit("pack snapshot bytes"));
    }
    *remaining -= bytes.len();
    Ok(bytes)
}

/// Failures opening or reading a repository object store; absence is `Ok(None)`.
#[derive(Debug, thiserror::Error)]
pub enum ObjectReadError {
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
    /// The index version is not 2 (headerless legacy indexes report version 1).
    #[error("unsupported pack index version {0}")]
    IndexVersion(u32),
    /// The pack version is not 2.
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
