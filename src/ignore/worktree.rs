use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{Case, Error, Ignore, InputKind, Limits, Match, Source};
use crate::Repository;
use crate::workflow::Environment;

/// Where a loaded ignore file sits in Git's precedence order.
///
/// ```
/// use girt::ignore::SourceKind;
///
/// let kind = SourceKind::Directory(b"src".to_vec());
/// assert_eq!(kind.directory(), Some(&b"src"[..]));
/// assert_eq!(SourceKind::Info.directory(), None);
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SourceKind {
    /// The file `core.excludesFile` names, or its XDG default.
    Global,
    /// The common directory's `info/exclude`.
    Info,
    /// A worktree `.gitignore` in this slash-separated directory; empty for the root.
    Directory(Vec<u8>),
}

impl SourceKind {
    /// Returns the repository-relative directory of a `.gitignore`, or `None` for root-anchored
    /// sources.
    #[must_use]
    pub fn directory(&self) -> Option<&[u8]> {
        match self {
            Self::Directory(directory) => Some(directory),
            Self::Global | Self::Info => None,
        }
    }
}

/// A loaded ignore file, retained so a [`Match`] can be explained.
///
/// ```
/// use girt::ignore::InputKind;
/// use girt::workflow::Environment;
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
/// std::fs::write(
///     repo.worktree().unwrap().join(".gitignore"),
///     b"# notes\n*.tmp\n",
/// )?;
/// let mut rules = repo.ignore_rules(&Environment::default())?;
/// let found = rules.check(b"draft.tmp", InputKind::File)?.unwrap();
/// let source = rules.source(found.source).unwrap();
/// assert!(source.path().ends_with(".gitignore"));
/// assert_eq!(source.line(found.line), Some(&b"*.tmp"[..]));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct SourceFile {
    kind: SourceKind,
    path: PathBuf,
    content: Vec<u8>,
}

impl SourceFile {
    /// The source's precedence level and, for a `.gitignore`, its directory.
    #[must_use]
    pub fn kind(&self) -> &SourceKind {
        &self.kind
    }

    /// The filesystem path the content was read from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The raw bytes read.
    #[must_use]
    pub fn content(&self) -> &[u8] {
        &self.content
    }

    /// Returns a one-based physical line without its line ending, as [`Match::line`] counts.
    #[must_use]
    pub fn line(&self, line: usize) -> Option<&[u8]> {
        let line = self
            .content
            .split(|byte| *byte == b'\n')
            .nth(line.checked_sub(1)?)?;
        Some(line.strip_suffix(b"\r").unwrap_or(line))
    }
}

/// Loading or applying a repository's ignore files failed.
///
/// No partial rule set is returned. A missing optional file is not an error; any other read
/// failure is reported rather than treated as an empty file.
///
/// ```
/// use girt::ignore::LoadError;
/// use girt::workflow::Environment;
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("bare"),
///     InitKind::Bare,
/// )?;
/// assert!(matches!(
///     repo.ignore_rules(&Environment::default()),
///     Err(LoadError::Unsupported(_))
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The repository has no working tree, or a configured path form is not supported.
    #[error("unsupported ignore configuration: {0}")]
    Unsupported(&'static str),
    /// `core.ignoreCase` is not a boolean.
    #[error("reading ignore configuration")]
    Config(#[from] crate::config::InvalidValue),
    /// An ignore file exists but could not be read.
    #[error("reading ignore file {path:?}")]
    Io {
        /// File that failed.
        path: PathBuf,
        /// Operating-system cause.
        #[source]
        source: std::io::Error,
    },
    /// An ignore file exceeded the rule limits.
    #[error("loading ignore file {path:?}")]
    Rules {
        /// File that failed.
        path: PathBuf,
        /// Limit or path cause.
        #[source]
        source: Error,
    },
    /// A queried path was invalid or exhausted the matching budget.
    #[error("checking ignore rules for {path:?}")]
    Check {
        /// Queried repository-relative path.
        path: Vec<u8>,
        /// Path or limit cause.
        #[source]
        source: Error,
    },
}

/// The ignore files Git consults for a worktree, with `.gitignore` files read on demand.
///
/// [`Repository::ignore_rules`] loads the global excludes file and `info/exclude` and chooses
/// [`Case`] from `core.ignoreCase`. [`Self::check`] then reads the `.gitignore` of the root and of
/// each ancestor directory of a queried path the first time it is needed, as `git check-ignore`
/// does. Pass the rules to [`Repository::status`] for ignore-aware status; status reads the
/// `.gitignore` files of the directories it visits itself and uses only [`Self::base`].
///
/// Loaded files are a snapshot: an edited `.gitignore` is not reread. Load the rules again to see
/// changes. Like Git, a `.gitignore` that is a symlink is skipped rather than followed, and
/// `.gitignore` files below a symlinked or non-directory component are not read. On macOS and
/// Linux each query reopens its ancestors with no-follow directory descriptors, so a directory
/// replaced by a symlink after an earlier query is not read through either.
///
/// This type does not consult the index: Git never ignores a tracked path, so apply tracked-file
/// policy before acting on a [`Match`].
///
/// ```
/// use girt::ignore::InputKind;
/// use girt::workflow::Environment;
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
/// let worktree = repo.worktree().unwrap();
/// std::fs::create_dir(worktree.join("src"))?;
/// std::fs::write(worktree.join(".gitignore"), b"*.log\n")?;
/// std::fs::write(worktree.join("src/.gitignore"), b"!keep.log\n")?;
///
/// let mut rules = repo.ignore_rules(&Environment::default())?;
/// assert!(rules.check(b"build.log", InputKind::File)?.unwrap().ignored);
/// assert!(
///     !rules
///         .check(b"src/keep.log", InputKind::File)?
///         .unwrap()
///         .ignored
/// );
/// assert!(rules.check(b"src/main.rs", InputKind::File)?.is_none());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct WorktreeRules {
    root: PathBuf,
    base: Ignore,
    checked: Ignore,
    sources: Vec<SourceFile>,
    loaded: BTreeSet<Vec<u8>>,
}

impl WorktreeRules {
    /// The comparison policy chosen from `core.ignoreCase`.
    #[must_use]
    pub fn case(&self) -> Case {
        self.base.case()
    }

    /// The limits every source and query is checked against.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.base.limits()
    }

    /// Rules from the global excludes file and `info/exclude` only, without any `.gitignore`.
    ///
    /// Add `.gitignore` content with [`Ignore::add`] when your application traverses the worktree
    /// itself. Source indices of these rules match [`Self::source`].
    #[must_use]
    pub fn base(&self) -> &Ignore {
        &self.base
    }

    /// Returns a loaded source by the index a [`Match`] reports.
    #[must_use]
    pub fn source(&self, index: usize) -> Option<&SourceFile> {
        self.sources.get(index)
    }

    /// Resolves a repository-relative path, first reading any unread ancestor `.gitignore`.
    ///
    /// Follows [`Ignore::check`]: an excluded ancestor decides the path, and `None` means no rule
    /// matched. Supply a slash-separated path without a trailing slash.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Io`] when a `.gitignore` exists but cannot be read,
    /// [`LoadError::Rules`] when one exceeds the limits, and [`LoadError::Check`] for an invalid
    /// path or exhausted matching budget. Files read before a failure stay loaded; a file that
    /// failed is retried, and fails again, on the next query.
    pub fn check(&mut self, path: &[u8], kind: InputKind) -> Result<Option<Match>, LoadError> {
        let ends = std::iter::once(0).chain(
            path.iter()
                .enumerate()
                .filter_map(|(end, byte)| (*byte == b'/').then_some(end)),
        );
        // Every query walks the ancestors again without following symlinks, so a directory that
        // became a symlink is never read through, even when an earlier query loaded it.
        let mut walk = Ancestors::open(&self.root)?;
        for end in ends {
            let directory = path.get(..end).unwrap_or_default();
            if !walk.enter(&self.root, directory)? {
                break;
            }
            if self.loaded.contains(directory) {
                continue;
            }
            let file = native(&self.root, directory)
                .unwrap_or_else(|| self.root.clone())
                .join(".gitignore");
            let remaining = self.checked.remaining_bytes();
            if let Some(content) = walk.read_gitignore(&file, remaining)? {
                self.push(
                    SourceKind::Directory(directory.to_vec()),
                    file,
                    content,
                    true,
                )?;
            }
            self.loaded.insert(directory.to_vec());
        }
        self.checked
            .check(path, kind)
            .map_err(|source| LoadError::Check {
                path: path.to_vec(),
                source,
            })
    }

    fn push(
        &mut self,
        kind: SourceKind,
        path: PathBuf,
        content: Vec<u8>,
        directory_only: bool,
    ) -> Result<(), LoadError> {
        let source = match &kind {
            SourceKind::Global => Source::Global,
            SourceKind::Info => Source::Info,
            SourceKind::Directory(directory) => Source::Directory(directory),
        };
        let rules = |source| LoadError::Rules {
            path: path.clone(),
            source,
        };
        let index = self.checked.add(source, &content).map_err(rules)?;
        if !directory_only {
            self.base.add(source, &content).map_err(rules)?;
        }
        debug_assert_eq!(index, self.sources.len());
        self.sources.push(SourceFile {
            kind,
            path,
            content,
        });
        Ok(())
    }
}

impl Repository {
    /// Loads the ignore files Git reads for this worktree, with default [`Limits`].
    ///
    /// Equivalent to [`Self::ignore_rules_with`] with [`Limits::default`]; see it for the sources
    /// and configuration consulted.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::ignore_rules_with`].
    pub fn ignore_rules(&self, environment: &Environment) -> Result<WorktreeRules, LoadError> {
        self.ignore_rules_with(environment, Limits::default())
    }

    /// Loads the ignore files Git reads for this worktree.
    ///
    /// Reads, lowest precedence first:
    ///
    /// 1. The file `core.excludesFile` names. A leading `~/` expands to `HOME` and a relative path
    ///    resolves against the working tree, as Git resolves it. When unset, Git's default
    ///    `$XDG_CONFIG_HOME/git/ignore` is used, or `$HOME/.config/git/ignore` when
    ///    `XDG_CONFIG_HOME` is unset or empty. An empty value names no file.
    /// 2. `info/exclude` in the common directory, shared by every linked worktree.
    /// 3. `.gitignore` files, read later by [`WorktreeRules::check`] or by status traversal.
    ///
    /// `core.ignoreCase = true` selects [`Case::AsciiInsensitive`]; otherwise matching is exact.
    /// Configuration comes from this repository's [`Self::config`], so open the repository with
    /// [`crate::config::ConfigInputs::git_default`] to see `core.excludesFile` from the user's
    /// global configuration. `environment` supplies only `HOME` and `XDG_CONFIG_HOME`; the process
    /// environment is never read. Missing files are skipped, as Git skips them.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Unsupported`] for a repository without a working tree, a
    /// `core.excludesFile` that starts with `~user/` (Git looks the user up; girt does not), or a
    /// source that is not a regular file (such as a FIFO, which is never waited on),
    /// [`LoadError::Config`] for a non-boolean `core.ignoreCase`, and [`LoadError::Io`] or
    /// [`LoadError::Rules`] when a file exists but cannot be read or exceeds `limits`. Reads stop
    /// at the remaining [`Limits::bytes`] budget, so an oversized file is never buffered whole.
    ///
    /// ```
    /// use girt::Repository;
    /// use girt::config::ConfigInputs;
    /// use girt::ignore::{InputKind, Limits};
    /// use girt::workflow::Environment;
    ///
    /// let home = tempfile::tempdir()?;
    /// std::fs::create_dir_all(home.path().join(".config/git"))?;
    /// std::fs::write(home.path().join(".config/git/ignore"), b".DS_Store\n")?;
    /// let environment = Environment::default()
    ///     .with("HOME", home.path())
    ///     .with("GIT_CONFIG_NOSYSTEM", "1");
    ///
    /// let directory = tempfile::tempdir()?;
    /// girt::Repository::init(
    ///     girt::ObjectFormat::Sha1,
    ///     directory.path().join("w"),
    ///     girt::InitKind::Worktree,
    /// )?;
    /// let repo = Repository::open_with_config(
    ///     directory.path().join("w"),
    ///     &ConfigInputs::git_default(&environment)?,
    /// )?;
    /// let mut rules = repo.ignore_rules_with(&environment, Limits::default())?;
    /// assert!(
    ///     rules
    ///         .check(b"src/.DS_Store", InputKind::File)?
    ///         .unwrap()
    ///         .ignored
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn ignore_rules_with(
        &self,
        environment: &Environment,
        limits: Limits,
    ) -> Result<WorktreeRules, LoadError> {
        let root = self
            .worktree()
            .ok_or(LoadError::Unsupported("repository has no working tree"))?
            .to_owned();
        let case = match self.config().boolean("core", None, "ignorecase")? {
            Some(true) => Case::AsciiInsensitive,
            _ => Case::Sensitive,
        };
        let mut rules = WorktreeRules {
            base: Ignore::new(case, limits),
            checked: Ignore::new(case, limits),
            root,
            sources: Vec::new(),
            loaded: BTreeSet::new(),
        };
        if let Some(path) = global_excludes(self, &rules.root, environment)?
            && let Some(content) = read_optional(&path, rules.checked.remaining_bytes())?
        {
            rules.push(SourceKind::Global, path, content, false)?;
        }
        let exclude = self.common_dir().join("info").join("exclude");
        if let Some(content) = read_optional(&exclude, rules.checked.remaining_bytes())? {
            rules.push(SourceKind::Info, exclude, content, false)?;
        }
        Ok(rules)
    }
}

/// Resolves `core.excludesFile` or its default without consulting the process environment.
fn global_excludes(
    repository: &Repository,
    root: &Path,
    environment: &Environment,
) -> Result<Option<PathBuf>, LoadError> {
    let Some(value) = repository.config().string("core", None, "excludesfile") else {
        let xdg = environment
            .get("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                environment
                    .get("HOME")
                    .map(|home| Path::new(home).join(".config"))
            });
        return Ok(xdg.map(|directory| directory.join("git").join("ignore")));
    };
    if value.is_empty() {
        return Ok(None);
    }
    if let Some(rest) = value.strip_prefix(b"~/") {
        let home = environment.get("HOME").ok_or(LoadError::Unsupported(
            "core.excludesFile uses ~/ but HOME is unset",
        ))?;
        return Ok(Some(Path::new(home).join(os_path(rest)?)));
    }
    if value.starts_with(b"~") {
        return Err(LoadError::Unsupported(
            "core.excludesFile uses ~user/ expansion",
        ));
    }
    Ok(Some(root.join(os_path(value)?)))
}

fn os_path(bytes: &[u8]) -> Result<PathBuf, LoadError> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes)
            .map(PathBuf::from)
            .map_err(|_| LoadError::Unsupported("core.excludesFile is not Unicode"))
    }
}

/// Converts a validated slash-separated directory to a native path below `root`.
fn native(root: &Path, directory: &[u8]) -> Option<PathBuf> {
    let mut path = root.to_owned();
    if directory.is_empty() {
        return Some(path);
    }
    for component in directory.split(|byte| *byte == b'/') {
        if component.is_empty() || component == b"." || component == b".." {
            return None;
        }
        path.push(os_path(component).ok()?);
    }
    Some(path)
}

fn io(path: &Path, source: impl Into<std::io::Error>) -> LoadError {
    LoadError::Io {
        path: path.to_owned(),
        source: source.into(),
    }
}

/// Reads at most `remaining` bytes, so an oversized source fails before it is buffered.
fn read_bounded(
    file: impl std::io::Read,
    path: &Path,
    remaining: usize,
) -> Result<Vec<u8>, LoadError> {
    use std::io::Read as _;
    let mut content = Vec::new();
    let limit = u64::try_from(remaining)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(limit)
        .read_to_end(&mut content)
        .map_err(|source| io(path, source))?;
    if content.len() > remaining {
        return Err(LoadError::Rules {
            path: path.to_owned(),
            source: Error::Limit("source bytes"),
        });
    }
    Ok(content)
}

/// Reads a configured excludes file or `info/exclude`, following symlinks as Git does.
///
/// The file is opened without blocking, so a FIFO is refused rather than waited on.
fn read_optional(path: &Path, remaining: usize) -> Result<Option<Vec<u8>>, LoadError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits().cast_signed());
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io(path, source)),
    };
    if !file
        .metadata()
        .map_err(|source| io(path, source))?
        .is_file()
    {
        return Err(LoadError::Unsupported("ignore file is not a regular file"));
    }
    read_bounded(file, path, remaining).map(Some)
}

/// The worktree directories of one query, opened component by component without following
/// symlinks.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Ancestors {
    directory: Option<rustix::fd::OwnedFd>,
    depth: usize,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Ancestors {
    fn open(root: &Path) -> Result<Self, LoadError> {
        let directory = crate::status::worktree::open_root(root).map_err(|_| {
            io(
                root,
                std::io::Error::other("cannot open the working tree without following symlinks"),
            )
        })?;
        Ok(Self {
            directory: Some(directory),
            depth: 0,
        })
    }

    /// Opens the last component of `directory`; false when it is missing, a symlink or not a
    /// directory, so no `.gitignore` at or below it applies.
    fn enter(&mut self, root: &Path, directory: &[u8]) -> Result<bool, LoadError> {
        use std::os::unix::ffi::OsStrExt;
        let Some(current) = &self.directory else {
            return Ok(false);
        };
        let components = directory
            .split(|byte| *byte == b'/')
            .filter(|c| !c.is_empty());
        let Some(name) = components.clone().nth(self.depth) else {
            return Ok(true);
        };
        if components.count() != self.depth + 1 || name == b"." || name == b".." {
            self.directory = None;
            return Ok(false);
        }
        match rustix::fs::openat(
            current,
            std::ffi::OsStr::from_bytes(name),
            crate::status::worktree::directory_flags(),
            rustix::fs::Mode::empty(),
        ) {
            Ok(next) => {
                self.directory = Some(next);
                self.depth += 1;
                Ok(true)
            }
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
                self.directory = None;
                Ok(false)
            }
            Err(source) => Err(io(&native(root, directory).unwrap_or_default(), source)),
        }
    }

    /// Reads `.gitignore` in the current directory, skipping symlinks and non-files as Git does.
    fn read_gitignore(&self, path: &Path, remaining: usize) -> Result<Option<Vec<u8>>, LoadError> {
        use rustix::fs::{FileType, Mode, OFlags};
        let Some(directory) = &self.directory else {
            return Ok(None);
        };
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let file = match rustix::fs::openat(directory, ".gitignore", flags, Mode::empty()) {
            Ok(file) => file,
            Err(rustix::io::Errno::NOENT | rustix::io::Errno::LOOP) => return Ok(None),
            Err(source) => return Err(io(path, source)),
        };
        let stat = rustix::fs::fstat(&file).map_err(|source| io(path, source))?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok(None);
        }
        read_bounded(std::fs::File::from(file), path, remaining).map(Some)
    }
}

/// Path-based fallback where descriptor-relative traversal is unavailable: each query rechecks
/// every ancestor with `symlink_metadata`, which cannot exclude concurrent substitution.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
struct Ancestors {
    blocked: bool,
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
impl Ancestors {
    fn open(_root: &Path) -> Result<Self, LoadError> {
        Ok(Self { blocked: false })
    }

    fn enter(&mut self, root: &Path, directory: &[u8]) -> Result<bool, LoadError> {
        self.blocked = self.blocked
            || !native(root, directory)
                .and_then(|folder| std::fs::symlink_metadata(folder).ok())
                .is_some_and(|metadata| metadata.is_dir());
        Ok(!self.blocked)
    }

    fn read_gitignore(&self, path: &Path, remaining: usize) -> Result<Option<Vec<u8>>, LoadError> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_file() => {
                let file = std::fs::File::open(path).map_err(|source| io(path, source))?;
                read_bounded(file, path, remaining).map(Some)
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(source) => Err(io(path, source)),
        }
    }
}
