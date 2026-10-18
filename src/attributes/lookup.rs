use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use super::rules::validate_path;
use super::{Attributes, Case, Error, Limits, Source, State};
use crate::source_files::{Ancestors, FileError, native, os_path, read_optional};
use crate::workflow::Environment;
use crate::{EntryMode, ObjectId, Objects, ReadLimits, Repository};

/// Where per-directory `.gitattributes` files are read from.
///
/// [`Repository::attribute_source`] chooses the source Git uses by default.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DirectorySource {
    /// Files in the working tree, as Git reads them for a repository with one.
    Worktree,
    /// Blobs in this tree, as Git reads them for `--attr-source` or `attr.tree`.
    Tree(ObjectId),
    /// No per-directory files: only the system, global and `info/attributes` files apply, as in a
    /// bare repository.
    None,
}

/// Loading or applying a repository's attribute files failed.
///
/// A missing optional file is not an error; any other read failure is reported rather than
/// treated as an empty file.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// The configuration names a form girt does not support, or the source cannot be used.
    #[error("unsupported attribute configuration: {0}")]
    Unsupported(&'static str),
    /// `core.ignoreCase` is not a boolean.
    #[error("reading attribute configuration")]
    Config(#[from] crate::config::InvalidValue),
    /// `GIT_ATTR_SOURCE` does not name a tree.
    #[error("resolving GIT_ATTR_SOURCE")]
    Source(#[source] Box<crate::Error>),
    /// An attribute file exists but could not be read.
    #[error("reading attribute file {path:?}")]
    Io {
        /// File that failed.
        path: PathBuf,
        /// Operating-system cause.
        #[source]
        source: std::io::Error,
    },
    /// A `.gitattributes` blob or its tree could not be read from storage.
    #[error("reading attribute file {path:?} from the attribute tree")]
    Object {
        /// Repository-relative path of the file or directory.
        path: Vec<u8>,
        /// Storage cause, boxed to keep this error small.
        #[source]
        source: Box<crate::Error>,
    },
    /// An attribute file exceeded the limits, or a query path was invalid or exhausted the budget.
    #[error("applying attributes for {path:?}")]
    Rules {
        /// File or queried path.
        path: Vec<u8>,
        /// Limit or path cause.
        #[source]
        source: Error,
    },
}

impl From<FileError> for LoadError {
    fn from(error: FileError) -> Self {
        match error {
            FileError::Io { path, source } => Self::Io { path, source },
            FileError::TooLarge { path } => Self::Rules {
                path: path.to_string_lossy().into_owned().into_bytes(),
                source: Error::Limit("source bytes"),
            },
            FileError::NotFile => Self::Unsupported("attribute file is not a regular file"),
        }
    }
}

/// The attribute files Git consults for a repository, with `.gitattributes` read on demand.
///
/// [`Repository::attributes`] loads the system, global and `info/attributes` files and chooses
/// [`Case`] from `core.ignoreCase`. [`Self::get`] then reads the `.gitattributes` of the root and
/// of each ancestor directory of a queried path the first time it is needed, from the
/// [`DirectorySource`]. Loaded files are a snapshot; load again to see edits.
///
/// Like Git, a worktree `.gitattributes` that is a symlink is skipped, as is one below a
/// symlinked component, and a tree entry that is not a regular file is ignored. The index is
/// never consulted: Git does not read it when comparing trees.
///
/// ```
/// use girt::attributes::{DirectorySource, State};
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
/// std::fs::write(worktree.join(".gitattributes"), b"*.png binary\n")?;
///
/// let mut attributes = repo.attributes(&Environment::default(), DirectorySource::Worktree)?;
/// assert_eq!(attributes.get(b"img/logo.png", b"diff")?, State::Unset);
/// assert_eq!(attributes.get(b"src/lib.rs", b"diff")?, State::Unspecified);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct Lookup {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    environment: Environment,
    attributes: Attributes,
    directories: Directories,
    loaded: BTreeSet<Vec<u8>>,
}

#[derive(Debug)]
enum Directories {
    Worktree {
        root: PathBuf,
        /// Opened on first use and kept, so sorted queries reuse open ancestors.
        walk: Option<Ancestors>,
    },
    Tree {
        objects: Box<Objects>,
        limits: ReadLimits,
        /// Tree identity of each visited directory; `None` when it is absent or not a tree.
        trees: HashMap<Vec<u8>, Option<ObjectId>>,
    },
    None,
}

impl Lookup {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn environment(&self) -> &Environment {
        &self.environment
    }

    /// The files loaded so far, as one rule set.
    #[must_use]
    pub fn attributes(&self) -> &Attributes {
        &self.attributes
    }

    /// Returns attribute `name` for the file at `path`, first reading any unread ancestor
    /// `.gitattributes`.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Io`] or [`LoadError::Object`] when a `.gitattributes` exists but cannot
    /// be read, and [`LoadError::Rules`] when one exceeds the limits or the path is invalid. Files
    /// read before a failure stay loaded.
    pub fn get(&mut self, path: &[u8], name: &[u8]) -> Result<State<'_>, LoadError> {
        let rules = |source| LoadError::Rules {
            path: path.to_vec(),
            source,
        };
        validate_path(path, false, self.attributes.limits().path_bytes).map_err(rules)?;
        self.load_ancestors(path)?;
        self.attributes.get(path, name).map_err(rules)
    }

    fn load_ancestors(&mut self, path: &[u8]) -> Result<(), LoadError> {
        let ends = std::iter::once(0).chain(
            path.iter()
                .enumerate()
                .filter_map(|(end, byte)| (*byte == b'/').then_some(end)),
        );
        let directories: Vec<&[u8]> = ends
            .map(|end| path.get(..end).unwrap_or_default())
            .collect();
        // Most paths share directories with earlier queries; skip them without any I/O.
        let Some(first) = directories
            .iter()
            .position(|directory| !self.loaded.contains(*directory))
        else {
            return Ok(());
        };
        match &mut self.directories {
            Directories::None => Ok(()),
            Directories::Worktree { root, walk } => {
                // Directories are opened from the root without following symlinks; ones kept from
                // earlier queries are the directories opened then.
                let walk = match walk {
                    Some(walk) => walk,
                    None => walk.insert(Ancestors::open(root)?),
                };
                for (index, directory) in directories.iter().copied().enumerate().skip(first) {
                    if !walk.enter(root, directory)? {
                        // A missing, symlinked or non-directory component: no file at or below
                        // it applies.
                        for blocked in directories.get(index..).unwrap_or_default() {
                            self.loaded.insert(blocked.to_vec());
                        }
                        break;
                    }
                    if self.loaded.contains(directory) {
                        continue;
                    }
                    let file = native(root, directory)
                        .unwrap_or_else(|| root.clone())
                        .join(".gitattributes");
                    let remaining = self.attributes.remaining_bytes();
                    if let Some(content) = walk.read(".gitattributes", &file, remaining)? {
                        self.attributes
                            .add(Source::Directory(directory), &content)
                            .map_err(|source| LoadError::Rules {
                                path: file.to_string_lossy().into_owned().into_bytes(),
                                source,
                            })?;
                    }
                    self.loaded.insert(directory.to_vec());
                }
                Ok(())
            }
            Directories::Tree {
                objects,
                limits,
                trees,
            } => {
                for (index, directory) in directories.iter().copied().enumerate().skip(first) {
                    if self.loaded.contains(directory) {
                        continue;
                    }
                    let Some(tree) = tree_at(objects, *limits, trees, directory)? else {
                        for absent in directories.get(index..).unwrap_or_default() {
                            self.loaded.insert(absent.to_vec());
                        }
                        break;
                    };
                    let file = if directory.is_empty() {
                        b".gitattributes".to_vec()
                    } else {
                        [directory, b"/.gitattributes"].concat()
                    };
                    let object = |source: crate::Error| LoadError::Object {
                        path: file.clone(),
                        source: Box::new(source),
                    };
                    let entries = objects
                        .read_tree_with(tree, *limits)
                        .map_err(|e| object(e.into()))?;
                    let blob = entries.entries().iter().find(|entry| {
                        entry.name == b".gitattributes"
                            && matches!(entry.mode, EntryMode::Blob | EntryMode::Executable)
                    });
                    if let Some(entry) = blob {
                        let remaining = self.attributes.remaining_bytes();
                        let limits = limits.with_max_object_bytes(
                            limits.max_object_bytes.min(remaining.saturating_add(1)),
                        );
                        let content = objects
                            .read_blob_with(entry.id, limits)
                            .map_err(|e| object(e.into()))?;
                        self.attributes
                            .add(Source::Directory(directory), &content)
                            .map_err(|source| LoadError::Rules {
                                path: file.clone(),
                                source,
                            })?;
                    }
                    self.loaded.insert(directory.to_vec());
                }
                Ok(())
            }
        }
    }
}

/// Resolves the tree of `directory`, walking down from the root through cached ancestors.
fn tree_at(
    objects: &Objects,
    limits: ReadLimits,
    trees: &mut HashMap<Vec<u8>, Option<ObjectId>>,
    directory: &[u8],
) -> Result<Option<ObjectId>, LoadError> {
    if let Some(id) = trees.get(directory) {
        return Ok(*id);
    }
    let Some((parent, name)) = crate::bytes::rsplit_once(directory, b'/')
        .or_else(|| (!directory.is_empty()).then_some((&b""[..], directory)))
    else {
        // The root is inserted when the lookup is created.
        return Ok(None);
    };
    let found = match tree_at(objects, limits, trees, parent)? {
        None => None,
        Some(parent_tree) => {
            let tree =
                objects
                    .read_tree_with(parent_tree, limits)
                    .map_err(|e| LoadError::Object {
                        path: parent.to_vec(),
                        source: Box::new(e.into()),
                    })?;
            tree.entries()
                .iter()
                .find(|entry| entry.name == name && entry.mode == EntryMode::Tree)
                .map(|entry| entry.id)
        }
    };
    trees.insert(directory.to_vec(), found);
    Ok(found)
}

impl Repository {
    /// Chooses the [`DirectorySource`] Git uses for this repository.
    ///
    /// `GIT_ATTR_SOURCE` in `environment` comes first and must name a tree-ish. Otherwise
    /// `attr.tree` names one; when it does not resolve, Git ignores it, and so does this. Without
    /// either, a repository with a working tree reads its worktree and a bare repository reads no
    /// per-directory files.
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Source`] when `GIT_ATTR_SOURCE` is set but does not resolve to a tree,
    /// and [`LoadError::Object`] when object storage cannot be opened.
    pub fn attribute_source(
        &self,
        environment: &Environment,
    ) -> Result<DirectorySource, LoadError> {
        let named = environment
            .get("GIT_ATTR_SOURCE")
            .map(|value| (value.to_string_lossy().into_owned(), true))
            .or_else(|| {
                self.config()
                    .string("attr", None, "tree")
                    .map(|value| (String::from_utf8_lossy(value).into_owned(), false))
            });
        if let Some((revision, required)) = named {
            match self.resolve_tree(&revision) {
                Ok(tree) => return Ok(DirectorySource::Tree(tree)),
                Err(error) if required => return Err(LoadError::Source(Box::new(error))),
                Err(_) => {}
            }
        }
        Ok(match self.worktree() {
            Some(_) => DirectorySource::Worktree,
            None => DirectorySource::None,
        })
    }

    fn resolve_tree(&self, revision: &str) -> Result<ObjectId, crate::Error> {
        let objects = self.objects()?;
        let resolver = crate::revision::Resolver::new(self, &objects)?;
        let revision = revision.parse()?;
        Ok(resolver.resolve_to(&revision, crate::ObjectKind::Tree)?)
    }

    /// Loads the attribute files Git reads for this repository, with default [`Limits`].
    ///
    /// Equivalent to [`Self::attributes_with`] with [`Limits::default`] and
    /// [`ReadLimits::default`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::attributes_with`].
    pub fn attributes(
        &self,
        environment: &Environment,
        directories: DirectorySource,
    ) -> Result<Lookup, LoadError> {
        self.attributes_with(
            environment,
            directories,
            Limits::default(),
            ReadLimits::default(),
        )
    }

    /// Loads the attribute files Git reads for this repository.
    ///
    /// Reads, lowest precedence first:
    ///
    /// 1. The system file `gitattributes` beside the system configuration girt's
    ///    [`ConfigInputs::git_default`](crate::config::ConfigInputs::git_default) selects, unless
    ///    `GIT_ATTR_NOSYSTEM` is set.
    /// 2. The file `core.attributesFile` names. A leading `~/` expands to `HOME`. When unset, Git's
    ///    default `$XDG_CONFIG_HOME/git/attributes` is used, or `$HOME/.config/git/attributes`.
    /// 3. `.gitattributes` files from `directories`, read later by [`Lookup::get`].
    /// 4. `info/attributes` in the common directory.
    ///
    /// `core.ignoreCase = true` selects [`Case::AsciiInsensitive`]. Configuration comes from
    /// [`Self::config`]; `environment` supplies only `HOME`, `XDG_CONFIG_HOME` and
    /// `GIT_ATTR_NOSYSTEM`. `read` bounds each blob and tree read for [`DirectorySource::Tree`].
    ///
    /// # Errors
    ///
    /// Returns [`LoadError::Unsupported`] for [`DirectorySource::Worktree`] without a working tree,
    /// a `core.attributesFile` that starts with `~user/`, or a configured file that is not a
    /// regular file; [`LoadError::Config`] for a non-boolean `core.ignoreCase`; and
    /// [`LoadError::Io`], [`LoadError::Object`] or [`LoadError::Rules`] when a file exists but
    /// cannot be read or exceeds `limits`.
    pub fn attributes_with(
        &self,
        environment: &Environment,
        directories: DirectorySource,
        limits: Limits,
        read: ReadLimits,
    ) -> Result<Lookup, LoadError> {
        let case = match self.config().boolean("core", None, "ignorecase")? {
            Some(true) => Case::AsciiInsensitive,
            _ => Case::Sensitive,
        };
        let directories = match directories {
            DirectorySource::Worktree => Directories::Worktree {
                root: self
                    .worktree()
                    .ok_or(LoadError::Unsupported("repository has no working tree"))?
                    .to_owned(),
                walk: None,
            },
            DirectorySource::Tree(tree) => Directories::Tree {
                objects: Box::new(self.objects().map_err(|e| LoadError::Object {
                    path: Vec::new(),
                    source: Box::new(e.into()),
                })?),
                limits: read,
                trees: HashMap::from([(Vec::new(), Some(tree))]),
            },
            DirectorySource::None => Directories::None,
        };
        let mut lookup = Lookup {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            environment: environment.clone(),
            attributes: Attributes::new(case, limits),
            directories,
            loaded: BTreeSet::new(),
        };
        let mut files = Vec::new();
        if environment.get("GIT_ATTR_NOSYSTEM").is_none()
            && let Some(path) = crate::config::system_file("gitattributes")
        {
            files.push((Source::System, path));
        }
        if let Some(path) = global_attributes(self, environment)? {
            files.push((Source::Global, path));
        }
        files.push((
            Source::Info,
            self.common_dir().join("info").join("attributes"),
        ));
        for (source, path) in files {
            let remaining = lookup.attributes.remaining_bytes();
            if let Some(content) = read_optional(&path, remaining)? {
                lookup
                    .attributes
                    .add(source, &content)
                    .map_err(|source| LoadError::Rules {
                        path: path.to_string_lossy().into_owned().into_bytes(),
                        source,
                    })?;
            }
        }
        Ok(lookup)
    }
}

/// Resolves `core.attributesFile` or its XDG default without reading the process environment.
fn global_attributes(
    repository: &Repository,
    environment: &Environment,
) -> Result<Option<PathBuf>, LoadError> {
    let Some(value) = repository.config().string("core", None, "attributesfile") else {
        let xdg = environment
            .get("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                environment
                    .get("HOME")
                    .map(|home| Path::new(home).join(".config"))
            });
        return Ok(xdg.map(|directory| directory.join("git").join("attributes")));
    };
    if value.is_empty() {
        return Ok(None);
    }
    let unicode = LoadError::Unsupported("core.attributesFile is not Unicode");
    if let Some(rest) = value.strip_prefix(b"~/") {
        let home = environment.get("HOME").ok_or(LoadError::Unsupported(
            "core.attributesFile uses ~/ but HOME is unset",
        ))?;
        return Ok(Some(Path::new(home).join(os_path(rest).ok_or(unicode)?)));
    }
    if value.starts_with(b"~") {
        return Err(LoadError::Unsupported(
            "core.attributesFile uses ~user/ expansion",
        ));
    }
    let path = os_path(value).ok_or(unicode)?;
    // As for `core.excludesFile`, a relative path resolves against the working tree.
    Ok(Some(match repository.worktree() {
        Some(root) => root.join(path),
        None => path,
    }))
}
