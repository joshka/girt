use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::{Error, FileMatches, Searcher};
use crate::attributes::{Lookup, State};
use crate::ignore::InputKind;
use crate::index::{Limits as IndexLimits, Mode};
use crate::pathspec::{Pathspec, Prune};
use crate::workflow::Environment;
use crate::{EntryMode, ObjectId, Objects, Repository};

/// Which files of a repository a search reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Source {
    /// Tracked files as the working tree has them, Git's default. Tracked files that match
    /// ignore rules are searched; symbolic links, gitlinks and files missing from the working
    /// tree are not. Entries marked skip-worktree are read from the index.
    Worktree,
    /// Tracked and untracked working tree files (`--untracked`). With `ignored` false, Git's
    /// ignore rules drop untracked files and, as in Git, tracked files they match too; with
    /// `ignored` true, nothing is dropped (`--no-exclude-standard`).
    WorktreeAndUntracked {
        /// Whether files ignore rules match are searched.
        ignored: bool,
    },
    /// The blobs the index records (`--cached`).
    Index,
    /// The blobs of a tree, or a single blob, by ID.
    Object(ObjectId),
}

/// Which paths a search reads: a pathspec and an optional `--max-depth`.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pathspec: Pathspec,
    max_depth: Option<usize>,
    environment: Environment,
}

impl Scope {
    /// Every path a pathspec selects, at any depth.
    ///
    /// `attr:` pathspec magic reads attributes as [`Repository::attributes`] does with
    /// [`Self::with_environment`]'s environment.
    pub fn new(pathspec: Pathspec) -> Self {
        Self {
            pathspec,
            ..Self::default()
        }
    }

    /// Reads paths at most `levels` directories below the pathspec entry that selects them
    /// (`--max-depth`). Entries with wildcards are not limited, as in Git; with no entries,
    /// depth counts from the root.
    #[must_use]
    pub fn with_max_depth(mut self, levels: Option<usize>) -> Self {
        self.max_depth = levels;
        self
    }

    /// Uses `environment` for attribute and ignore files, as [`Repository::attributes`] and
    /// [`Repository::ignore_rules`] read it.
    #[must_use]
    pub fn with_environment(mut self, environment: Environment) -> Self {
        self.environment = environment;
        self
    }

    fn selects(&self, path: &[u8], lookup: &mut Option<Lookup>) -> Result<bool, Error> {
        if self
            .max_depth
            .is_some_and(|levels| !self.pathspec.within_depth(path, levels))
        {
            return Ok(false);
        }
        if !self.pathspec.uses_attributes() {
            return Ok(self.pathspec.matches_unchecked(path));
        }
        let Some(lookup) = lookup else {
            return Ok(false);
        };
        self.pathspec.matches_with(path, |requirement| {
            let state: State<'_> =
                lookup
                    .get(path, requirement.name())
                    .map_err(|source| Error::Attributes {
                        path: path.to_vec(),
                        source: Box::new(source),
                    })?;
            Ok(requirement.accepts(state))
        })
    }
}

/// One searched file, handed to the visitor of [`Repository::grep`].
#[derive(Debug)]
#[non_exhaustive]
pub struct Found<'a> {
    /// The path: repository-relative, or relative to the searched tree or directory.
    pub path: &'a [u8],
    /// The file's bytes.
    pub content: &'a [u8],
    /// What the search found; files without a selected line are visited too, for `-L`.
    pub matches: &'a FileMatches,
    /// The searcher that produced `matches`, for [`GitFormat::write`](super::GitFormat::write).
    pub searcher: &'a Searcher,
}

/// Where a candidate file's bytes come from.
enum Content {
    Blob(ObjectId),
    File,
}

impl Repository {
    /// Searches the files `source` and `scope` select, in path order, one at a time.
    ///
    /// Each file is read, searched and passed to `visit`, then dropped, so memory holds one file
    /// at a time; pathspec pruning skips trees that cannot match. `visit` returns
    /// [`ControlFlow::Break`] to stop early, as `git grep -q` does after a match.
    ///
    /// ```
    /// use std::ops::ControlFlow;
    ///
    /// use girt::grep::{Expression, Matching, Options, Query, Scope, Searcher, Source};
    /// use girt::pathspec::Pathspec;
    /// use girt::tree::TreeEditor;
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let path = directory.path().join("repo.git");
    /// let repo = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
    /// let loose = repo.loose_objects();
    /// let mut editor = TreeEditor::new(&loose);
    /// editor.set(
    ///     b"src/lib.rs",
    ///     loose.write_blob(b"fn main() {}\n// TODO: tests\n")?,
    /// )?;
    /// editor.set(b"README", loose.write_blob(b"No todos here.\n")?)?;
    /// let tree = editor.write()?;
    ///
    /// let query = Query::new(&Expression::pattern("TODO"), Matching::new())?;
    /// let mut searcher = Searcher::new(query, Options::new());
    /// let mut hits = Vec::new();
    /// repo.grep(
    ///     Source::Object(tree),
    ///     &Scope::new(Pathspec::all()),
    ///     &mut searcher,
    ///     |found| {
    ///         if found.matches.is_match() {
    ///             hits.push((found.path.to_vec(), found.matches.lines[0].number));
    ///         }
    ///         ControlFlow::Continue(())
    ///     },
    /// )?;
    /// assert_eq!(hits, [(b"src/lib.rs".to_vec(), 2)]);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`Error::Object`] when the index, a tree or a blob cannot be read,
    /// [`Error::Read`] when a working tree file exists but cannot be read, [`Error::Untracked`]
    /// when untracked files cannot be listed, and the errors of [`Searcher::search`].
    pub fn grep(
        &self,
        source: Source,
        scope: &Scope,
        searcher: &mut Searcher,
        mut visit: impl FnMut(Found<'_>) -> ControlFlow<()>,
    ) -> Result<(), Error> {
        let objects = self.objects().map_err(Error::object)?;
        let mut lookup = if scope.pathspec.uses_attributes() {
            let directories = self
                .attribute_source(&scope.environment)
                .map_err(|source| Error::Attributes {
                    path: Vec::new(),
                    source: Box::new(source),
                })?;
            Some(
                self.attributes(&scope.environment, directories)
                    .map_err(|source| Error::Attributes {
                        path: Vec::new(),
                        source: Box::new(source),
                    })?,
            )
        } else {
            None
        };
        let candidates = match source {
            Source::Object(id) => {
                let object = objects.read(id).map_err(Error::object)?;
                if let Some(object) =
                    object.filter(|object| object.kind() == crate::ObjectKind::Blob)
                {
                    let matches = searcher.search(b"", object.data())?;
                    let _ = visit(Found {
                        path: b"",
                        content: object.data(),
                        matches: &matches,
                        searcher,
                    });
                    return Ok(());
                }
                let mut candidates = Vec::new();
                tree_files(
                    &objects,
                    id,
                    Vec::new(),
                    scope,
                    &mut lookup,
                    &mut candidates,
                )?;
                candidates
            }
            Source::Index => self.index_files(&objects, scope, &mut lookup, false)?,
            Source::Worktree => self.index_files(&objects, scope, &mut lookup, true)?,
            Source::WorktreeAndUntracked { ignored } => {
                let mut candidates = self.index_files(&objects, scope, &mut lookup, true)?;
                self.add_untracked(&mut candidates, scope, &mut lookup, ignored)?;
                candidates
            }
        };
        let root = self.worktree().map(Path::to_owned);
        let mut checked = CheckedDirectories::default();
        for (path, content) in candidates {
            let bytes = match content {
                Content::Blob(id) => objects.read_blob(id).map_err(Error::object)?,
                Content::File => {
                    let Some(root) = &root else { continue };
                    match read_worktree_file(root, &path, &mut checked)? {
                        Some(bytes) => bytes,
                        None => continue,
                    }
                }
            };
            let matches = searcher.search(&path, &bytes)?;
            let found = Found {
                path: &path,
                content: &bytes,
                matches: &matches,
                searcher,
            };
            if visit(found).is_break() {
                break;
            }
        }
        Ok(())
    }

    /// The index's paths; working tree reads unless `worktree` is false or skip-worktree is set.
    fn index_files(
        &self,
        objects: &Objects,
        scope: &Scope,
        lookup: &mut Option<Lookup>,
        worktree: bool,
    ) -> Result<Vec<(Vec<u8>, Content)>, Error> {
        let index = self
            .read_index(IndexLimits::default())
            .map_err(Error::object)?;
        let Some(index) = index else {
            return Ok(Vec::new());
        };
        let mut candidates: Vec<(Vec<u8>, Content)> = Vec::new();
        for entry in index.entries() {
            // Each side of a conflict names the same working tree file.
            if candidates
                .last()
                .is_some_and(|(path, _)| *path == entry.path)
            {
                continue;
            }
            let content = match entry.mode {
                Mode::SparseDirectory => {
                    let directory = entry.path.strip_suffix(b"/").unwrap_or(&entry.path);
                    tree_files(
                        objects,
                        entry.id,
                        directory.to_vec(),
                        scope,
                        lookup,
                        &mut candidates,
                    )?;
                    continue;
                }
                Mode::Gitlink => continue,
                Mode::Symlink if worktree => continue,
                _ if worktree && !entry.skip_worktree => Content::File,
                _ => Content::Blob(entry.id),
            };
            if scope.selects(&entry.path, lookup)? {
                candidates.push((entry.path.clone(), content));
            }
        }
        Ok(candidates)
    }

    fn add_untracked(
        &self,
        candidates: &mut Vec<(Vec<u8>, Content)>,
        scope: &Scope,
        lookup: &mut Option<Lookup>,
        ignored: bool,
    ) -> Result<(), Error> {
        let untracked = |source| Error::Untracked(Box::new(source));
        let never = AtomicBool::new(false);
        let paths: Vec<Vec<u8>> = if ignored {
            self.raw_status_with(
                crate::status::Baseline::Tree(None),
                crate::status::Untracked::RawFilesWithoutIgnores,
                &scope.pathspec,
                crate::status::Limits::default(),
                &never,
            )
            .map_err(untracked)?
            .raw_untracked
        } else {
            let mut rules = self
                .ignore_rules(&scope.environment)
                .map_err(|source| Error::Ignore(Box::new(source)))?;
            // Git's ignore rules hide tracked files too when untracked files are searched.
            let mut kept = Vec::with_capacity(candidates.len());
            for candidate in candidates.drain(..) {
                let hidden = rules
                    .check(&candidate.0, InputKind::File)
                    .map_err(|source| Error::Ignore(Box::new(source)))?
                    .is_some_and(|found| found.ignored);
                if !hidden {
                    kept.push(candidate);
                }
            }
            *candidates = kept;
            self.status_with(
                crate::status::Baseline::Tree(None),
                crate::status::UntrackedFiles::All,
                &rules,
                &scope.pathspec,
                crate::status::Limits::default(),
                &never,
            )
            .map_err(untracked)?
            .untracked
            .into_iter()
            .filter(|entry| entry.kind == crate::status::UntrackedKind::File)
            .map(|entry| entry.path)
            .collect()
        };
        let tracked: std::collections::HashSet<Vec<u8>> =
            candidates.iter().map(|(path, _)| path.clone()).collect();
        for path in paths {
            if !tracked.contains(&path) && scope.selects(&path, lookup)? {
                candidates.push((path, Content::File));
            }
        }
        candidates.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(())
    }
}

/// Searches the regular files below `root` (`git grep --no-index`), in path order.
///
/// Every file is searched, including files ignore rules match; symbolic links are not followed.
/// Paths passed to `visit` are relative to `root`, with `/` separators.
///
/// # Errors
///
/// Returns [`Error::Read`] when a directory or file cannot be read, and the errors of
/// [`Searcher::search`].
pub fn grep_directory(
    root: &Path,
    scope: &Scope,
    searcher: &mut Searcher,
    mut visit: impl FnMut(Found<'_>) -> ControlFlow<()>,
) -> Result<(), Error> {
    let mut files = Vec::new();
    walk_directory(root, &[], scope, &mut files)?;
    files.sort();
    for path in files {
        let native = native_path(root, &path)?;
        let bytes = std::fs::read(&native).map_err(|source| Error::Read {
            path: native.clone(),
            source,
        })?;
        let matches = searcher.search(&path, &bytes)?;
        let found = Found {
            path: &path,
            content: &bytes,
            matches: &matches,
            searcher,
        };
        if visit(found).is_break() {
            break;
        }
    }
    Ok(())
}

fn walk_directory(
    root: &Path,
    directory: &[u8],
    scope: &Scope,
    files: &mut Vec<Vec<u8>>,
) -> Result<(), Error> {
    let native = native_path(root, directory)?;
    let read = |source| Error::Read {
        path: native.clone(),
        source,
    };
    for entry in std::fs::read_dir(&native).map_err(read)? {
        let entry = entry.map_err(read)?;
        let name = entry.file_name();
        let Some(name) = name_bytes(&name) else {
            continue;
        };
        if name == b".git" {
            continue;
        }
        let mut path = directory.to_vec();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(&name);
        let kind = entry.file_type().map_err(read)?;
        if kind.is_dir() {
            if scope.pathspec.directory(&path) != Prune::Skip {
                walk_directory(root, &path, scope, files)?;
            }
        } else if kind.is_file() && scope.selects(&path, &mut None)? {
            files.push(path);
        }
    }
    Ok(())
}

fn name_bytes(name: &std::ffi::OsStr) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(name.as_bytes().to_vec())
    }
    #[cfg(not(unix))]
    {
        name.to_str().map(|name| name.as_bytes().to_vec())
    }
}

fn native_path(root: &Path, path: &[u8]) -> Result<PathBuf, Error> {
    if path.is_empty() {
        return Ok(root.to_owned());
    }
    crate::source_files::native(root, path).ok_or_else(|| Error::Path(path.to_vec()))
}

/// Appends the blobs below tree `id`, whose path is `directory`, that `scope` selects.
fn tree_files(
    objects: &Objects,
    id: ObjectId,
    directory: Vec<u8>,
    scope: &Scope,
    lookup: &mut Option<Lookup>,
    out: &mut Vec<(Vec<u8>, Content)>,
) -> Result<(), Error> {
    let tree = objects.read_tree(id).map_err(Error::object)?;
    for entry in tree.entries() {
        let mut path = directory.clone();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(&entry.name);
        match entry.mode {
            EntryMode::Tree => {
                if scope.pathspec.directory(&path) != Prune::Skip {
                    tree_files(objects, entry.id, path, scope, lookup, out)?;
                }
            }
            EntryMode::Gitlink => {}
            _ => {
                if scope.selects(&path, lookup)? {
                    out.push((path, Content::Blob(entry.id)));
                }
            }
        }
    }
    Ok(())
}

/// Directories already found to be real directories rather than symbolic links.
#[derive(Default)]
struct CheckedDirectories(std::collections::HashSet<Vec<u8>>);

/// Reads a tracked file from the working tree; `None` when it is missing, is not a regular file,
/// or lies beyond a symbolic link, as Git skips such files.
fn read_worktree_file(
    root: &Path,
    path: &[u8],
    checked: &mut CheckedDirectories,
) -> Result<Option<Vec<u8>>, Error> {
    let mut end = 0;
    while let Some(slash) = path
        .get(end..)
        .and_then(|rest| rest.iter().position(|&byte| byte == b'/'))
    {
        end += slash;
        let directory = path.get(..end).unwrap_or_default();
        if !checked.0.contains(directory) {
            let native = native_path(root, directory)?;
            match std::fs::symlink_metadata(&native) {
                Ok(metadata) if metadata.is_dir() => {
                    checked.0.insert(directory.to_vec());
                }
                _ => return Ok(None),
            }
        }
        end += 1;
    }
    let native = native_path(root, path)?;
    match std::fs::symlink_metadata(&native) {
        Ok(metadata) if metadata.is_file() => {}
        _ => return Ok(None),
    }
    match std::fs::read(&native) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(Error::Read {
            path: native,
            source,
        }),
    }
}
