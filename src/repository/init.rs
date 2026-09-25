//! Exclusive creation of minimal SHA-1 or SHA-256 repository metadata.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use super::discover::has_marker;
use super::{OpenError, Repository};
use crate::refs::RefName;

/// Layout to create with [`Repository::init`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InitKind {
    /// Put metadata in `.git` under the destination worktree directory.
    Worktree,
    /// Put metadata directly in a new destination directory, without a worktree.
    Bare,
}

/// Initialization refusal, filesystem failure, or final metadata validation failure.
#[derive(Debug, Error)]
pub enum InitError {
    /// A destination or repository marker already exists; it was not modified.
    #[error("initialization destination already exists: {0}")]
    AlreadyExists(PathBuf),
    /// A filesystem operation failed. Newly created files may remain; see [`Repository::init`].
    #[error("cannot initialize {path}: {source}")]
    Io {
        /// Path being accessed or created.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Inspecting existing metadata or opening the newly created repository failed.
    #[error(transparent)]
    Open(#[from] OpenError),
}

impl Repository {
    // Clone has exclusively reserved an absent destination root. Keep population shared with
    // init, but never relax init's public bare-destination refusal to accommodate clone.
    pub(crate) fn init_reserved_clone(path: &Path, kind: InitKind) -> Result<Self, InitError> {
        let git_dir = match kind {
            InitKind::Bare => path.to_path_buf(),
            InitKind::Worktree => {
                let git_dir = path.join(".git");
                create_directory(&git_dir)?;
                git_dir
            }
        };
        populate(&git_dir, kind, crate::ObjectFormat::Sha1)?;
        Ok(Self::open(path)?)
    }

    /// Creates an empty repository in the selected object format with unborn `refs/heads/main`.
    ///
    /// Creates version-0 configuration for SHA-1 or version-1 with `extensions.objectFormat` for
    /// SHA-256, files-backend reference directories and an object directory. No Git process,
    /// templates, hooks, ambient configuration, index, or initial commit are used. The
    /// destination's parent must exist. Ordinary worktrees may use an existing directory
    /// containing unrelated files; a `.git`, `HEAD`, or `objects` entry refuses initialization.
    /// Bare destinations must not exist, even as empty directories. Separate Git directories
    /// and linked worktrees cannot be created through this API.
    ///
    /// Reinitialization is deliberately refused. Existing recognized metadata is inspected with
    /// [`Self::open`] so unsupported formats/layouts and malformed repositories retain their
    /// errors. No existing files are overwritten or removed. Directory creation reserves the
    /// new metadata location before writing, so competing initializers cannot both populate it.
    ///
    /// # Errors
    ///
    /// Existing destinations return [`InitError::AlreadyExists`] or an opening error before writes.
    /// On later I/O or final opening failure, the newly created destination and partial metadata
    /// remain for inspection; there is no automatic rollback or crash-durability guarantee.
    /// Callers must exclude concurrent path replacement and other writers while initializing.
    /// Symlinks in parent paths and an existing worktree root are resolved by the filesystem;
    /// this operation is not a security boundary for untrusted paths.
    ///
    /// ```
    /// use girt::{InitKind, Repository};
    /// let root = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     girt::ObjectFormat::Sha1,
    ///     root.path().join("project"),
    ///     InitKind::Worktree,
    /// )?;
    /// let nested = repo.worktree().unwrap().join("src");
    /// std::fs::create_dir(&nested)?;
    /// assert_eq!(Repository::discover(&nested)?.git_dir(), repo.git_dir());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn init(
        format: crate::ObjectFormat,
        path: impl AsRef<Path>,
        kind: InitKind,
    ) -> Result<Self, InitError> {
        let path = path.as_ref();
        if has_marker(path)? {
            // Preserve unsupported format/layout errors before any mutation.
            Self::open(path)?;
            return Err(InitError::AlreadyExists(path.into()));
        }
        let git_dir = match kind {
            InitKind::Bare => {
                create_directory(path)?;
                path.to_path_buf()
            }
            InitKind::Worktree => {
                match fs::metadata(path) {
                    Ok(metadata) if metadata.is_dir() => (),
                    Ok(_) => return Err(InitError::AlreadyExists(path.into())),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        create_directory(path)?
                    }
                    Err(source) => return Err(init_io(path, source)),
                }
                let git_dir = path.join(".git");
                create_directory(&git_dir)?;
                git_dir
            }
        };
        populate(&git_dir, kind, format)?;
        Ok(Self::open(path)?)
    }
}

fn populate(git_dir: &Path, kind: InitKind, format: crate::ObjectFormat) -> Result<(), InitError> {
    for name in [
        "objects",
        "objects/info",
        "objects/pack",
        "refs",
        "refs/heads",
        "refs/tags",
    ] {
        create_directory(&git_dir.join(name))?;
    }
    create_file(&git_dir.join("config"), &format_config(kind, format))?;
    // HEAD is last so a partially initialized directory does not look ready to open.
    let mut head = b"ref: ".to_vec();
    head.extend_from_slice(initial_branch().as_bytes());
    head.push(b'\n');
    create_file(&git_dir.join("HEAD"), &head)
}

// Initialization owns these defaults, including exact bytes used by clone's config precondition.
pub(crate) fn initial_config(kind: InitKind) -> Vec<u8> {
    format_config(kind, crate::ObjectFormat::Sha1)
}

fn format_config(kind: InitKind, format: crate::ObjectFormat) -> Vec<u8> {
    if format == crate::ObjectFormat::Sha256 {
        return format!("[core]\n\trepositoryformatversion = 1\n\tbare = {}\n[extensions]\n\tobjectformat = sha256\n", kind == InitKind::Bare).into_bytes();
    }
    format!(
        "[core]\n\trepositoryformatversion = 0\n\tbare = {}\n",
        kind == InitKind::Bare
    )
    .into_bytes()
}

pub(crate) fn initial_branch() -> RefName {
    RefName::new("refs/heads/main").expect("fixed initial branch")
}

fn create_directory(path: &Path) -> Result<(), InitError> {
    fs::create_dir(path).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            InitError::AlreadyExists(path.into())
        } else {
            init_io(path, source)
        }
    })
}

fn create_file(path: &Path, bytes: &[u8]) -> Result<(), InitError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| init_io(path, source))?;
    file.write_all(bytes)
        .map_err(|source| init_io(path, source))
}

fn init_io(path: &Path, source: io::Error) -> InitError {
    InitError::Io {
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::ordinary(InitKind::Worktree, false)]
    #[case::bare(InitKind::Bare, true)]
    fn creates_expected_layout(#[case] kind: InitKind, #[case] bare: bool) {
        let root = tempfile::tempdir().unwrap();
        let repo =
            Repository::init(crate::ObjectFormat::Sha1, root.path().join("repo"), kind).unwrap();
        assert_eq!(repo.worktree().is_none(), bare);
        assert_eq!(repo.format_version(), 0);
        assert_eq!(
            fs::read(repo.git_dir().join("HEAD")).unwrap(),
            b"ref: refs/heads/main\n"
        );
        assert!(!repo.git_dir().join("refs/heads/main").exists());
    }

    #[test]
    fn preserves_unrelated_worktree_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("keep"), b"original").unwrap();
        Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
        assert_eq!(fs::read(root.path().join("keep")).unwrap(), b"original");
    }

    #[test]
    fn refuses_existing_empty_bare_destination() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Bare),
            Err(InitError::AlreadyExists(_))
        ));
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[rstest]
    #[case::file(".git")]
    #[case::head("HEAD")]
    fn refuses_existing_marker_without_writes(#[case] marker: &str) {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join(marker), b"keep").unwrap();
        assert!(
            Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Worktree).is_err()
        );
        assert_eq!(fs::read(root.path().join(marker)).unwrap(), b"keep");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn missing_parent_is_not_created() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            Repository::init(
                crate::ObjectFormat::Sha1,
                root.path().join("missing/repo"),
                InitKind::Bare
            )
            .is_err()
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn dangling_marker_is_not_replaced() {
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing", root.path().join(".git")).unwrap();
        assert!(
            Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Worktree).is_err()
        );
        assert_eq!(
            fs::read_link(root.path().join(".git")).unwrap(),
            Path::new("missing")
        );
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn partial_population_failure_preserves_files_and_omits_head() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("config"), b"another writer").unwrap();
        assert!(populate(root.path(), InitKind::Bare, crate::ObjectFormat::Sha1).is_err());
        assert_eq!(
            fs::read(root.path().join("config")).unwrap(),
            b"another writer"
        );
        assert!(root.path().join("objects").is_dir());
        assert!(!root.path().join("HEAD").exists());
    }

    #[test]
    fn concurrent_initializers_have_one_winner() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("repo");
        let barrier = std::sync::Barrier::new(2);
        let (first, second) = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                Repository::init(crate::ObjectFormat::Sha1, &path, InitKind::Bare)
            });
            barrier.wait();
            let second = Repository::init(crate::ObjectFormat::Sha1, &path, InitKind::Bare);
            (first.join().unwrap(), second)
        });
        assert_ne!(first.is_ok(), second.is_ok());
        let repo = Repository::open(&path).unwrap();
        assert_eq!(
            fs::read(repo.git_dir().join("HEAD")).unwrap(),
            b"ref: refs/heads/main\n"
        );
    }

    #[test]
    fn exclusive_file_creation_preserves_existing_bytes() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config");
        fs::write(&path, b"keep").unwrap();
        assert!(create_file(&path, b"replace").is_err());
        assert_eq!(fs::read(path).unwrap(), b"keep");
    }
}
