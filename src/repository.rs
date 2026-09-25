//! Repository location, opening, and creation.
mod colocation;
mod discover;
mod init;
mod operation;
mod shallow;
mod worktrees;
use std::path::{Path, PathBuf};
use std::{fs, io};

pub use colocation::{ColocationEdit, ColocationError};
pub use init::{InitError, InitKind};
pub(crate) use init::{initial_branch, initial_config};
pub use operation::{OperationCleanupError, OperationError, OperationLimits, OperationState};
pub use shallow::{ShallowError, ShallowRoots};
use thiserror::Error;
pub use worktrees::{Worktree, WorktreeError, WorktreeState};

use crate::config::{ConfigFile, ConfigInputs, ConfigScope, ResolveError, integer};
use crate::{Config, ConfigError, LooseObjects, ObjectFormat};

/// An opened repository's metadata paths, checkout location and resolved snapshots.
///
/// Opening accepts a worktree root, Git directory, or `gitdir:` file. It never searches parents,
/// initializes files, runs Git, or reads environment overrides or system/global configuration.
/// Relative input paths resolve against the process current directory. Symlinks are resolved;
/// opening is not a security boundary against concurrent filesystem changes or untrusted paths.
///
/// Ordinary, bare, separate-Git-directory and linked-worktree layouts are supported. Repository
/// format versions 0 and 1 with SHA-1 objects, and version 1 with SHA-256 objects are
/// supported, including shallow roots and relative linked-worktree paths. Includes and enabled
/// worktree configuration are resolved. Local alternates, `noop`, `preciousObjects`, and
/// `partialClone` are recognized. Unknown extensions remain explicit errors. Precious-object
/// metadata is retained in [`Self::config`]; no pruning or lazy fetching is performed.
/// Object access uses [`Self::loose_objects`] for loose reads/writes or [`Self::objects`] for
/// bounded loose/packed reads. Opening repository metadata alone does not validate object storage.
///
/// Paths preserve OS bytes on Unix. On other platforms metadata paths must be UTF-8. No tilde,
/// environment-variable or prefix interpolation is performed. Tilde and `%(...)` prefixes in
/// core.worktree are rejected; dollar signs are literal path bytes.
///
/// Read an existing loose blob by its full identity (no reference lookup):
///
/// ```no_run
/// use girt::{ObjectId, Repository};
/// let repository = Repository::open("/path/to/repository")?;
/// let id: ObjectId = "ce013625030ba8dba906f756967f9e9ca394464a".parse()?;
/// let bytes = repository.loose_objects().read_blob(id, 1024)?;
/// assert_eq!(bytes, b"hello\n");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Repository {
    git_dir: PathBuf,
    common_dir: PathBuf,
    object_dir: PathBuf,
    worktree: Option<PathBuf>,
    bare: bool,
    config: Config,
    format_version: u32,
    object_format: ObjectFormat,
    shallow: ShallowRoots,
}

/// Repository location, metadata, configuration or supported-format failure.
#[derive(Debug, Error)]
pub enum OpenError {
    /// The opened checkout belongs to a different common repository.
    #[error("unrelated common repository: {0}")]
    Unrelated(PathBuf),
    /// Shallow-root metadata could not be read.
    #[error(transparent)]
    Shallow(#[from] ShallowError),
    /// The explicit path does not identify a repository.
    #[error("no repository at {0}")]
    NotFound(PathBuf),
    /// Filesystem access failed; the path and original cause are retained.
    #[error("cannot read {path}: {source}")]
    Io {
        /// Failed path.
        path: PathBuf,
        /// Original operating-system error.
        #[source]
        source: io::Error,
    },
    /// Repository metadata contradicts the supported layout or has invalid content.
    #[error("malformed repository metadata at {path}: {reason}")]
    Malformed {
        /// Metadata path.
        path: PathBuf,
        /// Failure explanation.
        reason: String,
    },
    /// Configuration syntax is invalid or unsupported.
    #[error("invalid configuration at {path}: {source}")]
    Config {
        /// Configuration source path.
        path: PathBuf,
        /// Parser failure including physical line.
        #[source]
        source: ConfigError,
    },
    /// Effective configuration could not be resolved.
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    /// A recognized feature cannot be interpreted by this implementation.
    #[error("unsupported repository feature at {path}: {feature}")]
    Unsupported {
        /// Source of the unsupported feature.
        path: PathBuf,
        /// Feature or setting requiring support.
        feature: String,
    },
}

impl Repository {
    /// Borrows this repository's files-backend reference store.
    ///
    /// Uses the detected common/worktree directories. See [`crate::refs::References`] for the
    /// byte-name, platform, read and explicit no-reflog update boundaries.
    ///
    /// # Errors
    ///
    /// Reference storage supports Unix and Windows local filesystems. Repository
    /// backends such as reftable are rejected by [`Self::open`] before a handle can be constructed.
    pub fn references(&self) -> Result<crate::refs::References<'_>, crate::refs::ReferenceError> {
        crate::refs::References::new(self)
    }

    /// Opens only the supplied location, without changing files.
    ///
    /// Reads `config` from the common directory; repeated scalar settings use the last value.
    /// Missing config uses version 0, SHA-1, and layout-based worktree inference. `core.bare` and
    /// `core.worktree` override ordinary layout inference; relative core.worktree is relative to
    /// the Git directory. A linked worktree uses its `gitdir` backlink, verified against its
    /// `.git` file when opening through a checkout. Opening private metadata does not require an
    /// accessible checkout; use [`Self::worktrees`] to inspect backlink availability. Backlinks
    /// may be absolute or relative to the private Git directory. Without worktreeConfig, linked
    /// layouts ignore shared core.bare/worktree. With it, direct common settings followed by
    /// direct private settings determine the checkout. Includes participate only in the
    /// effective snapshot. Unknown extension keys are rejected even in version 0.
    ///
    /// # Errors
    ///
    /// Distinguishes missing locations, malformed metadata, I/O failures, configuration failures,
    /// and unsupported configuration or storage features. No files are written
    /// on success or failure. Filesystem reads and allocation are synchronous and unbounded by a
    /// caller-supplied resource limit except configuration and shallow metadata. The default
    /// shallow snapshot is limited to 16 MiB.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OpenError> {
        Self::open_with_config(path, &ConfigInputs::default())
    }

    /// Opens a repository with explicit inherited sources, environment pairs and caller overrides.
    ///
    /// Adds common-directory local configuration and, when enabled in that file, per-worktree
    /// configuration to the supplied inputs. Repository paths and HEAD provide include context;
    /// caller Git-directory aliases are retained. No ambient environment is read.
    /// Format bootstrap uses only the direct common configuration; enabled direct worktree settings
    /// also participate in layout bootstrap. Includes, worktree format settings and overrides
    /// cannot change the opened object format. Reopen to refresh both
    /// metadata and effective configuration; existing handles remain snapshots.
    ///
    /// # Errors
    ///
    /// Returns the layout/format errors of [`Self::open`] or contextual configuration resolution
    /// failures. All operations are read-only. Config resolution obeys the supplied budgets;
    /// repository metadata bootstrap retains [`Self::open`]'s allocation contract.
    pub fn open_with_config(
        path: impl AsRef<Path>,
        inputs: &ConfigInputs,
    ) -> Result<Self, OpenError> {
        let input = path.as_ref();
        let metadata = match fs::metadata(input) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(OpenError::NotFound(input.into()));
            }
            Err(source) => return Err(io_error(input, source)),
        };
        let logical_input = std::path::absolute(input).map_err(|error| io_error(input, error))?;
        let logical_git_dir = if metadata.is_dir() && logical_input.join(".git").is_dir() {
            Some(logical_input.join(".git"))
        } else if metadata.is_dir() && !logical_input.join(".git").exists() {
            Some(logical_input.clone())
        } else if metadata.is_file() {
            Some(gitfile_target(&logical_input)?)
        } else if metadata.is_dir() && logical_input.join(".git").is_file() {
            Some(gitfile_target(&logical_input.join(".git"))?)
        } else {
            None
        };
        let input = canonical(input)?;
        let (git_dir, inferred_worktree) = if metadata.is_file() {
            (
                read_gitfile(&logical_input)?,
                logical_input.parent().map(canonical).transpose()?,
            )
        } else if entry_exists(&input.join(".git"))? {
            let dotgit = input.join(".git");
            let git_dir = if dotgit.is_dir() {
                canonical(&dotgit)?
            } else {
                read_gitfile(&dotgit)?
            };
            (git_dir, Some(input.clone()))
        } else {
            let parent = logical_input.parent();
            let dotgit_identity =
                parent.and_then(|parent| fs::canonicalize(parent.join(".git")).ok());
            let inferred = parent
                .filter(|_| dotgit_identity.as_deref() == Some(input.as_path()))
                .map(canonical)
                .transpose()?;
            (input.clone(), inferred)
        };
        if !exists(&git_dir.join("HEAD"))? {
            if inferred_worktree.is_some() || exists(&git_dir.join("objects"))? {
                return Err(malformed(&git_dir, "missing HEAD marker"));
            }
            return Err(OpenError::NotFound(input));
        }
        validate_head(&git_dir.join("HEAD"))?;
        let common_file = git_dir.join("commondir");
        let common_dir = if exists(&common_file)? {
            let relative = metadata_path(&common_file, &read(&common_file)?)?;
            canonical(&git_dir.join(relative))?
        } else {
            git_dir.clone()
        };
        let object_dir = common_dir.join("objects");
        require_directory(&object_dir)?;
        require_directory(&common_dir.join("refs"))?;
        let config_path = common_dir.join("config");
        let bytes = if exists(&config_path)? {
            read_config(&config_path, inputs.limits.bytes)?
        } else {
            Vec::new()
        };
        let config = Config::parse(&bytes).map_err(|source| OpenError::Config {
            path: config_path.clone(),
            source,
        })?;
        let (format_version, object_format) = validate_config(&config, &config_path)?;
        let mut inputs = inputs.clone();
        inputs.context.git_dirs.push(git_dir.clone());
        if let Some(logical_git_dir) = logical_git_dir {
            inputs.context.git_dirs.push(logical_git_dir);
        }
        let head = read(&git_dir.join("HEAD"))?;
        let head = head.strip_suffix(b"\n").unwrap_or(&head);
        let head = head.strip_suffix(b"\r").unwrap_or(head);
        inputs.context.branch = head.strip_prefix(b"ref: refs/heads/").map(<[u8]>::to_vec);
        inputs.files.push(ConfigFile {
            path: config_path.clone(),
            scope: ConfigScope::Local,
            optional: true,
        });
        let worktree_config_enabled = extension_boolean(&config, &config_path, "worktreeconfig")?;
        let mut layout_config = if git_dir != common_dir && !worktree_config_enabled {
            Config::parse(b"").expect("empty configuration")
        } else {
            config.clone()
        };
        if worktree_config_enabled {
            let worktree_path = git_dir.join("config.worktree");
            if exists(&worktree_path)? {
                let worktree_config =
                    Config::parse(&read_config(&worktree_path, inputs.limits.bytes)?).map_err(
                        |source| OpenError::Config {
                            path: worktree_path.clone(),
                            source,
                        },
                    )?;
                layout_config.append(&worktree_config);
            }
            inputs.files.push(ConfigFile {
                path: worktree_path,
                scope: ConfigScope::Worktree,
                optional: true,
            });
        }
        let worktree = resolve_worktree(
            &git_dir,
            &common_dir,
            inferred_worktree,
            &layout_config,
            &config_path,
        )?;
        let bare = boolean(&layout_config, &config_path, "bare")?.unwrap_or(worktree.is_none());
        let config = Config::resolve(&inputs)?;
        let shallow = ShallowRoots::read(
            common_dir.join("shallow"),
            object_format,
            16 * 1024 * 1024,
            &std::sync::atomic::AtomicBool::new(false),
        )?;
        Ok(Self {
            git_dir,
            common_dir,
            object_dir,
            worktree,
            bare,
            config,
            format_version,
            object_format,
            shallow,
        })
    }

    /// Immutable shallow boundaries captured when this handle was opened or refreshed.
    pub fn shallow_roots(&self) -> &ShallowRoots {
        &self.shallow
    }

    /// Atomically replaces this handle's shallow snapshot after a successful bounded read.
    ///
    /// Existing object readers retain their old boundaries. Reopen object readers after Git
    /// deepening to refresh both packs and boundaries. On failure this handle remains unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`ShallowError`] for malformed metadata, I/O, cancellation or the byte limit.
    pub fn refresh_shallow(
        &mut self,
        max_bytes: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<(), ShallowError> {
        let roots = ShallowRoots::read(
            self.common_dir.join("shallow"),
            self.object_format,
            max_bytes,
            cancel,
        )?;
        self.shallow = roots;
        Ok(())
    }

    /// Per-worktree Git directory, containing HEAD; canonical absolute path.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }
    /// Shared directory containing configuration, refs and objects; canonical absolute path.
    pub fn common_dir(&self) -> &Path {
        &self.common_dir
    }
    /// Shared loose/packed object directory. Use [`Self::objects`] to open packed reads.
    pub fn object_dir(&self) -> &Path {
        &self.object_dir
    }
    /// Known worktree root. `None` means bare or an unknown checkout location; [`Self::is_bare`]
    /// distinguishes them. Opening separate metadata alone cannot infer a checkout elsewhere.
    /// Existing accessible paths are canonical; missing/inaccessible paths retain an absolute OS
    /// spelling, possibly containing `..`.
    pub fn worktree(&self) -> Option<&Path> {
        self.worktree.as_deref()
    }
    /// Whether layout bootstrap declares a bare repository, independently of checkout availability.
    ///
    /// A nonbare separate Git directory can have an unknown [`Self::worktree`] when opened without
    /// its gitfile. Metadata and object operations remain available; checkout operations require a
    /// known root. Open through the checkout or gitfile to establish that relationship.
    pub fn is_bare(&self) -> bool {
        self.bare
    }

    /// Resolved configuration snapshot, including provenance and explicit inherited sources.
    pub fn config(&self) -> &Config {
        &self.config
    }
    /// Accepted core.repositoryformatversion (0 or 1).
    pub fn format_version(&self) -> u32 {
        self.format_version
    }
    /// Object format verified from common repository configuration while opening.
    pub fn object_format(&self) -> ObjectFormat {
        self.object_format
    }
    /// Opens a bounded snapshot of pack/index pairs alongside live loose-object reads.
    ///
    /// See [`crate::Objects`] for supported versions, validation timing, snapshot lifetime,
    /// external-base policy, and filesystem assumptions. Does not create files or directories.
    ///
    /// # Errors
    ///
    /// Returns [`crate::ObjectReadError`] for malformed or unsupported packed storage, I/O
    /// failures, or exhausted snapshot limits. Both SHA-1 and SHA-256 pack/index v2 pairs use
    /// the repository's configured format; wrong-format bytes are corruption.
    /// Reopen after a concurrent repack failure.
    pub fn objects(
        &self,
        limits: crate::PackLimits,
    ) -> Result<crate::Objects, crate::ObjectReadError> {
        self.objects_with_alternates(limits, crate::AlternateLimits::default())
    }

    /// Opens object storage with aggregate pack and alternate-discovery bounds.
    ///
    /// Uses primary-first, depth-first alternates-file order; each store searches loose objects
    /// before filename-ordered packs. Canonical aliases/cycles are visited once. Missing alternate
    /// directories are skipped. Metadata and packs form a fixed snapshot; loose files remain live.
    /// Reopen to observe topology changes. Concurrent edits can yield an I/O or metadata error;
    /// callers needing a consistent graph must exclude writers. No borrowed store is modified.
    /// `GIT_ALTERNATE_OBJECT_DIRECTORIES` and `GIT_OBJECT_DIRECTORY` are ignored, as with opening.
    /// HTTP alternates metadata is inert. Missing promised objects return absence without fetching.
    ///
    /// # Errors
    ///
    /// Returns storage errors for malformed paths, inaccessible directories, corrupt packs or
    /// exceeded aggregate limits. A corrupt duplicate fails at the first searched candidate.
    pub fn objects_with_alternates(
        &self,
        packs: crate::PackLimits,
        alternates: crate::AlternateLimits,
    ) -> Result<crate::Objects, crate::ObjectReadError> {
        let mut objects =
            crate::Objects::open(self.object_format, &self.object_dir, packs, alternates)?;
        objects.shallow = self.shallow.clone();
        Ok(objects)
    }

    /// Opens bounded file-backed storage with cooperative cancellation.
    ///
    /// Checks cancellation around alternate discovery and bounded index parsing, between directory
    /// entries, and every 64 KiB during pack validation. Individual filesystem calls cannot be
    /// interrupted. Run the whole operation on a caller-owned blocking worker when needed.
    /// On failure all opened artifacts are closed and no files are changed.
    ///
    /// # Errors
    ///
    /// Returns the errors from [`Self::objects_with_alternates`] or
    /// [`crate::ObjectReadError::Cancelled`].
    pub fn objects_controlled(
        &self,
        packs: crate::PackLimits,
        alternates: crate::AlternateLimits,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<crate::Objects, crate::ObjectReadError> {
        let mut objects = crate::Objects::open_controlled(
            self.object_format,
            &self.object_dir,
            packs,
            alternates,
            cancelled,
        )?;
        objects.shallow = self.shallow.clone();
        Ok(objects)
    }

    /// Connects to the existing loose-object API without creating directories or files.
    ///
    /// Both openable formats support loose storage. Subsequent reads/writes retain
    /// [`LooseObjects`]' format checks and storage contracts.
    pub fn loose_objects(&self) -> LooseObjects {
        LooseObjects::new(&self.object_dir, self.object_format())
    }
}

fn read_config(path: &Path, max_bytes: usize) -> Result<Vec<u8>, OpenError> {
    use std::io::Read;
    let file = fs::File::open(path).map_err(|error| io_error(path, error))?;
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error(path, error))?;
    if bytes.len() > max_bytes {
        return Err(ResolveError {
            location: crate::config::SourceLocation {
                path: Some(path.into()),
                line: 1,
            },
            included_from: Vec::new(),
            source: crate::config::ResolveFailure::Limit("bootstrap configuration bytes"),
        }
        .into());
    }
    Ok(bytes)
}

fn validate_config(config: &Config, path: &Path) -> Result<(u32, ObjectFormat), OpenError> {
    for entry in config.entries() {
        if entry.section.eq_ignore_ascii_case(b"extensions")
            && (entry.subsection.is_some()
                || !entry.name.eq_ignore_ascii_case(b"objectformat")
                    && !entry.name.eq_ignore_ascii_case(b"worktreeconfig")
                    && !entry.name.eq_ignore_ascii_case(b"relativeworktrees")
                    && !entry.name.eq_ignore_ascii_case(b"noop")
                    && !entry.name.eq_ignore_ascii_case(b"preciousobjects")
                    && !entry.name.eq_ignore_ascii_case(b"partialclone"))
        {
            return Err(unsupported(
                path,
                &format!(
                    "extensions.{}",
                    String::from_utf8_lossy(&entry.name.to_ascii_lowercase())
                ),
            ));
        }
    }
    let version = match config.value("core", None, "repositoryformatversion") {
        None => 0,
        Some(Some(value)) => parse_version(value)
            .ok_or_else(|| malformed(path, "invalid core.repositoryformatversion"))?,
        Some(None) => return Err(malformed(path, "implicit core.repositoryformatversion")),
    };
    if version > 1 {
        return Err(unsupported(
            path,
            &format!("repository format version {version}"),
        ));
    }
    extension_boolean(config, path, "relativeworktrees")?;
    extension_boolean(config, path, "preciousobjects")?;
    if config.value("extensions", None, "partialclone") == Some(None) {
        return Err(malformed(path, "implicit extensions.partialClone"));
    }
    let mut object_format = ObjectFormat::Sha1;
    if let Some(value) = config.value("extensions", None, "objectformat") {
        if version == 0 {
            return Err(unsupported(
                path,
                "extensions.objectFormat requires repository version 1",
            ));
        }
        match value {
            Some(b"sha1") => (),
            Some(b"sha256") => object_format = ObjectFormat::Sha256,
            _ => return Err(unsupported(path, "unknown extensions.objectFormat")),
        }
    }
    Ok((version, object_format))
}

fn extension_boolean(config: &Config, path: &Path, name: &str) -> Result<bool, OpenError> {
    match config.value("extensions", None, name) {
        None => Ok(false),
        Some(None) => Ok(true),
        Some(Some(value)) => match value.to_ascii_lowercase().as_slice() {
            b"true" | b"yes" | b"on" => Ok(true),
            b"false" | b"no" | b"off" | b"" => Ok(false),
            _ => integer(value)
                .map(|n| n != 0)
                .ok_or_else(|| malformed(path, "invalid extension boolean")),
        },
    }
}

fn parse_version(value: &[u8]) -> Option<u32> {
    u32::try_from(integer(value)?).ok()
}

fn boolean(config: &Config, path: &Path, name: &str) -> Result<Option<bool>, OpenError> {
    match config.value("core", None, name) {
        None => Ok(None),
        Some(None) => Ok(Some(true)),
        Some(Some(bytes)) => {
            let value = bytes.to_ascii_lowercase();
            match value.as_slice() {
                b"true" | b"yes" | b"on" => Ok(Some(true)),
                b"" | b"false" | b"no" | b"off" => Ok(Some(false)),
                _ => {
                    let number = integer(bytes);
                    number
                        .map(|value| Some(value != 0))
                        .ok_or_else(|| malformed(path, &format!("invalid core.{name} boolean")))
                }
            }
        }
    }
}

fn resolve_worktree(
    git_dir: &Path,
    common_dir: &Path,
    inferred: Option<PathBuf>,
    config: &Config,
    source: &Path,
) -> Result<Option<PathBuf>, OpenError> {
    let bare = boolean(config, source, "bare")?;
    let configured = config.value("core", None, "worktree");
    if git_dir != common_dir && configured.is_none() && bare != Some(true) {
        let backlink = git_dir.join("gitdir");
        let target = git_dir.join(metadata_path(&backlink, &read(&backlink)?)?);
        let root = target
            .parent()
            .ok_or_else(|| malformed(&backlink, "backlink has no parent"))?;
        // Metadata remains useful when a registered checkout has disappeared. Validate a live
        // backlink separately; enumeration retains its failure without losing this Git directory.
        if let Some(inferred) = inferred {
            if canonical(root)? != inferred || read_gitfile(&target)? != git_dir {
                return Err(malformed(&backlink, "worktree and backlink disagree"));
            }
            return Ok(Some(inferred));
        }
        return Ok(Some(available_path(root)?));
    }
    if let Some(value) = configured {
        if bare == Some(true) {
            return Err(malformed(source, "core.bare and core.worktree conflict"));
        }
        let value = value
            .filter(|value| !value.is_empty())
            .ok_or_else(|| malformed(source, "empty core.worktree"))?;
        if value.starts_with(b"~") || value.starts_with(b"%(") {
            return Err(unsupported(source, "core.worktree path interpolation"));
        }
        return Ok(Some(available_path(
            &git_dir.join(path_bytes(source, value)?),
        )?));
    }
    if bare == Some(true) {
        return Ok(None);
    }
    if let Some(root) = inferred {
        return Ok(Some(root));
    }
    Ok(None)
}

fn validate_head(path: &Path) -> Result<(), OpenError> {
    let bytes = read(path)?;
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    let symbolic = bytes.strip_prefix(b"ref: ").is_some_and(|name| {
        name.starts_with(b"refs/") && !name.iter().any(|b| b.is_ascii_whitespace() || *b == 0)
    });
    let detached = matches!(bytes.len(), 40 | 64) && bytes.iter().all(u8::is_ascii_hexdigit);
    if !symbolic && !detached {
        return Err(malformed(
            path,
            "invalid HEAD marker (reference resolution is not performed)",
        ));
    }
    Ok(())
}

fn read_gitfile(path: &Path) -> Result<PathBuf, OpenError> {
    canonical(&gitfile_target(path)?)
}
fn gitfile_target(path: &Path) -> Result<PathBuf, OpenError> {
    let bytes = read(path)?;
    let value = bytes
        .strip_prefix(b"gitdir: ")
        .ok_or_else(|| malformed(path, "expected gitdir: indirection"))?;
    let target = metadata_path(path, value)?;
    Ok(path.parent().unwrap_or(Path::new(".")).join(target))
}
fn metadata_path(source: &Path, bytes: &[u8]) -> Result<PathBuf, OpenError> {
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    if bytes.is_empty() || bytes.contains(&b'\n') || bytes.contains(&0) {
        return Err(malformed(source, "empty or multiline path"));
    }
    path_bytes(source, bytes)
}
fn path_bytes(source: &Path, bytes: &[u8]) -> Result<PathBuf, OpenError> {
    if bytes.contains(&0) {
        return Err(malformed(source, "NUL in path"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| unsupported(source, "non-UTF-8 path"))
    }
}
fn entry_exists(path: &Path) -> Result<bool, OpenError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}
fn exists(path: &Path) -> Result<bool, OpenError> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(path, source)),
    }
}
fn read(path: &Path) -> Result<Vec<u8>, OpenError> {
    fs::read(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            malformed(path, "missing required metadata")
        } else {
            io_error(path, source)
        }
    })
}
fn canonical(path: &Path) -> Result<PathBuf, OpenError> {
    fs::canonicalize(path).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            malformed(path, "metadata path does not exist")
        } else {
            io_error(path, source)
        }
    })
}
fn io_error(path: &Path, source: io::Error) -> OpenError {
    OpenError::Io {
        path: path.into(),
        source,
    }
}
fn malformed(path: &Path, reason: &str) -> OpenError {
    OpenError::Malformed {
        path: path.into(),
        reason: reason.into(),
    }
}
fn unsupported(path: &Path, feature: &str) -> OpenError {
    OpenError::Unsupported {
        path: path.into(),
        feature: feature.into(),
    }
}

// Preserve an absolute OS path for missing/inaccessible checkouts; metadata opening is independent
// of checkout availability. Canonicalize live paths to compare aliases without lexical guesses.
fn available_path(path: &Path) -> Result<PathBuf, OpenError> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(path),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            std::path::absolute(path).map_err(|error| io_error(path, error))
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn require_directory(path: &Path) -> Result<(), OpenError> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(malformed(path, "expected directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Err(malformed(path, "missing required directory"))
        }
        Err(error) => Err(io_error(path, error)),
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    #[rstest]
    #[case::zero(b"0", Some(0))]
    #[case::plus(b"+1", Some(1))]
    #[case::suffix(b"1k", Some(1024))]
    #[case::invalid(b"no", None)]
    fn versions(#[case] bytes: &[u8], #[case] expected: Option<u32>) {
        assert_eq!(parse_version(bytes), expected);
    }
    #[rstest]
    #[case::octal(b"010", Some(8))]
    #[case::hex(b"0x1", Some(1))]
    #[case::negative(b"-1", Some(-1))]
    #[case::invalid_octal(b"08", None)]
    #[case::overflow(b"99999999999999999999", None)]
    fn git_integers(#[case] bytes: &[u8], #[case] expected: Option<i64>) {
        assert_eq!(integer(bytes), expected);
    }

    #[rstest]
    #[case::false_value(b"[core]\nbare=off\n", Some(false))]
    #[case::implicit(b"[core]\nbare\n", Some(true))]
    #[case::absent(b"[core]\n", None)]
    fn bare_values(#[case] bytes: &[u8], #[case] expected: Option<bool>) {
        let config = Config::parse(bytes).unwrap();
        assert_eq!(
            boolean(&config, Path::new("config"), "bare").unwrap(),
            expected
        );
    }

    #[cfg(unix)]
    #[test]
    fn metadata_paths_preserve_bytes() {
        use std::os::unix::ffi::OsStrExt;
        let path = path_bytes(Path::new("config"), b"work-\xff").unwrap();
        assert_eq!(path.as_os_str().as_bytes(), b"work-\xff");
    }

    #[test]
    fn missing_path_is_not_created() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("absent");
        assert!(matches!(
            Repository::open(&path),
            Err(OpenError::NotFound(_))
        ));
        assert!(!path.exists());
    }
}
