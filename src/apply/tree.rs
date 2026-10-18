use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::{Applied, Limits, Patch};
use crate::{EntryMode, ObjectId, Objects, ReadLimits, Tree, TreeEntry, TreeValue};

/// Path mapping for tree application. No filesystem paths are opened.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TreeOptions {
    /// Components removed from each patch path (default one, removing `a/` or `b/`).
    pub strip_components: usize,
    /// Repository-relative directory prepended after stripping (default empty).
    pub directory: Vec<u8>,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            strip_components: 1,
            directory: Vec::new(),
        }
    }
}

/// Aggregate bounds for tree application, in addition to per-file content limits.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct TreeLimits {
    /// Per-file text application bounds.
    pub content: Limits,
    /// Reconstruction bounds for each existing tree/blob read.
    pub read: ReadLimits,
    /// Combined encoded bytes of directories read (default 64 MiB).
    pub max_tree_bytes: usize,
    /// Combined entries in directories read (default 1,000,000).
    pub max_entries: usize,
    /// Maximum mapped file path components (default 128).
    pub max_depth: usize,
    /// Sum of each old/new mapped path (counting equal sides twice) and each visited directory
    /// path (default 16 MiB). The conservative bound is checked before mapping allocations.
    pub max_path_bytes: usize,
    /// Combined generated blob and encoded tree bytes (default 64 MiB).
    pub max_generated_bytes: usize,
}

impl Default for TreeLimits {
    fn default() -> Self {
        Self {
            content: Limits::default(),
            read: ReadLimits::default(),
            max_tree_bytes: 64 * 1024 * 1024,
            max_entries: 1_000_000,
            max_depth: 128,
            max_path_bytes: 16 * 1024 * 1024,
            max_generated_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Unpublished replacement tree and generated objects. Unchanged subtrees retain their identities.
///
/// Computing this value writes nothing. [`Self::install_objects`] adds its generated objects;
/// references, the index and working files remain caller-owned. Installation may leave some
/// additive objects on failure, but never publishes the root through a reference.
#[derive(Clone, Debug)]
pub struct AppliedTree {
    tree: Tree,
    blobs: Vec<Vec<u8>>,
    trees: Vec<Tree>,
    files: Vec<AppliedFile>,
}

impl AppliedTree {
    /// Candidate root. Its ID is publishable only after generated objects are installed.
    pub fn tree(&self) -> &Tree {
        &self.tree
    }

    /// Results in patch order, with mapped repository-relative paths.
    pub fn files(&self) -> &[AppliedFile] {
        &self.files
    }

    /// Installs generated objects without changing references, index entries or working files.
    ///
    /// # Errors
    ///
    /// Rejects a destination of another object format before writing. Storage failure can leave
    /// previously installed immutable objects; retrying the same installation is idempotent.
    pub fn install_objects(&self, loose: &crate::LooseObjects) -> Result<(), TreeError> {
        if loose.object_format() != self.tree.object_format() {
            return Err(TreeError::ObjectFormat);
        }
        for bytes in &self.blobs {
            loose.write_blob(bytes)?;
        }
        for tree in &self.trees {
            loose.write_tree(tree)?;
        }
        Ok(())
    }
}

/// One file's computed tree edit, including deletion results.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct AppliedFile {
    /// Mapped destination path, or source for a deletion; never a filesystem path.
    pub path: Vec<u8>,
    /// Original source path, absent for additions; copies retain this entry.
    pub source_path: Option<Vec<u8>>,
    /// Original entry, absent for an addition.
    pub old: Option<TreeValue>,
    /// Replacement entry, absent for a deletion.
    pub new: Option<TreeValue>,
    /// Applied text hunk locations.
    pub hunks: Vec<super::HunkResult>,
}

/// Tree application or explicit object installation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TreeError {
    /// An existing directory could not be decoded.
    #[error("reading patch tree")]
    ReadTree(#[from] crate::ReadTreeError),
    /// An existing blob could not be read.
    #[error("reading patch preimage")]
    ReadBlob(#[from] crate::ReadBlobError),
    /// A generated tree is invalid.
    #[error("constructing patch tree")]
    Tree(#[from] crate::TreeError),
    /// Explicit additive installation stopped.
    #[error("installing patch objects")]
    Write(#[from] crate::loose::Error),
    /// Content validation failed for a mapped path.
    #[error("applying patch content")]
    Content {
        /// Mapped byte path.
        path: Vec<u8>,
        /// Hunk or content-limit failure.
        #[source]
        source: super::Error,
    },
    /// A path is invalid, changes its name, traverses a leaf, or violates presence preconditions.
    #[error("patch path precondition failed")]
    Path(Vec<u8>),
    /// Existing entry has an unsupported mode or differs from the declared source mode.
    #[error("patch entry mode is unsupported")]
    Mode,
    /// Computation exceeded a bound without returning a partial artifact.
    #[error("tree patch limit exceeded: {0}")]
    Limit(&'static str),
    /// Computation observed cancellation without publishing objects.
    #[error("tree patch cancelled")]
    Cancelled,
    /// Artifact and installation destination use different object formats.
    #[error("patch object format mismatch")]
    ObjectFormat,
}

impl Patch {
    /// Applies text edits to a stored tree in memory with default mapping and bounds.
    ///
    /// Wraps [`Self::apply_tree_with`] with default options, limits and no cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] on invalid paths, missing sources, existing addition destinations,
    /// unsupported modes, failed hunks or exhausted bounds. No objects are written.
    ///
    /// ```
    /// use girt::apply::Patch;
    /// use girt::{InitKind, ObjectFormat, Repository, Tree};
    ///
    /// let temporary = tempfile::tempdir()?;
    /// let repository = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     temporary.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let loose = repository.loose_objects();
    /// let base = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// let patch = Patch::parse(b"--- /dev/null\n+++ b/file\n@@ -0,0 +1 @@\n+hello\n")?;
    ///
    /// let result = patch.apply_tree(&repository.objects()?, base)?;
    /// assert_eq!(result.files()[0].path, b"file");
    /// result.install_objects(&loose)?;
    /// assert_eq!(
    ///     repository.objects()?.read_tree(result.tree().id())?,
    ///     result.tree().clone()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn apply_tree(&self, objects: &Objects, base: ObjectId) -> Result<AppliedTree, TreeError> {
        self.apply_tree_with(
            objects,
            base,
            TreeOptions::default(),
            TreeLimits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Computes a tree replacement without writing objects, index entries or working files.
    ///
    /// Reads affected directories and preimage blobs, keeping unrelated subtrees opaque. All
    /// mapped paths must be relative, nonempty, NUL-free and free of dot or `.git` components
    /// (ignoring ASCII case for `.git`). Differing names require rename/copy metadata. Declared
    /// modes are checked and applied; otherwise existing modes are preserved and additions use
    /// regular-file mode. Directories and gitlinks cannot be replaced or traversed as files.
    /// Empty parent directories are pruned after deletion. Duplicate destinations, repeated
    /// non-copy sources and replacing an emptied directory are refused. All sources are read
    /// before path edits, so rename cycles use original preimages and copies retain their sources.
    /// Filesystem-specific metadata aliases remain the responsibility of later checkout.
    ///
    /// Cancellation is checked between files, directory reads and generated trees. Individual
    /// bounded reads and exact content application cannot be interrupted. Callers publishing the
    /// result must retain access to unchanged source objects and install generated objects first.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] without external effects or a partial artifact on computation failure.
    pub fn apply_tree_with(
        &self,
        objects: &Objects,
        base: ObjectId,
        options: TreeOptions,
        limits: TreeLimits,
        cancel: &AtomicBool,
    ) -> Result<AppliedTree, TreeError> {
        let mut state = State {
            objects,
            limits,
            cancel,
            directories: BTreeMap::new(),
            tree_bytes: 0,
            entries: 0,
            generated: 0,
            path_bytes: 0,
        };

        if self.files().len() > limits.content.max_files {
            return Err(TreeError::Limit("files"));
        }
        let (mappings, removals) = map_files(self, &options, &mut state)?;
        state.load(Vec::new(), base)?;
        apply_mapped(state, mappings, removals)
    }
}

fn apply_mapped(
    mut state: State<'_>,
    mappings: Vec<MappedFile<'_>>,
    removals: Paths,
) -> Result<AppliedTree, TreeError> {
    let limits = state.limits;
    let objects = state.objects;
    let mut files = Vec::new();
    let mut blobs = Vec::new();
    let mut replacements = Vec::new();
    for mapping in &mappings {
        state.check()?;
        if mapping
            .file
            .binary
            .as_ref()
            .is_some_and(|binary| binary.old.format() != objects.object_format())
        {
            return Err(TreeError::ObjectFormat);
        }
        let path = mapping.new.as_ref().or(mapping.old.as_ref()).unwrap();
        let old = if let Some(source) = &mapping.old {
            let (parent, name) = split(source);
            state.ensure_directory(parent, None)?;
            Some(
                *state.directories[parent]
                    .get(name)
                    .ok_or_else(|| TreeError::Path(source.clone()))?,
            )
        } else {
            None
        };
        if let Some(destination) = &mapping.new {
            let (parent, name) = split(destination);
            state.ensure_directory(parent, Some(&removals))?;
            let existing = state.directories[parent].get(name);
            if state.directories.contains_key(destination)
                || existing.is_some_and(|value| value.mode == EntryMode::Tree)
                || (existing.is_some()
                    && mapping.old.as_ref() != Some(destination)
                    && !removals.contains(destination))
            {
                return Err(TreeError::Path(destination.clone()));
            }
        }
        if mapping
            .file
            .old_mode
            .is_some_and(|mode| old.is_none_or(|value| value.mode != mode))
        {
            return Err(TreeError::Mode);
        }
        let (preimage, mode) = state.preimage(old)?;
        let mut content_limits = limits.content;
        let remaining = limits.max_generated_bytes.saturating_sub(state.generated);
        content_limits.max_output_bytes = content_limits.max_output_bytes.min(remaining);
        let Applied { content, hunks } = mapping
            .file
            .apply_with(&preimage, content_limits)
            .map_err(|source| {
                if source == super::Error::Limit(super::text::OUTPUT_BYTES)
                    && remaining < limits.content.max_output_bytes
                {
                    TreeError::Limit("generated bytes")
                } else {
                    TreeError::Content {
                        path: path.clone(),
                        source,
                    }
                }
            })?;
        let new = if mapping.new.is_some() {
            state.charge(content.len())?;
            let id = ObjectId::for_blob(objects.object_format(), &content);
            blobs.push(content);
            Some(TreeValue {
                mode: mapping.file.new_mode.unwrap_or(mode),
                id,
            })
        } else {
            None
        };
        replacements.push(new);
        files.push(AppliedFile {
            path: path.clone(),
            source_path: mapping.old.clone(),
            old,
            new,
            hunks,
        });
    }
    for path in &removals {
        let (parent, name) = split(path);
        state.directories.get_mut(parent).unwrap().remove(name);
    }
    for (mapping, value) in mappings.iter().zip(replacements) {
        if let (Some(path), Some(value)) = (&mapping.new, value) {
            let (parent, name) = split(path);
            let directory = state.directories.get_mut(parent).unwrap();
            if !directory.contains_key(name) {
                state.entries = state
                    .entries
                    .checked_add(1)
                    .ok_or(TreeError::Limit("directory entries"))?;
                if state.entries > limits.max_entries {
                    return Err(TreeError::Limit("directory entries"));
                }
            }
            directory.insert(name.to_vec(), value);
        }
    }
    let trees = state.build()?;
    let tree = trees.last().unwrap().clone();
    Ok(AppliedTree {
        tree,
        trees,
        blobs,
        files,
    })
}

/// Seeds the same planner from an already validated, fully expanded stage-zero index.
pub(super) fn apply_index_tree(
    patch: &Patch,
    objects: &Objects,
    index: &crate::index::Index,
    options: TreeOptions,
    limits: TreeLimits,
    cancel: &AtomicBool,
) -> Result<AppliedTree, TreeError> {
    if index.object_format() != objects.object_format() {
        return Err(TreeError::ObjectFormat);
    }
    if patch.files().len() > limits.content.max_files {
        return Err(TreeError::Limit("files"));
    }
    let mut state = State {
        objects,
        limits,
        cancel,
        directories: BTreeMap::new(),
        tree_bytes: 0,
        entries: 0,
        generated: 0,
        path_bytes: 0,
    };
    let (mappings, removals) = map_files(patch, &options, &mut state)?;
    state.directories.insert(Vec::new(), Directory::new());
    let identity = TreeOptions {
        strip_components: 0,
        directory: Vec::new(),
    };
    let empty = Paths::new();
    for entry in index.entries() {
        state.check()?;
        let path = state.map_path(&entry.path, &identity)?;
        let (parent, name) = split(&path);
        state.ensure_directory(parent, Some(&empty))?;
        if state.entries >= limits.max_entries {
            return Err(TreeError::Limit("directory entries"));
        }
        state.entries += 1;
        let mode = match entry.mode {
            crate::index::Mode::Regular => EntryMode::Blob,
            crate::index::Mode::Executable => EntryMode::Executable,
            crate::index::Mode::Symlink => EntryMode::Symlink,
            crate::index::Mode::Gitlink => EntryMode::Gitlink,
            crate::index::Mode::SparseDirectory => return Err(TreeError::Mode),
        };
        state
            .directories
            .get_mut(parent)
            .unwrap()
            .insert(name.to_vec(), TreeValue { mode, id: entry.id });
    }
    apply_mapped(state, mappings, removals)
}

struct MappedFile<'a> {
    file: &'a super::FilePatch,
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
}

type Paths = std::collections::BTreeSet<Vec<u8>>;

fn map_files<'a>(
    patch: &'a Patch,
    options: &TreeOptions,
    state: &mut State<'_>,
) -> Result<(Vec<MappedFile<'a>>, Paths), TreeError> {
    let mut mappings = Vec::new();
    let mut removals = Paths::new();
    let mut destinations = Paths::new();
    let mut sources = Paths::new();
    for file in patch.files() {
        state.check()?;
        let old = file
            .old_path()
            .map(|path| state.map_path(path, options))
            .transpose()?;
        let new = file
            .new_path()
            .map(|path| state.map_path(path, options))
            .transpose()?;
        let path = new.as_ref().or(old.as_ref()).unwrap();
        if file.operation == super::FileOperation::Edit
            && old.is_some()
            && new.is_some()
            && old != new
        {
            return Err(TreeError::Path(path.clone()));
        }
        if file.operation != super::FileOperation::Edit && old == new {
            return Err(TreeError::Path(path.clone()));
        }
        if let Some(destination) = &new
            && !destinations.insert(destination.clone())
        {
            return Err(TreeError::Path(destination.clone()));
        }
        if file.operation != super::FileOperation::Copy
            && let Some(source) = &old
        {
            if !sources.insert(source.clone()) {
                return Err(TreeError::Path(source.clone()));
            }
            if old != new {
                removals.insert(source.clone());
            }
        }
        mappings.push(MappedFile { file, old, new });
    }
    for path in &destinations {
        let mut parent = split(path).0;
        while !parent.is_empty() {
            if destinations.contains(parent) {
                return Err(TreeError::Path(path.clone()));
            }
            parent = split(parent).0;
        }
    }
    Ok((mappings, removals))
}

type Directory = BTreeMap<Vec<u8>, TreeValue>;

struct State<'a> {
    objects: &'a Objects,
    limits: TreeLimits,
    cancel: &'a AtomicBool,
    directories: BTreeMap<Vec<u8>, Directory>,
    tree_bytes: usize,
    entries: usize,
    generated: usize,
    path_bytes: usize,
}

impl State<'_> {
    fn map_path(&mut self, path: &[u8], options: &TreeOptions) -> Result<Vec<u8>, TreeError> {
        let remaining = self.limits.max_path_bytes.saturating_sub(self.path_bytes);
        let mapped = mapped(path, options, self.limits, remaining)?;
        self.path_bytes += mapped.len();
        Ok(mapped)
    }

    fn check(&self) -> Result<(), TreeError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(TreeError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn load(&mut self, path: Vec<u8>, id: ObjectId) -> Result<(), TreeError> {
        self.check()?;
        let mut read = self.limits.read;
        read.max_object_bytes = read
            .max_object_bytes
            .min(self.limits.max_tree_bytes.saturating_sub(self.tree_bytes));
        let object = self
            .objects
            .read_with(id, read)
            .map_err(crate::ReadTreeError::Read)?
            .ok_or(crate::ReadTreeError::Missing(id))?;
        if object.kind() != crate::ObjectKind::Tree {
            return Err(crate::ReadTreeError::Kind {
                id,
                kind: object.kind(),
            }
            .into());
        }
        self.tree_bytes += object.data().len();
        let tree = Tree::parse(self.objects.object_format(), object.data())?;
        tree.validate()?;
        if tree.entries().len() > self.limits.max_entries.saturating_sub(self.entries) {
            return Err(TreeError::Limit("directory entries"));
        }
        self.entries += tree.entries().len();
        let entries = tree
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.name.clone(),
                    TreeValue {
                        mode: entry.mode,
                        id: entry.id,
                    },
                )
            })
            .collect();
        self.directories.insert(path, entries);
        Ok(())
    }

    fn ensure_directory(&mut self, path: &[u8], removals: Option<&Paths>) -> Result<(), TreeError> {
        let mut parent = Vec::new();
        for name in path.split(|b| *b == b'/').filter(|name| !name.is_empty()) {
            let value = self.directories[&parent].get(name).copied();
            let mut child = parent.clone();
            if !child.is_empty() {
                child.push(b'/');
            }
            child.extend_from_slice(name);
            if !self.directories.contains_key(&child) {
                if child.len() > self.limits.max_path_bytes.saturating_sub(self.path_bytes) {
                    return Err(TreeError::Limit("path bytes"));
                }
                self.path_bytes += child.len();
                match value {
                    Some(value) if value.mode == EntryMode::Tree => {
                        self.load(child.clone(), value.id)?
                    }
                    _ if removals
                        .is_some_and(|removed| value.is_none() || removed.contains(&child)) =>
                    {
                        self.entries = self
                            .entries
                            .checked_add(1)
                            .ok_or(TreeError::Limit("directory entries"))?;
                        if self.entries > self.limits.max_entries {
                            return Err(TreeError::Limit("directory entries"));
                        }
                        self.directories.insert(child.clone(), Directory::new());
                    }
                    _ => return Err(TreeError::Path(child)),
                }
            }
            parent = child;
        }
        Ok(())
    }

    fn preimage(&self, value: Option<TreeValue>) -> Result<(Vec<u8>, EntryMode), TreeError> {
        let Some(value) = value else {
            return Ok((Vec::new(), EntryMode::Blob));
        };
        if !matches!(
            value.mode,
            EntryMode::Blob | EntryMode::Executable | EntryMode::Symlink
        ) {
            return Err(TreeError::Mode);
        }
        let mut read = self.limits.read;
        read.max_object_bytes = read
            .max_object_bytes
            .min(self.limits.content.max_input_bytes);
        Ok((self.objects.read_blob_with(value.id, read)?, value.mode))
    }

    fn charge(&mut self, size: usize) -> Result<(), TreeError> {
        if size
            > self
                .limits
                .max_generated_bytes
                .saturating_sub(self.generated)
        {
            return Err(TreeError::Limit("generated bytes"));
        }
        self.generated += size;
        Ok(())
    }

    fn build(mut self) -> Result<Vec<Tree>, TreeError> {
        let mut trees = Vec::new();
        while let Some((path, entries)) = self.directories.pop_last() {
            self.check()?;
            let entries = entries
                .into_iter()
                .map(|(name, value)| TreeEntry {
                    name,
                    mode: value.mode,
                    id: value.id,
                })
                .collect();
            let tree = Tree::new(self.objects.object_format(), entries)?;
            let encoded_len = tree.entries().iter().try_fold(0usize, |total, entry| {
                total
                    .checked_add(
                        entry.mode.octal().len() + 2 + entry.name.len() + entry.id.as_bytes().len(),
                    )
                    .ok_or(TreeError::Limit("generated bytes"))
            })?;
            self.charge(encoded_len)?;
            if !path.is_empty() {
                let (parent, name) = split(&path);
                let directory = self.directories.get_mut(parent).unwrap();
                if tree.entries().is_empty() {
                    directory.remove(name);
                } else {
                    directory.insert(
                        name.to_vec(),
                        TreeValue {
                            mode: EntryMode::Tree,
                            id: tree.id(),
                        },
                    );
                }
            }
            trees.push(tree);
        }
        Ok(trees)
    }
}

fn mapped(
    path: &[u8],
    options: &TreeOptions,
    limits: TreeLimits,
    remaining: usize,
) -> Result<Vec<u8>, TreeError> {
    validate(path)?;
    if !options.directory.is_empty() {
        validate(&options.directory)?;
    }
    let mut stripped = path;
    for _ in 0..options.strip_components {
        let slash = stripped
            .iter()
            .position(|b| *b == b'/')
            .ok_or_else(|| TreeError::Path(path.to_vec()))?;
        stripped = &stripped[slash + 1..];
    }
    let separator = usize::from(!options.directory.is_empty());
    let length = stripped
        .len()
        .saturating_add(options.directory.len())
        .saturating_add(separator);
    if length > remaining {
        return Err(TreeError::Limit("path bytes"));
    }
    let mut result = options.directory.clone();
    if separator != 0 {
        result.push(b'/');
    }
    result.extend_from_slice(stripped);
    if result.split(|b| *b == b'/').count() > limits.max_depth {
        return Err(TreeError::Limit("path depth"));
    }
    validate(&result)?;
    Ok(result)
}

fn validate(path: &[u8]) -> Result<(), TreeError> {
    if path.split(|b| *b == b'/').any(|name| {
        name.is_empty()
            || name == b"."
            || name == b".."
            || name.contains(&0)
            || name.eq_ignore_ascii_case(b".git")
    }) {
        return Err(TreeError::Path(path.to_vec()));
    }
    Ok(())
}

fn split(path: &[u8]) -> (&[u8], &[u8]) {
    path.iter()
        .rposition(|b| *b == b'/')
        .map_or((&b""[..], path), |at| (&path[..at], &path[at + 1..]))
}
