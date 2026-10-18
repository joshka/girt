//! Structural differences between Git trees, before content or rename inference.
//!
//! [`Objects::compare_trees`] answers which leaf paths changed identity or mode. Use this for
//! changed-file lists, staged comparisons or inputs to a patch viewer. Trees store snapshots;
//! renames are inferred separately by [`crate::rewrites`], and line edits come from
//! [`crate::content_diff`]. Both SHA-1 and SHA-256 trees are supported.
//!
//! Paths remain slash-separated bytes. Equal subtree identities skip entire subtrees, so the
//! comparison can be cheap for a small edit in a large tree. That skip does not validate object
//! existence; use content reads or verification when that is required. Changed tree payloads are
//! verified, but leaf targets are not read. Gitlinks remain leaves.
//!
//! See the runnable [`Objects::compare_trees_with`] example for repository setup.
//! [`Objects::compare_trees`] uses default limits and never cancels.
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::pathspec::{Pathspec, Prune};
use crate::{
    EntryMode, ObjectId, ObjectKind, ObjectReadError, Objects, ReadLimits, Tree, TreeError,
};

/// The identity and mode recorded for a leaf path in a tree.
///
/// Regular files, executable files, symlinks and gitlinks are leaves. IDs describe content
/// identity independently of mode: an executable-bit or symlink transition can retain an ID.
/// Gitlinks name commits in another repository, not trees to traverse locally.
///
/// ```
/// use girt::{EntryMode, ObjectFormat, ObjectId, TreeValue};
/// let id = ObjectId::for_blob(ObjectFormat::Sha256, b"#!/bin/sh\n");
///
/// let regular = TreeValue {
///     id,
///     mode: EntryMode::Blob,
/// };
/// let executable = TreeValue {
///     id,
///     mode: EntryMode::Executable,
/// };
///
/// assert_ne!(regular, executable); // Same content can still be a structural change.
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TreeValue {
    /// Referenced object identity; comparison does not read or verify leaf targets.
    pub id: ObjectId,
    /// Recorded entry mode.
    pub mode: EntryMode,
}

/// One added, removed or changed leaf path.
///
/// Use structural comparison for changed-file lists. For patch content, load each change with
/// [`crate::content_diff::BlobContent::read`] and call [`crate::content_diff::diff`]. Use
/// [`crate::rewrites`] to infer relationships across different paths; trees store snapshots rather
/// than rename records. Both SHA-1 and SHA-256 are supported.
///
/// Results from [`Objects::compare_trees`] have at least one side and never have equal sides.
/// `old == None` means addition; `new == None` means removal; two present sides mean change.
/// Compare IDs to distinguish content identity changes from mode/type changes. A mode change
/// alone still produces a record. Trees themselves are omitted, including empty directories.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
///
/// let loose = repo.loose_objects();
/// let blob = loose.write_blob(b"new file\n")?;
/// let tree = loose.write_tree(&Tree::new(
///     ObjectFormat::Sha1,
///     vec![TreeEntry {
///         name: b"new.txt".to_vec(),
///         mode: EntryMode::Blob,
///         id: blob,
///     }],
/// )?)?;
///
/// let changes = repo.objects_with(Default::default())?.compare_trees_with(
///     None,
///     Some(tree),
///     Default::default(),
///     &AtomicBool::new(false),
/// )?;
/// assert!(changes[0].old.is_none());
/// assert_eq!(changes[0].new.unwrap().id, blob);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # File-to-directory replacement
///
/// A replacement is expressed as leaf removal plus descendant additions. Keep both paths when
/// building a patch; there is no synthetic directory change record.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeEntry};
///
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("r");
/// let repo = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
///
/// let loose = repo.loose_objects();
/// let entry = |name: &[u8], id, mode| TreeEntry {
///     name: name.to_vec(),
///     id,
///     mode,
/// };
/// let tree = |entries| loose.write_tree(&Tree::new(ObjectFormat::Sha1, entries)?);
/// let cancel = AtomicBool::new(false);
///
/// let id = loose.write_blob(b"data")?;
/// let old = tree(vec![entry(b"path", id, EntryMode::Blob)])?;
/// let child = tree(vec![entry(b"file", id, EntryMode::Blob)])?;
/// let new = tree(vec![entry(b"path", child, EntryMode::Tree)])?;
///
/// let changes = repo.objects_with(Default::default())?.compare_trees_with(
///     Some(old),
///     Some(new),
///     Default::default(),
///     &cancel,
/// )?;
///
/// assert_eq!(changes.len(), 2);
/// assert_eq!(changes[0].path, b"path");
/// assert!(changes[0].new.is_none());
/// assert_eq!(changes[1].path, b"path/file");
/// assert!(changes[1].old.is_none());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub struct TreeChange {
    /// Relative slash-separated Git path bytes, without UTF-8 conversion or a trailing slash.
    pub path: Vec<u8>,
    /// Previous leaf, absent for additions.
    pub old: Option<TreeValue>,
    /// Replacement leaf, absent for removals.
    pub new: Option<TreeValue>,
}

impl TreeChange {
    /// Classifies this change as Git's `--name-status` does: added, deleted, type-changed or
    /// modified.
    ///
    /// This replaces matching on [`old`](Self::old) and [`new`](Self::new) by hand. A change
    /// between a regular file, a symlink and a gitlink is [`ChangeStatus::TypeChanged`]; an
    /// executable-bit change alone is [`ChangeStatus::Modified`]. A change with neither side,
    /// which [`Objects::compare_trees`] never returns, reports [`ChangeStatus::Modified`].
    ///
    /// ```
    /// use girt::{ChangeStatus, EntryMode, ObjectFormat, ObjectId, TreeChange, TreeValue};
    ///
    /// let id = ObjectId::for_blob(ObjectFormat::Sha1, b"target");
    /// let file = TreeValue {
    ///     id,
    ///     mode: EntryMode::Blob,
    /// };
    /// let link = TreeValue {
    ///     id,
    ///     mode: EntryMode::Symlink,
    /// };
    ///
    /// assert_eq!(ChangeStatus::between(Some(file), Some(link)).letter(), 'T');
    /// assert_eq!(ChangeStatus::between(None, Some(file)), ChangeStatus::Added);
    /// ```
    #[must_use]
    pub fn status(&self) -> ChangeStatus {
        ChangeStatus::between(self.old, self.new)
    }
}

/// How one path changed, with the letter Git prints for it in `--name-status` and
/// `status --porcelain` output.
///
/// Rename and copy letters come from [`rewrites::Kind::letter`](crate::rewrites::Kind::letter).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ChangeStatus {
    /// The path is new (`A`).
    Added,
    /// The path was removed (`D`).
    Deleted,
    /// Content or the executable bit changed (`M`).
    Modified,
    /// The path changed between a regular file, a symlink and a gitlink (`T`).
    TypeChanged,
}

impl ChangeStatus {
    /// Classifies a path from its old and new values, as [`TreeChange::status`] does.
    ///
    /// Use this for leaves that do not come from a tree comparison, such as an index entry
    /// against a working-tree observation.
    #[must_use]
    pub fn between(old: Option<TreeValue>, new: Option<TreeValue>) -> Self {
        match (old, new) {
            (None, Some(_)) => Self::Added,
            (Some(_), None) => Self::Deleted,
            (Some(old), Some(new)) if file_type(old.mode) != file_type(new.mode) => {
                Self::TypeChanged
            }
            _ => Self::Modified,
        }
    }

    /// Returns Git's status letter: `A`, `D`, `M` or `T`.
    #[must_use]
    pub fn letter(self) -> char {
        match self {
            Self::Added => 'A',
            Self::Deleted => 'D',
            Self::Modified => 'M',
            Self::TypeChanged => 'T',
        }
    }
}

/// File types Git distinguishes for `T`; the executable bit is not a type.
#[derive(Eq, PartialEq)]
enum FileType {
    Regular,
    Symlink,
    Gitlink,
    Tree,
}

fn file_type(mode: EntryMode) -> FileType {
    match mode {
        EntryMode::Blob | EntryMode::Executable => FileType::Regular,
        EntryMode::Symlink => FileType::Symlink,
        EntryMode::Gitlink => FileType::Gitlink,
        EntryMode::Tree => FileType::Tree,
    }
}

/// Input and output bounds for one recursive tree comparison.
///
/// Counters charge occurrences, not distinct identities: the same subtree at two paths may be
/// read twice. Equal-ID tree pairs consume no read/entry budget. Bounds are checked before queue
/// growth or result insertion; object bytes are bounded before decoding and entries before pairing.
/// Parsing one bounded payload still allocates proportional entry/name storage before the entry
/// count check. Memory is O(total tree bytes + entries + generated path bytes + changes), plus
/// the existing pack snapshot and per-read decoding workspace. These are not exact heap limits.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{
///     EntryMode, InitKind, ObjectFormat, Repository, Tree, TreeCompareError, TreeCompareLimits,
///     TreeEntry,
/// };
///
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("r");
/// let repo = Repository::init(ObjectFormat::Sha1, path, InitKind::Bare)?;
///
/// let loose = repo.loose_objects();
/// let entry = |name: &[u8], id, mode| TreeEntry {
///     name: name.to_vec(),
///     id,
///     mode,
/// };
/// let tree = |entries| loose.write_tree(&Tree::new(ObjectFormat::Sha1, entries)?);
/// let cancel = AtomicBool::new(false);
///
/// let id = loose.write_blob(b"data")?;
/// let root = tree(vec![entry(b"file", id, EntryMode::Blob)])?;
///
/// let objects = repo.objects_with(Default::default())?;
/// let result = objects.compare_trees_with(
///     None,
///     Some(root),
///     TreeCompareLimits::default().with_max_changes(0),
///     &cancel,
/// );
/// assert!(matches!(result, Err(TreeCompareError::Limit(_))));
///
/// // Equal tree identities skip traversal, even under this output budget.
/// assert!(
///     objects
///         .compare_trees_with(
///             Some(root),
///             Some(root),
///             TreeCompareLimits::default().with_max_changes(0),
///             &cancel
///         )?
///         .is_empty()
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct TreeCompareLimits {
    /// Maximum tree reads across both sides (default 100,000).
    pub max_trees: usize,
    /// Maximum sum of tree payload bytes read across both sides (default 256 MiB).
    pub max_tree_bytes: usize,
    /// Maximum sum of entries in trees read across both sides (default 1,000,000).
    pub max_entries: usize,
    /// Maximum directory depth below the root; zero permits root leaves only (default 1,024).
    pub max_depth: usize,
    /// Maximum sum of generated full path lengths, including unchanged entries (default 64 MiB).
    pub max_path_bytes: usize,
    /// Maximum number of output records (default 1,000,000).
    pub max_changes: usize,
    /// Independent storage reconstruction bounds for each read.
    pub read: ReadLimits,
}

impl TreeCompareLimits {
    /// Sets [`Self::max_trees`].
    #[must_use]
    pub fn with_max_trees(mut self, max_trees: usize) -> Self {
        self.max_trees = max_trees;
        self
    }

    /// Sets [`Self::max_tree_bytes`].
    #[must_use]
    pub fn with_max_tree_bytes(mut self, max_tree_bytes: usize) -> Self {
        self.max_tree_bytes = max_tree_bytes;
        self
    }

    /// Sets [`Self::max_entries`].
    #[must_use]
    pub fn with_max_entries(mut self, max_entries: usize) -> Self {
        self.max_entries = max_entries;
        self
    }

    /// Sets [`Self::max_depth`].
    #[must_use]
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Sets [`Self::max_path_bytes`].
    #[must_use]
    pub fn with_max_path_bytes(mut self, max_path_bytes: usize) -> Self {
        self.max_path_bytes = max_path_bytes;
        self
    }

    /// Sets [`Self::max_changes`].
    #[must_use]
    pub fn with_max_changes(mut self, max_changes: usize) -> Self {
        self.max_changes = max_changes;
        self
    }

    /// Sets [`Self::read`].
    #[must_use]
    pub fn with_read(mut self, read: ReadLimits) -> Self {
        self.read = read;
        self
    }
}

impl Default for TreeCompareLimits {
    fn default() -> Self {
        Self {
            max_trees: 100_000,
            max_tree_bytes: 256 * 1024 * 1024,
            max_entries: 1_000_000,
            max_depth: 1_024,
            max_path_bytes: 64 * 1024 * 1024,
            max_changes: 1_000_000,
            read: ReadLimits::default(),
        }
    }
}

impl Objects {
    /// Compares two tree IDs recursively, using `None` for an empty side.
    ///
    /// Returns leaf additions, removals and changes in ascending lexicographic **raw full-path
    /// byte order** (shorter prefix first). Git tree ordering is validated before pairing entries
    /// by byte name: files and directories with the same name must pair even when their positions
    /// in Git order differ. A file/directory replacement yields a removal/addition at the file
    /// path and additions/removals below the directory. Empty directories yield no leaf records.
    /// No rename/copy detection, text diff, index or worktree access occurs; use
    /// [`Self::compare_trees_matching`] to limit paths.
    ///
    /// Equal subtree IDs, including equal roots, are trusted and skipped without any read.
    /// This establishes structural identity, **not validity or existence**. Other encountered
    /// trees are identity-verified by storage, parsed and structurally validated. Blob, symlink
    /// and gitlink targets are never read, even when changed; dangling or wrong-kind leaf
    /// targets therefore remain unvalidated. Gitlinks are not interpreted as local subtrees.
    ///
    /// I/O and comparison are synchronous and read-only. Traversal uses an explicit stack, not
    /// recursion. Set `cancel` from another thread and leave it set until return. Checks occur
    /// before starting (even for equal roots), between tree reads and entry processing, and before
    /// and after sorting output. Individual reads, parsing, validation and sorting cannot be
    /// interrupted; cancellation is cooperative, without a wall-clock guarantee. Callers using
    /// an async runtime should run the whole operation on a caller-bounded blocking worker.
    ///
    /// # Errors
    ///
    /// Returns [`TreeCompareError`] for a visited missing, wrong-kind, corrupt, malformed or
    /// structurally invalid tree, cancellation, or an exhausted bound. Read/parse failures include
    /// the tree ID and byte path (empty for the root). No partial results or filesystem mutations
    /// are produced. Both object formats and the five modes accepted by [`Tree::parse`] are
    /// supported.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{
    ///     EntryMode, InitKind, ObjectFormat, PackLimits, Repository, Tree, TreeCompareLimits,
    ///     TreeEntry,
    /// };
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let loose = repo.loose_objects();
    /// let tree = |content: &[u8]| -> Result<_, Box<dyn std::error::Error>> {
    ///     let id = loose.write_blob(content)?;
    ///     let entry = TreeEntry {
    ///         mode: EntryMode::Blob,
    ///         name: b"notes.txt".to_vec(),
    ///         id,
    ///     };
    ///     Ok(loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![entry])?)?)
    /// };
    /// let (old, new) = (tree(b"draft\n")?, tree(b"final\n")?);
    ///
    /// let objects = repo.objects_with(PackLimits::default())?;
    /// let cancel = AtomicBool::new(false);
    /// let limits = TreeCompareLimits::default();
    /// let changes = objects.compare_trees_with(Some(old), Some(new), limits, &cancel)?;
    /// assert_eq!(changes.len(), 1);
    /// assert_eq!(changes[0].path, b"notes.txt");
    /// assert_ne!(
    ///     changes[0].old.map(|old| old.id),
    ///     changes[0].new.map(|new| new.id)
    /// );
    ///
    /// // `None` stands for an empty tree on either side.
    /// assert!(
    ///     objects
    ///         .compare_trees_with(None, None, limits, &cancel)?
    ///         .is_empty()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn compare_trees_with(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        limits: TreeCompareLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<TreeChange>, TreeCompareError> {
        self.compare_trees_matching(old, new, &Pathspec::all(), limits, cancel)
    }

    /// [`Self::compare_trees_with`], keeping only changes at paths `paths` selects.
    ///
    /// This is the comparison `git diff-tree -r <old> <new> -- <pathspec>` makes. Subtrees the
    /// pathspec cannot reach are not read, and paths below a subtree it selects whole are not
    /// tested, so a narrow pathspec over a large tree reads only the trees on the way to it.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::pathspec::{Context, Pathspec};
    /// use girt::tree::TreeEditor;
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let loose = repo.loose_objects();
    /// let mut editor = TreeEditor::new(&loose);
    /// editor.set(b"src/lib.rs", loose.write_blob(b"old\n")?)?;
    /// editor.set(b"docs/guide.md", loose.write_blob(b"old\n")?)?;
    /// let old = editor.write()?;
    /// editor.set(b"src/lib.rs", loose.write_blob(b"new\n")?)?;
    /// editor.set(b"docs/guide.md", loose.write_blob(b"new\n")?)?;
    /// let new = editor.write()?;
    ///
    /// let paths = Pathspec::parse(["*.rs"], &Context::new())?;
    /// let objects = repo.objects()?;
    /// let changes = objects.compare_trees_matching(
    ///     Some(old),
    ///     Some(new),
    ///     &paths,
    ///     Default::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// assert_eq!(changes.len(), 1);
    /// assert_eq!(changes[0].path, b"src/lib.rs");
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::compare_trees_with`], and [`TreeCompareError::Unsupported`]
    /// for a pathspec with `attr:` magic, which needs attributes a tree comparison does not load.
    pub fn compare_trees_matching(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        paths: &Pathspec,
        limits: TreeCompareLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<TreeChange>, TreeCompareError> {
        self.compare_trees_among(old, new, &HashMap::new(), paths, limits, cancel)
    }

    /// [`Self::compare_trees_with`], taking trees found in `local` instead of reading them.
    ///
    /// `local` must map each identity to the tree that hashes to it, such as the trees built from
    /// an index. Its trees are not charged to `limits`; trees read from storage still are.
    pub(crate) fn compare_trees_among(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        local: &HashMap<ObjectId, Tree>,
        paths: &Pathspec,
        limits: TreeCompareLimits,
        cancel: &AtomicBool,
    ) -> Result<Vec<TreeChange>, TreeCompareError> {
        if paths.uses_attributes() {
            return Err(TreeCompareError::Unsupported("attr: pathspec magic"));
        }
        for id in old.into_iter().chain(new) {
            id.require_format(self.object_format())?;
        }
        let mut budget = Budget {
            remaining: limits,
            cancel,
            local,
        };
        budget.check()?;
        let mut pending = vec![Directory {
            path: vec![],
            old,
            new,
            depth: 0,
            selected: paths.is_all(),
        }];
        let mut changes = Vec::new();
        while let Some(directory) = pending.pop() {
            budget.check()?;
            if directory.old == directory.new {
                continue;
            }
            let old = budget.read(self, directory.old, &directory.path)?;
            let new = budget.read(self, directory.new, &directory.path)?;
            // Pair by literal name, not Git's mode-dependent ordering. This handles a file/tree
            // replacement even when siblings sort between its old and new positions.
            let pairs = pair_entries(&old, &new, cancel)?;
            for (name, (old, new)) in pairs {
                budget.check()?;
                let length = directory
                    .path
                    .len()
                    .checked_add(name.len())
                    .and_then(|n| n.checked_add(usize::from(!directory.path.is_empty())))
                    .ok_or(TreeCompareError::Limit("path bytes"))?;
                charge(&mut budget.remaining.max_path_bytes, length, "path bytes")?;
                let mut path = Vec::with_capacity(length);
                path.extend_from_slice(&directory.path);
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(name);

                let old_tree = tree_id(old);
                let new_tree = tree_id(new);
                if old_tree != new_tree {
                    let prune = if directory.selected {
                        Prune::All
                    } else {
                        paths.directory(&path)
                    };
                    if prune != Prune::Skip {
                        if directory.depth == limits.max_depth {
                            return Err(TreeCompareError::Limit("depth"));
                        }
                        pending.push(Directory {
                            path: path.clone(),
                            old: old_tree,
                            new: new_tree,
                            depth: directory.depth + 1,
                            selected: prune == Prune::All,
                        });
                    }
                }
                let old = old.filter(|value| value.mode != EntryMode::Tree);
                let new = new.filter(|value| value.mode != EntryMode::Tree);
                if old != new && (directory.selected || paths.matches_unchecked(&path)) {
                    charge(&mut budget.remaining.max_changes, 1, "changes")?;
                    changes.push(TreeChange { path, old, new });
                }
            }
        }
        budget.check()?;
        changes.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        budget.check()?;
        Ok(changes)
    }

    /// Compares two trees with [`TreeCompareLimits::default`] and no cancellation.
    ///
    /// Equivalent to `self.compare_trees_with(old, new, TreeCompareLimits::default(), &never)` with
    /// a flag that is never set; see [`Self::compare_trees_with`] for ordering and
    /// verification.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::compare_trees_with`] except cancellation.
    pub fn compare_trees(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
    ) -> Result<Vec<TreeChange>, TreeCompareError> {
        self.compare_trees_with(
            old,
            new,
            TreeCompareLimits::default(),
            &AtomicBool::new(false),
        )
    }
}

fn tree_id(value: Option<TreeValue>) -> Option<ObjectId> {
    value
        .filter(|value| value.mode == EntryMode::Tree)
        .map(|value| value.id)
}

type EntryPairs<'a> = BTreeMap<&'a [u8], (Option<TreeValue>, Option<TreeValue>)>;

/// Pure name alignment; borrowed names live only until this directory has been processed.
fn pair_entries<'a>(
    old: &'a Tree,
    new: &'a Tree,
    cancel: &AtomicBool,
) -> Result<EntryPairs<'a>, TreeCompareError> {
    let mut pairs: EntryPairs<'_> = BTreeMap::new();
    for entry in old.entries() {
        check_cancel(cancel)?;
        pairs.entry(&entry.name).or_default().0 = Some(TreeValue {
            id: entry.id,
            mode: entry.mode,
        });
    }
    for entry in new.entries() {
        check_cancel(cancel)?;
        pairs.entry(&entry.name).or_default().1 = Some(TreeValue {
            id: entry.id,
            mode: entry.mode,
        });
    }
    Ok(pairs)
}

struct Directory {
    path: Vec<u8>,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    depth: usize,
    /// The pathspec selects everything below, so paths need no further tests.
    selected: bool,
}

struct Budget<'a> {
    remaining: TreeCompareLimits,
    cancel: &'a AtomicBool,
    local: &'a HashMap<ObjectId, Tree>,
}

impl Budget<'_> {
    fn check(&self) -> Result<(), TreeCompareError> {
        check_cancel(self.cancel)
    }

    fn read(
        &mut self,
        objects: &Objects,
        id: Option<ObjectId>,
        path: &[u8],
    ) -> Result<Tree, TreeCompareError> {
        self.check()?;
        let Some(id) = id else {
            return Ok(Tree::new(objects.object_format(), vec![]).expect("empty tree"));
        };
        if let Some(tree) = self.local.get(&id) {
            return Ok(tree.clone());
        }
        charge(&mut self.remaining.max_trees, 1, "trees")?;
        let mut read = self.remaining.read;
        read.max_object_bytes = read.max_object_bytes.min(self.remaining.max_tree_bytes);
        let object = objects
            .read_with(id, read)
            .map_err(|source| TreeCompareError::Read {
                id,
                path: path.to_vec(),
                source: Box::new(source),
            })?
            .ok_or_else(|| TreeCompareError::Missing {
                id,
                path: path.to_vec(),
            })?;
        self.check()?;
        if object.kind() != ObjectKind::Tree {
            return Err(TreeCompareError::NotTree {
                id,
                path: path.to_vec(),
                actual: object.kind(),
            });
        }
        charge(
            &mut self.remaining.max_tree_bytes,
            object.data().len(),
            "tree bytes",
        )?;
        let invalid = |source| TreeCompareError::Invalid {
            id,
            path: path.to_vec(),
            source,
        };
        let tree = Tree::parse(object.object_format(), object.data()).map_err(invalid)?;
        charge(
            &mut self.remaining.max_entries,
            tree.entries().len(),
            "entries",
        )?;
        tree.validate().map_err(invalid)?;
        self.check()?;
        Ok(tree)
    }
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), TreeCompareError> {
    if cancel.load(Ordering::Relaxed) {
        Err(TreeCompareError::Cancelled)
    } else {
        Ok(())
    }
}

fn charge(remaining: &mut usize, count: usize, name: &'static str) -> Result<(), TreeCompareError> {
    *remaining = remaining
        .checked_sub(count)
        .ok_or(TreeCompareError::Limit(name))?;
    Ok(())
}

/// A structural comparison failed; no partial result is returned.
///
/// A missing or invalid tree needs storage recovery; raising a traversal bound does not repair
/// it. Preserve the ID/path and source chain when reporting [`TreeCompareError::Read`] or
/// [`TreeCompareError::Invalid`]. Cancellation and limits discard the whole result.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::{InitKind, ObjectFormat, Repository, TreeCompareError};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
///
/// let objects = repo.objects_with(Default::default())?;
/// assert!(matches!(
///     objects.compare_trees_with(None, None, Default::default(), &AtomicBool::new(true)),
///     Err(TreeCompareError::Cancelled)
/// ));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TreeCompareError {
    /// A root uses a different object format from the reader.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// A tree to be traversed is absent.
    #[error("missing tree {id} at {path:?}")]
    Missing {
        /// Expected tree identity.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
    },
    /// A visited tree reference resolves to a different object kind.
    #[error("expected tree {id} at {path:?}, found {actual:?}")]
    NotTree {
        /// Expected tree identity.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
        /// Object kind found in storage.
        actual: ObjectKind,
    },
    /// Storage, identity verification or per-read decoding failed.
    ///
    /// The effective object-byte limit also includes the remaining cumulative tree-byte budget.
    #[error("reading tree {id} at {path:?}")]
    Read {
        /// Tree being read.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
        /// Underlying storage failure. Boxed to keep contextual errors compact.
        #[source]
        source: Box<ObjectReadError>,
    },
    /// A visited tree has unsupported syntax, invalid names, duplicates or invalid Git ordering.
    #[error("invalid tree {id} at {path:?}")]
    Invalid {
        /// Tree being parsed or validated.
        id: ObjectId,
        /// Relative directory path bytes; empty for a root.
        path: Vec<u8>,
        /// Parsing or structural validation failure.
        #[source]
        source: TreeError,
    },
    /// A named traversal or output bound was exceeded.
    #[error("tree comparison limit exceeded: {0}")]
    Limit(&'static str),
    /// The request needs something tree comparison does not provide.
    #[error("unsupported in tree comparison: {0}")]
    Unsupported(&'static str),
    /// The caller requested cancellation at a cooperative checkpoint.
    #[error("tree comparison cancelled")]
    Cancelled,
}

#[cfg(test)]
mod tests;
