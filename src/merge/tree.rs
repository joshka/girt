use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{ContentMerge, ContentOptions, MarkerOptions, content_with};
use crate::{EntryMode, IdSet, ObjectId, ObjectKind, Objects, Tree, TreeEntry, TreeValue};

/// Policy and traversal bounds for path-based tree merging.
///
/// This operation does not infer renames. Callers requiring Git rename semantics must detect
/// and reconcile renamed paths before using this primitive. Unchanged subtree identities are
/// reused without recursively validating their descendants.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct TreeOptions {
    /// Blob comparison and favoring policy.
    pub content: ContentOptions,
    /// Maximum nested directory depth (default 128; root is depth zero).
    pub max_depth: usize,
    /// Combined entries read from all three sides, including repeated reads (default 1,000,000).
    pub max_entries: usize,
}

impl Default for TreeOptions {
    fn default() -> Self {
        Self {
            content: ContentOptions::default(),
            max_depth: 128,
            max_entries: 1_000_000,
        }
    }
}

/// Structural or content reason a path needs caller resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConflictKind {
    /// Both sides added different entries at one path.
    AddAdd,
    /// One side deleted an entry the other modified.
    ModifyDelete,
    /// Regular file text or binary payloads conflict.
    Content,
    /// Entry kinds differ, including directory/file conflicts.
    Type,
    /// Executable modes cannot be reconciled independently of content.
    Mode,
    /// Symbolic link targets differ.
    Symlink,
    /// Submodule commit identities differ; the submodule is not opened.
    Gitlink,
    /// One source was renamed to two different destinations.
    RenameRename,
    /// One side renamed a path the other deleted.
    RenameDelete,
    /// An addition was relocated by an inferred directory rename.
    DirectoryRename,
}

/// Original entries for one unresolved path, independent of provisional tree content.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TreeConflict {
    /// Repository-relative byte path without a leading slash.
    pub path: Vec<u8>,
    /// Why this path needs resolution.
    pub kind: ConflictKind,
    /// Original common ancestor entry; absent for additions.
    pub base: Option<TreeValue>,
    /// Original our entry; absent when deleted.
    pub ours: Option<TreeValue>,
    /// Original their entry; absent when deleted.
    pub theirs: Option<TreeValue>,
    /// Content-level hunks when regular file payloads were compared.
    pub content: Option<ContentMerge>,
    /// Original rename source, when the conflict arose from path inference.
    pub rename_source: Option<Vec<u8>>,
    /// Per-stage paths for conflicts with distinct destinations; otherwise [`Self::path`].
    ///
    /// Ordered base/ours/theirs. Missing sides ignore their corresponding path.
    pub stage_paths: Option<[Vec<u8>; 3]>,
}

impl TreeConflict {
    /// Converts leaf conflict sides to index stages 1–3 without publishing an index.
    ///
    /// # Errors
    ///
    /// Directory/file conflicts return [`TreeError::DirectoryStages`]; callers must first
    /// expand the directory side and choose conflict paths. Other stages preserve original IDs
    /// and modes, even when the candidate tree has a different provisional blob.
    pub fn index_entries(&self) -> Result<Vec<crate::index::Entry>, TreeError> {
        use crate::index::{Entry, Mode, Stage};
        let mut entries = Vec::new();
        for (position, (side, stage)) in [
            (self.base, Stage::Base),
            (self.ours, Stage::Ours),
            (self.theirs, Stage::Theirs),
        ]
        .into_iter()
        .enumerate()
        {
            let Some(side) = side else {
                continue;
            };
            let mode = match side.mode {
                EntryMode::Blob => Mode::Regular,
                EntryMode::Executable => Mode::Executable,
                EntryMode::Symlink => Mode::Symlink,
                EntryMode::Gitlink => Mode::Gitlink,
                EntryMode::Tree => return Err(TreeError::DirectoryStages),
            };
            let path = self
                .stage_paths
                .as_ref()
                .map_or(&self.path, |paths| &paths[position]);
            entries.push(Entry::new(path.clone(), mode, side.id).with_stage(stage));
        }
        Ok(entries)
    }
}

/// Candidate root tree, original conflicts and newly computed objects, all owned in memory.
///
/// No object is written during computation. Install generated blobs and trees before publishing
/// the root identity. Existing entries reference the input object store. A conflicted candidate
/// uses ours (or theirs if ours is absent) as a provisional entry, while [`Self::conflicts`]
/// retains original sides. It is not a resolved tree and does not contain marker blobs.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct TreeMerge {
    /// Candidate root; valid for publication only after installing generated objects.
    pub tree: Tree,
    /// Unresolved paths in traversal order.
    pub conflicts: Vec<TreeConflict>,
    /// New blob payloads, with their identities; clean content merges only.
    pub blobs: Vec<(ObjectId, Vec<u8>)>,
    /// New subtree objects in child-before-parent order, including the root.
    pub trees: Vec<Tree>,
}

impl TreeMerge {
    /// Whether every path resolved under the selected policy.
    pub fn is_clean(&self) -> bool {
        self.conflicts.is_empty()
    }
}

/// Tree computation failed without writing objects, references, index or working files.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TreeError {
    /// Reading an input directory failed.
    #[error("reading merge tree")]
    ReadTree(#[from] crate::ReadTreeError),
    /// Reading a regular-file payload failed.
    #[error("reading merge blob")]
    ReadBlob(#[from] crate::ReadBlobError),
    /// Generated tree validation failed, including mixed object formats.
    #[error("constructing merge tree")]
    Tree(#[from] crate::TreeError),
    /// Content comparison failed.
    #[error("merging tree content")]
    Content(#[from] super::Error),
    /// Traversal reached its depth or entry bound; no approximate result is returned.
    #[error("merge traversal limit: {0}")]
    Limit(&'static str),
    /// Caller cancelled computation.
    #[error("merge cancelled")]
    Cancelled,
    /// A caller-selected content driver failed; no fallback resolution is attempted.
    #[error("running path-selected merge driver")]
    Driver {
        /// Destination byte path passed to the callback.
        path: Vec<u8>,
        /// Original caller-owned failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Directory conflicts need explicit path expansion before index representation.
    #[error("directory conflicts cannot be represented as leaf index stages")]
    DirectoryStages,
    /// In a partial clone, blobs the merge would compare are not stored. Every missing blob is
    /// listed before any content is merged; fetch them, for example with
    /// [`workflow::fetch_needed`](crate::workflow::fetch_needed), and merge again.
    #[error("merge needs objects the repository does not store")]
    MissingObjects(#[source] crate::MissingObjectsError),
}

impl Objects {
    /// Merges three tree snapshots by path with default bounds and no cancellation.
    ///
    /// This primitive does not infer renames. See [`Self::merge_tree_entries_with`] for the
    /// computation/publication boundary and [`TreeMerge`] for generated object ownership.
    ///
    /// # Errors
    ///
    /// Returns input object, content comparison, tree validation or traversal errors.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository, Tree};
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("repo"),
    ///     InitKind::Bare,
    /// )?;
    /// let empty = Tree::new(repo.object_format(), vec![])?;
    /// let id = repo.loose_objects().write_tree(&empty)?;
    /// let result = repo.objects()?.merge_tree_entries(id, id, id)?;
    /// assert!(result.is_clean());
    /// assert_eq!(result.tree.id(), id);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn merge_tree_entries(
        &self,
        base: ObjectId,
        ours: ObjectId,
        theirs: ObjectId,
    ) -> Result<TreeMerge, TreeError> {
        self.merge_tree_entries_with(
            base,
            ours,
            theirs,
            TreeOptions::default(),
            &AtomicBool::new(false),
        )
    }

    /// Merges entries at matching paths without rename inference or working-tree access.
    ///
    /// Exact entry equality and unchanged-side selection precede content comparison. Directories
    /// merge recursively; regular-file modes and payloads merge independently. Different symlink
    /// targets, entry types and gitlinks remain structured conflicts. Add/add regular files use
    /// an empty content base. Modify/delete preserves the surviving entry provisionally. No
    /// submodule repository is opened. Paths and file payloads remain bytes.
    ///
    /// Generated blobs/trees stay in memory; no object store, index, working file or reference is
    /// mutated. Read errors discard the entire result. Callers own input-store retention during
    /// computation and installation. Rename-aware callers must reconcile renamed paths separately.
    ///
    /// # Errors
    ///
    /// Returns [`TreeError`] on unreadable/wrong-kind inputs, content limits, cancellation,
    /// traversal bounds or invalid generated trees. Reused input subtrees are not validated.
    pub fn merge_tree_entries_with(
        &self,
        base: ObjectId,
        ours: ObjectId,
        theirs: ObjectId,
        options: TreeOptions,
        cancel: &AtomicBool,
    ) -> Result<TreeMerge, TreeError> {
        merge_prepared(
            self,
            [base, ours, theirs],
            options,
            cancel,
            &super::memory::Memory::default(),
        )
    }
}

pub(super) fn merge_prepared(
    objects: &Objects,
    roots: [ObjectId; 3],
    options: TreeOptions,
    cancel: &AtomicBool,
    memory: &super::memory::Memory,
) -> Result<TreeMerge, TreeError> {
    merge_prepared_with_driver(objects, roots, options, cancel, memory, None)
}

pub(super) fn merge_prepared_with_driver<'a, 'd>(
    objects: &'a Objects,
    roots: [ObjectId; 3],
    options: TreeOptions,
    cancel: &'a AtomicBool,
    memory: &'a super::memory::Memory,
    driver: Option<&'a mut (dyn super::TreeDriver + 'd)>,
) -> Result<TreeMerge, TreeError> {
    let missing = missing_blobs(objects, roots, options, cancel, memory)?;
    if !missing.is_empty() {
        return Err(TreeError::MissingObjects(crate::MissingObjectsError::new(
            missing,
        )));
    }
    let mut merger = Merger::new(objects, options, cancel, memory, driver);
    let [base, ours, theirs] = roots;
    let tree = merger.directory(Some(base), Some(ours), Some(theirs), &[], 0)?;
    merger.check()?;
    Ok(TreeMerge {
        tree,
        conflicts: merger.conflicts,
        blobs: merger.blobs,
        trees: merger.trees,
    })
}

/// In a partial clone, lists every blob a path merge of `roots` would compare that is not
/// stored, in traversal order. Other repositories skip this pass and return an empty list: a
/// missing blob there is damage, reported where it is read.
///
/// The survey walks the same paths as the merge, so it lists exactly the base, ours and theirs
/// blobs of paths changed on both sides, without reading any blob.
pub(super) fn missing_blobs(
    objects: &Objects,
    roots: [ObjectId; 3],
    options: TreeOptions,
    cancel: &AtomicBool,
    memory: &super::memory::Memory,
) -> Result<Vec<(ObjectId, ObjectKind)>, TreeError> {
    if !objects.partial_clone {
        return Ok(Vec::new());
    }
    let mut survey = Merger::new(objects, options, cancel, memory, None);
    survey.survey = Some(Survey::default());
    let [base, ours, theirs] = roots;
    survey.directory(Some(base), Some(ours), Some(theirs), &[], 0)?;
    Ok(survey
        .survey
        .map(|survey| survey.missing)
        .unwrap_or_default())
}

/// Missing-blob collection in place of content merging.
#[derive(Default)]
struct Survey {
    seen: IdSet,
    missing: Vec<(ObjectId, ObjectKind)>,
}

struct Merger<'a, 'd> {
    objects: &'a Objects,
    memory: &'a super::memory::Memory,
    driver: Option<&'a mut (dyn super::TreeDriver + 'd)>,
    options: TreeOptions,
    cancel: &'a AtomicBool,
    remaining: usize,
    conflicts: Vec<TreeConflict>,
    blobs: Vec<(ObjectId, Vec<u8>)>,
    trees: Vec<Tree>,
    /// When set, records missing blobs instead of merging content.
    survey: Option<Survey>,
}

impl<'a, 'd> Merger<'a, 'd> {
    fn new(
        objects: &'a Objects,
        options: TreeOptions,
        cancel: &'a AtomicBool,
        memory: &'a super::memory::Memory,
        driver: Option<&'a mut (dyn super::TreeDriver + 'd)>,
    ) -> Self {
        Self {
            objects,
            options,
            cancel,
            memory,
            driver,
            remaining: options.max_entries,
            conflicts: Vec::new(),
            blobs: Vec::new(),
            trees: Vec::new(),
            survey: None,
        }
    }

    /// Records the blobs of `values` that are neither generated nor stored.
    fn note_missing(&mut self, values: [Option<TreeValue>; 3]) -> Result<(), TreeError> {
        let Some(survey) = self.survey.as_mut() else {
            return Ok(());
        };
        for value in values.into_iter().flatten() {
            if self.memory.blobs.contains_key(&value.id) || !survey.seen.insert(value.id) {
                continue;
            }
            let stored = self
                .objects
                .contains(value.id)
                .map_err(crate::ReadBlobError::from)?;
            if !stored {
                survey.missing.push((value.id, ObjectKind::Blob));
            }
        }
        Ok(())
    }
    fn check(&self) -> Result<(), TreeError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(TreeError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn read(&mut self, id: Option<ObjectId>) -> Result<BTreeMap<Vec<u8>, TreeValue>, TreeError> {
        self.check()?;
        let Some(id) = id else {
            return Ok(BTreeMap::new());
        };
        let tree = self.memory.read_tree(self.objects, id)?;
        self.remaining = self
            .remaining
            .checked_sub(tree.entries().len())
            .ok_or(TreeError::Limit("entries"))?;
        Ok(tree
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
            .collect())
    }

    fn directory(
        &mut self,
        base: Option<ObjectId>,
        ours: Option<ObjectId>,
        theirs: Option<ObjectId>,
        path: &[u8],
        depth: usize,
    ) -> Result<Tree, TreeError> {
        self.check()?;
        if depth > self.options.max_depth {
            return Err(TreeError::Limit("depth"));
        }
        let b = self.read(base)?;
        let o = self.read(ours)?;
        let t = self.read(theirs)?;
        let names: BTreeSet<_> = b.keys().chain(o.keys()).chain(t.keys()).collect();
        let mut entries = Vec::new();
        for name in names {
            self.check()?;
            let mut child = path.to_vec();
            if !child.is_empty() {
                child.push(b'/');
            }
            child.extend_from_slice(name);
            if let Some(value) = self.entry(
                b.get(name).copied(),
                o.get(name).copied(),
                t.get(name).copied(),
                &child,
                depth,
            )? {
                entries.push(TreeEntry {
                    name: name.clone(),
                    mode: value.mode,
                    id: value.id,
                });
            }
        }
        let tree = Tree::new(self.objects.object_format(), entries)?;
        self.trees.push(tree.clone());
        Ok(tree)
    }

    fn entry(
        &mut self,
        base: Option<TreeValue>,
        ours: Option<TreeValue>,
        theirs: Option<TreeValue>,
        path: &[u8],
        depth: usize,
    ) -> Result<Option<TreeValue>, TreeError> {
        if ours == theirs || base == theirs {
            return Ok(ours);
        }
        if base == ours {
            return Ok(theirs);
        }
        if directories(base, ours, theirs) {
            let tree = self.directory(
                base.map(|v| v.id),
                ours.map(|v| v.id),
                theirs.map(|v| v.id),
                path,
                depth + 1,
            )?;
            return Ok(Some(TreeValue {
                mode: EntryMode::Tree,
                id: tree.id(),
            }));
        }
        let mut conflict = TreeConflict {
            path: path.to_vec(),
            kind: ConflictKind::Type,
            base,
            ours,
            theirs,
            content: None,
            rename_source: None,
            stage_paths: None,
        };
        if let (Some(o), Some(t)) = (ours, theirs) {
            if regular(o.mode) && regular(t.mode) && base.is_none_or(|b| regular(b.mode)) {
                return self.file(conflict, o, t);
            }
            conflict.kind = if o.mode == EntryMode::Gitlink && t.mode == EntryMode::Gitlink {
                ConflictKind::Gitlink
            } else if o.mode == EntryMode::Symlink && t.mode == EntryMode::Symlink {
                ConflictKind::Symlink
            } else {
                ConflictKind::Type
            };
        } else {
            conflict.kind = ConflictKind::ModifyDelete;
        }
        self.conflicts.push(conflict);
        Ok(ours.or(theirs))
    }

    fn file(
        &mut self,
        mut conflict: TreeConflict,
        ours: TreeValue,
        theirs: TreeValue,
    ) -> Result<Option<TreeValue>, TreeError> {
        let (mode, mode_conflict) =
            if ours.mode == theirs.mode || conflict.base.is_some_and(|b| b.mode == theirs.mode) {
                (ours.mode, false)
            } else if conflict.base.is_some_and(|b| b.mode == ours.mode) {
                (theirs.mode, false)
            } else {
                (ours.mode, true)
            };
        if self.survey.is_some() {
            self.note_missing([conflict.base, Some(ours), Some(theirs)])?;
            return Ok(Some(ours));
        }
        let base = match conflict.base {
            Some(b) => self.memory.read_blob(self.objects, b.id)?,
            None => Vec::new(),
        };
        let a = self.memory.read_blob(self.objects, ours.id)?;
        let b = self.memory.read_blob(self.objects, theirs.id)?;
        let content = if let Some(driver) = self.driver.as_deref_mut() {
            driver
                .merge(
                    &conflict.path,
                    &base,
                    &a,
                    &b,
                    self.options.content,
                    self.cancel,
                )
                .map_err(|source| TreeError::Driver {
                    path: conflict.path.clone(),
                    source,
                })?
        } else {
            content_with(&base, &a, &b, self.options.content, self.cancel)?
        };
        self.check()?;
        if content.is_clean() && !mode_conflict {
            let bytes = content.render(&MarkerOptions::default());
            let id = ObjectId::for_blob(self.objects.object_format(), &bytes);
            self.blobs.push((id, bytes));
            return Ok(Some(TreeValue { mode, id }));
        }
        conflict.kind = if mode_conflict {
            ConflictKind::Mode
        } else if conflict.base.is_none() {
            ConflictKind::AddAdd
        } else {
            ConflictKind::Content
        };
        conflict.content = Some(content);
        self.conflicts.push(conflict);
        Ok(Some(TreeValue { mode, id: ours.id }))
    }
}

fn regular(mode: EntryMode) -> bool {
    matches!(mode, EntryMode::Blob | EntryMode::Executable)
}

fn directories(
    base: Option<TreeValue>,
    ours: Option<TreeValue>,
    theirs: Option<TreeValue>,
) -> bool {
    ours.is_some_and(|v| v.mode == EntryMode::Tree)
        && theirs.is_some_and(|v| v.mode == EntryMode::Tree)
        && base.is_none_or(|v| v.mode == EntryMode::Tree)
}
