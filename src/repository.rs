//! Repository location, opening, and creation.
mod discover;
mod init;

use std::path::{Path, PathBuf};
use std::{fs, io};

pub use init::{InitError, InitKind};
pub(crate) use init::{initial_branch, initial_config};
use thiserror::Error;

use crate::{Config, ConfigError, LooseObjects, ObjectFormat};

/// An opened repository's canonical paths and local configuration snapshot.
///
/// Opening accepts a worktree root, Git directory, or `gitdir:` file. It never searches parents,
/// initializes files, runs Git, or reads environment overrides or system/global configuration.
/// Relative input paths resolve against the process current directory. Symlinks are resolved;
/// opening is not a security boundary against concurrent filesystem changes or untrusted paths.
///
/// Ordinary, bare, separate-Git-directory and linked-worktree layouts are supported. Repository
/// format versions 0 and 1 with SHA-1 objects, and version 1 with SHA-256 objects are
/// supported. Includes, worktree configuration, alternates, shallow repositories and other
/// extensions are rejected explicitly. Object access uses [`Self::loose_objects`] for loose
/// reads/writes or [`Self::objects`] for bounded loose/packed reads. Opening repository metadata
/// alone does not validate object storage.
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
    config: Config,
    format_version: u32,
    object_format: ObjectFormat,
}

/// Repository location, metadata, configuration or supported-format failure.
#[derive(Debug, Error)]
pub enum OpenError {
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
    /// Non-Unix reference storage is explicitly unsupported. Repository
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
    /// `.git` file. Shared core.worktree is rejected in linked layouts because this slice does
    /// not implement worktree-specific configuration. Unknown extension keys are rejected even
    /// in version 0.
    ///
    /// # Errors
    ///
    /// Distinguishes missing locations, malformed metadata, I/O failures, configuration failures,
    /// and unsupported configuration or storage features. No files are written
    /// on success or failure. Filesystem reads and allocation are synchronous and unbounded by a
    /// caller-supplied resource limit.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, OpenError> {
        let input = path.as_ref();
        let metadata = match fs::metadata(input) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(OpenError::NotFound(input.into()));
            }
            Err(source) => return Err(io_error(input, source)),
        };
        let input = canonical(input)?;
        let (git_dir, inferred_worktree) = if metadata.is_file() {
            (read_gitfile(&input)?, input.parent().map(Path::to_path_buf))
        } else if entry_exists(&input.join(".git"))? {
            let dotgit = input.join(".git");
            let git_dir = if dotgit.is_dir() {
                canonical(&dotgit)?
            } else {
                read_gitfile(&dotgit)?
            };
            (git_dir, Some(input.clone()))
        } else {
            let inferred = (input.file_name().is_some_and(|name| name == ".git"))
                .then(|| input.parent().unwrap().to_path_buf());
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
        if !object_dir.is_dir() || !common_dir.join("refs").is_dir() {
            return Err(malformed(&common_dir, "missing objects or refs directory"));
        }
        let config_path = common_dir.join("config");
        let bytes = if exists(&config_path)? {
            read(&config_path)?
        } else {
            Vec::new()
        };
        let config = Config::parse(&bytes).map_err(|source| OpenError::Config {
            path: config_path.clone(),
            source,
        })?;
        let (format_version, object_format) = validate_config(&config, &config_path)?;
        reject_storage_features(&common_dir, &object_dir)?;
        let worktree = resolve_worktree(
            &git_dir,
            &common_dir,
            inferred_worktree,
            &config,
            &config_path,
        )?;
        Ok(Self {
            git_dir,
            common_dir,
            object_dir,
            worktree,
            config,
            format_version,
            object_format,
        })
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
    /// Canonical worktree root, or `None` for a bare repository.
    pub fn worktree(&self) -> Option<&Path> {
        self.worktree.as_deref()
    }
    /// Parsed common-directory config snapshot; no ambient sources are merged.
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
        crate::Objects::open(self.object_format, &self.object_dir, limits)
    }

    /// Connects to the existing loose-object API without creating directories or files.
    ///
    /// Both openable formats support loose storage. Subsequent reads/writes retain
    /// [`LooseObjects`]' format checks and storage contracts.
    pub fn loose_objects(&self) -> LooseObjects {
        LooseObjects::new(&self.object_dir, self.object_format())
    }
}

fn validate_config(config: &Config, path: &Path) -> Result<(u32, ObjectFormat), OpenError> {
    for entry in config.entries() {
        if entry.section == b"include" || entry.section == b"includeif" {
            return Err(unsupported(path, "configuration includes"));
        }
        if entry.section == b"extensions"
            && (entry.subsection.is_some() || entry.name != b"objectformat")
        {
            return Err(unsupported(
                path,
                &format!("extensions.{}", String::from_utf8_lossy(&entry.name)),
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

fn parse_version(value: &[u8]) -> Option<u32> {
    u32::try_from(integer(value)?).ok()
}

// Git integer syntax has C-style bases and binary k/m/g multipliers.
fn integer(value: &[u8]) -> Option<i64> {
    let text = std::str::from_utf8(value).ok()?;
    let (text, multiplier) = match text.as_bytes().last()? {
        b'k' | b'K' => (&text[..text.len() - 1], 1024i64),
        b'm' | b'M' => (&text[..text.len() - 1], 1024i64.pow(2)),
        b'g' | b'G' => (&text[..text.len() - 1], 1024i64.pow(3)),
        _ => (text, 1),
    };
    let (digits, sign) = if let Some(rest) = text.strip_prefix('-') {
        (rest, -1)
    } else {
        (text.strip_prefix('+').unwrap_or(text), 1)
    };
    let (digits, radix) = if let Some(rest) = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        (rest, 16)
    } else if digits.starts_with('0') {
        (digits, 8)
    } else {
        (digits, 10)
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    i64::from_str_radix(digits, radix)
        .ok()?
        .checked_mul(sign)?
        .checked_mul(multiplier)
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
    if git_dir != common_dir {
        if configured.is_some() {
            return Err(unsupported(source, "core.worktree in linked layout"));
        }
        let backlink = git_dir.join("gitdir");
        let target = metadata_path(&backlink, &read(&backlink)?)?;
        if !target.is_absolute() {
            return Err(unsupported(&backlink, "relative linked-worktree backlink"));
        }
        let target = canonical(&target)?;
        if read_gitfile(&target)? != git_dir {
            return Err(malformed(
                &backlink,
                "backlink does not point back to Git directory",
            ));
        }
        let root = target
            .parent()
            .ok_or_else(|| malformed(&backlink, "backlink has no parent"))?
            .to_path_buf();
        if inferred.is_some_and(|inferred| inferred != root) {
            return Err(malformed(&backlink, "worktree and backlink disagree"));
        }
        return Ok(Some(root));
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
        return Ok(Some(canonical(&git_dir.join(path_bytes(source, value)?))?));
    }
    if bare == Some(true) {
        return Ok(None);
    }
    if let Some(root) = inferred {
        return Ok(Some(root));
    }
    if bare == Some(false) {
        return Err(unsupported(
            source,
            "non-bare Git directory without an explicit worktree relationship",
        ));
    }
    Ok(None)
}

fn reject_storage_features(common: &Path, objects: &Path) -> Result<(), OpenError> {
    for (path, feature) in [
        (common.join("shallow"), "shallow repository"),
        (objects.join("info/alternates"), "object alternates"),
        (
            objects.join("info/http-alternates"),
            "HTTP object alternates",
        ),
    ] {
        if exists(&path)? {
            return Err(unsupported(&path, feature));
        }
    }
    Ok(())
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
    let bytes = read(path)?;
    let value = bytes
        .strip_prefix(b"gitdir: ")
        .ok_or_else(|| malformed(path, "expected gitdir: indirection"))?;
    let target = metadata_path(path, value)?;
    canonical(&path.parent().unwrap_or(Path::new(".")).join(target))
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
