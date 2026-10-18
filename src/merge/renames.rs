use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

use super::memory::Memory;
use super::{ConflictKind, TreeConflict, TreeError, TreeMerge, TreeOptions};
use crate::{EntryMode, ObjectId, Objects, TreeValue};

type Leaves = BTreeMap<Vec<u8>, TreeValue>;
type Renames = BTreeMap<Vec<u8>, Vec<u8>>;

/// Rename inference and traversal policy for [`Objects::merge_trees`].
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct TreeMergeOptions {
    /// Path-based merge and content bounds.
    pub trees: TreeOptions,
    /// Rename similarity and candidate policy; copy selection is always disabled.
    pub renames: crate::rewrites::Options,
    /// Bounds applied separately to each base-to-side rename comparison.
    pub rename_limits: crate::rewrites::Limits,
}

/// Rename-aware computation failed without installing any generated objects.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenameError {
    /// Traversal, content comparison or generated tree construction failed.
    #[error("merging renamed tree entries")]
    Tree(#[from] TreeError),
    /// Existing rewrite inference could not complete.
    #[error("inferring merge renames")]
    Inference(#[from] crate::tree_diff::Error),
    /// Candidate limits skipped inexact inference; a deletion/addition approximation is refused.
    #[error("merge rename candidate search was incomplete")]
    Incomplete,
    /// Path alignment would overwrite another entry; this layout requires explicit resolution.
    #[error("rename destination collides with another path")]
    DestinationCollision(Vec<u8>),
    /// One removed directory has several inferred destinations.
    #[error("directory rename has several destinations")]
    DirectoryAmbiguous(Vec<u8>),
    /// Non-root empty trees cannot be preserved by this leaf-based normalization.
    #[error("rename normalization does not support stored empty subdirectories")]
    EmptyDirectory(Vec<u8>),
}

impl Objects {
    /// Merges three trees with rename inference and default bounds, without writing objects.
    ///
    /// See [`Self::merge_trees_with`] for conflict and unsupported-layout contracts.
    ///
    /// # Errors
    ///
    /// Returns [`RenameError`] on incomplete inference, unsupported alignment or tree failures.
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository, Tree};
    /// let temporary = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     temporary.path().join("repo"),
    ///     InitKind::Bare,
    /// )?;
    /// let empty = repo
    ///     .loose_objects()
    ///     .write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// let result = repo.objects()?.merge_trees(empty, empty, empty)?;
    /// assert!(result.is_clean());
    /// assert_eq!(result.tree.id(), empty);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn merge_trees(
        &self,
        base: ObjectId,
        ours: ObjectId,
        theirs: ObjectId,
    ) -> Result<TreeMerge, RenameError> {
        self.merge_trees_with(
            base,
            ours,
            theirs,
            TreeMergeOptions::default(),
            &AtomicBool::new(false),
        )
    }

    /// Aligns renamed paths, then merges content and modes entirely in memory.
    ///
    /// Base-to-side inference uses the existing rewrite detector. One-sided renames carry an
    /// opposite-side modification to the destination. Distinct rename destinations and
    /// rename/delete remain structured conflicts with original entries. Removed directories with
    /// one inferred destination relocate opposite-side additions and report directory-rename
    /// conflicts, matching Git's default conservative directory-addition policy.
    ///
    /// Destination collisions, ambiguous directory moves, skipped inexact inference and stored
    /// empty subdirectories are explicit errors rather than deletion/addition approximations.
    /// Input normalization trees and clean merged blobs stay in [`TreeMerge`]; install generated
    /// objects before publishing its root. No attributes, configuration or submodules are opened.
    /// Each traversal/comparison has its own bounds. Cancellation discards the entire result.
    ///
    /// # Errors
    ///
    /// Returns [`RenameError`] for inference, layout, object-read, content or traversal failures.
    pub fn merge_trees_with(
        &self,
        base: ObjectId,
        ours: ObjectId,
        theirs: ObjectId,
        options: TreeMergeOptions,
        cancel: &AtomicBool,
    ) -> Result<TreeMerge, RenameError> {
        merge_with_memory(
            self,
            [base, ours, theirs],
            options,
            cancel,
            &mut Memory::default(),
        )
    }
}

pub(super) fn merge_with_memory(
    objects: &Objects,
    roots: [ObjectId; 3],
    options: TreeMergeOptions,
    cancel: &AtomicBool,
    memory: &mut Memory,
) -> Result<TreeMerge, RenameError> {
    let [base, ours, theirs] = roots;
    let left = infer(objects, base, ours, options, cancel, memory)?;
    let right = infer(objects, base, theirs, options, cancel, memory)?;
    let mut reader = FlatReader::new(objects, options, cancel, memory);

    let mut b = reader.read(base, &[], 0)?;
    let mut o = reader.read(ours, &[], 0)?;
    let mut t = reader.read(theirs, &[], 0)?;
    let original = b.clone();
    let left_dirs = directory_moves(&left, &o)?;
    let right_dirs = directory_moves(&right, &t)?;
    let mut conflicts = Vec::new();
    let sources: BTreeSet<_> = left.keys().chain(right.keys()).cloned().collect();
    for source in sources {
        let base_value = b.get(&source).copied();
        match (left.get(&source), right.get(&source)) {
            (Some(a), Some(c)) if a != c => {
                conflicts.push(TreeConflict {
                    path: source.clone(),
                    kind: ConflictKind::RenameRename,
                    base: base_value,
                    ours: o.get(a).copied(),
                    theirs: t.get(c).copied(),
                    content: None,
                    rename_source: Some(source.clone()),
                    stage_paths: Some([source.clone(), a.clone(), c.clone()]),
                });
                b.remove(&source);
            }
            (Some(target), Some(_)) => {
                if b.contains_key(target) {
                    return Err(RenameError::DestinationCollision(target.clone()));
                }
                if let Some(value) = b.remove(&source) {
                    b.insert(target.clone(), value);
                }
            }
            (Some(target), _) => align(
                &source,
                target,
                &mut b,
                &mut o,
                &mut t,
                true,
                &mut conflicts,
            )?,
            (_, Some(target)) => align(
                &source,
                target,
                &mut b,
                &mut t,
                &mut o,
                false,
                &mut conflicts,
            )?,
            _ => {}
        }
    }
    relocate_additions(&original, &left_dirs, &mut t, false, &mut conflicts)?;
    relocate_additions(&original, &right_dirs, &mut o, true, &mut conflicts)?;
    let roots = [
        memory.build(objects, b)?,
        memory.build(objects, o)?,
        memory.build(objects, t)?,
    ];
    let mut result = super::tree::merge_prepared(objects, roots, options.trees, cancel, memory)?;
    for conflict in &mut result.conflicts {
        if let Some((source, _)) = left
            .iter()
            .chain(right.iter())
            .find(|(_, target)| **target == conflict.path)
        {
            conflict.rename_source = Some(source.clone());
        }
    }
    result.conflicts.extend(conflicts);
    result.conflicts.sort_by(|a, b| a.path.cmp(&b.path));
    let mut trees: Vec<_> = memory
        .order
        .iter()
        .map(|id| memory.trees[id].clone())
        .collect();
    trees.append(&mut result.trees);
    result.trees = trees;
    Ok(result)
}

fn infer(
    objects: &Objects,
    base: ObjectId,
    side: ObjectId,
    options: TreeMergeOptions,
    cancel: &AtomicBool,
    memory: &Memory,
) -> Result<Renames, RenameError> {
    let policy = options
        .renames
        .with_copies(crate::rewrites::Copies::Disabled);
    if memory.trees.contains_key(&base) || memory.trees.contains_key(&side) {
        let mut reader = FlatReader::new(objects, options, cancel, memory);
        let old = reader.read(base, &[], 0)?;
        let new = reader.read(side, &[], 0)?;
        let names: BTreeSet<_> = old.keys().chain(new.keys()).collect();
        let mut changes = Vec::new();
        for path in names {
            let a = old.get(path).copied();
            let b = new.get(path).copied();
            if a != b {
                if changes.len() == options.rename_limits.trees.max_changes {
                    return Err(TreeError::Limit("changes").into());
                }
                changes.push(crate::TreeChange {
                    path: path.clone(),
                    old: a,
                    new: b,
                });
            }
        }
        let detection = objects
            .detect_rewrites_with_memory(
                &changes,
                policy,
                options.rename_limits,
                None,
                cancel,
                Some(&memory.blobs),
            )
            .map_err(crate::tree_diff::Error::from)?;
        if detection.inexact_skipped.is_some() {
            return Err(RenameError::Incomplete);
        }
        return Ok(detection
            .rewrites
            .into_iter()
            .filter(|r| r.kind == crate::rewrites::Kind::Rename)
            .map(|r| (r.source, r.target))
            .collect());
    }
    let comparison = objects.diff_trees_with(
        Some(base),
        Some(side),
        policy,
        options.rename_limits,
        cancel,
    )?;
    if comparison.inexact_skipped.is_some() {
        return Err(RenameError::Incomplete);
    }
    Ok(comparison
        .entries
        .into_iter()
        .filter_map(|entry| match entry {
            crate::tree_diff::Entry::Rewrite {
                kind: crate::rewrites::Kind::Rename,
                source,
                target,
                ..
            } => Some((source, target)),
            _ => None,
        })
        .collect())
}

fn align(
    source: &[u8],
    target: &[u8],
    base: &mut Leaves,
    renamed: &mut Leaves,
    opposite: &mut Leaves,
    ours_renamed: bool,
    conflicts: &mut Vec<TreeConflict>,
) -> Result<(), RenameError> {
    if base.contains_key(target) {
        return Err(RenameError::DestinationCollision(target.to_vec()));
    }
    let original = base.remove(source);
    if opposite.contains_key(target) {
        return Err(RenameError::DestinationCollision(target.to_vec()));
    }
    if let Some(value) = opposite.remove(source) {
        opposite.insert(target.to_vec(), value);
    } else {
        let value = renamed.get(target).copied();
        conflicts.push(TreeConflict {
            path: target.to_vec(),
            kind: ConflictKind::RenameDelete,
            base: original,
            ours: if ours_renamed { value } else { None },
            theirs: if ours_renamed { None } else { value },
            content: None,
            rename_source: Some(source.to_vec()),
            stage_paths: None,
        });
        // Removing the base makes the candidate keep the renamed entry as an addition.
        return Ok(());
    }
    if let Some(value) = original {
        base.insert(target.to_vec(), value);
    }
    Ok(())
}

fn parent(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|b| *b == b'/')
        .map_or(&[], |slash| &path[..slash])
}

fn below(path: &[u8], directory: &[u8]) -> bool {
    path.starts_with(directory) && path.get(directory.len()) == Some(&b'/')
}

fn directory_moves(renames: &Renames, side: &Leaves) -> Result<Renames, RenameError> {
    let mut candidates: BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>> = BTreeMap::new();
    for (source, target) in renames {
        let from = parent(source);
        let to = parent(target);
        if from.is_empty() || from == to || side.keys().any(|path| below(path, from)) {
            continue;
        }
        candidates
            .entry(from.to_vec())
            .or_default()
            .insert(to.to_vec());
    }
    let mut moves = Renames::new();
    for (from, targets) in candidates {
        if targets.len() != 1 {
            return Err(RenameError::DirectoryAmbiguous(from));
        }
        moves.insert(from, targets.into_iter().next().unwrap_or_default());
    }
    Ok(moves)
}

fn relocate_additions(
    base: &Leaves,
    moves: &Renames,
    side: &mut Leaves,
    ours: bool,
    conflicts: &mut Vec<TreeConflict>,
) -> Result<(), RenameError> {
    let additions: Vec<_> = side
        .keys()
        .filter(|path| !base.contains_key(*path))
        .cloned()
        .collect();
    for source in additions {
        let Some((from, to)) = moves
            .iter()
            .filter(|(from, _)| below(&source, from))
            .max_by_key(|(from, _)| from.len())
        else {
            continue;
        };
        let mut target = to.clone();
        let suffix = &source[from.len() + 1..];
        if !target.is_empty() {
            target.push(b'/');
        }
        target.extend_from_slice(suffix);
        if side.contains_key(&target) {
            return Err(RenameError::DestinationCollision(target));
        }
        let value = side.remove(&source);
        if let Some(value) = value {
            side.insert(target.clone(), value);
        }
        conflicts.push(TreeConflict {
            path: target,
            kind: ConflictKind::DirectoryRename,
            base: None,
            ours: if ours { value } else { None },
            theirs: if ours { None } else { value },
            content: None,
            rename_source: Some(source),
            stage_paths: None,
        });
    }
    Ok(())
}

pub(super) fn flatten(
    objects: &Objects,
    root: ObjectId,
    options: TreeMergeOptions,
    cancel: &AtomicBool,
    memory: &Memory,
) -> Result<Leaves, RenameError> {
    FlatReader::new(objects, options, cancel, memory).read(root, &[], 0)
}

struct FlatReader<'a> {
    objects: &'a Objects,
    memory: &'a Memory,
    options: TreeOptions,
    traversal: crate::TreeCompareLimits,
    reads: usize,
    bytes: usize,
    paths: usize,
    cancel: &'a AtomicBool,
    remaining: usize,
}

impl<'a> FlatReader<'a> {
    fn new(
        objects: &'a Objects,
        options: TreeMergeOptions,
        cancel: &'a AtomicBool,
        memory: &'a Memory,
    ) -> Self {
        let traversal = options.rename_limits.trees;
        Self {
            objects,
            memory,
            options: options.trees,
            traversal,
            cancel,
            remaining: options.trees.max_entries.min(traversal.max_entries),
            reads: traversal.max_trees,
            bytes: traversal.max_tree_bytes,
            paths: traversal.max_path_bytes,
        }
    }

    fn read(&mut self, id: ObjectId, path: &[u8], depth: usize) -> Result<Leaves, RenameError> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(TreeError::Cancelled.into());
        }
        if depth > self.options.max_depth.min(self.traversal.max_depth) {
            return Err(TreeError::Limit("depth").into());
        }
        self.reads = self
            .reads
            .checked_sub(1)
            .ok_or(TreeError::Limit("tree reads"))?;
        let tree = self
            .memory
            .read_tree_with(self.objects, id, self.traversal.read)?;
        self.bytes = self
            .bytes
            .checked_sub(tree.encode().len())
            .ok_or(TreeError::Limit("tree bytes"))?;
        self.remaining = self
            .remaining
            .checked_sub(tree.entries().len())
            .ok_or(TreeError::Limit("entries"))?;
        if tree.entries().is_empty() && !path.is_empty() {
            return Err(RenameError::EmptyDirectory(path.to_vec()));
        }
        let mut leaves = Leaves::new();
        for entry in tree.entries() {
            let mut child = path.to_vec();
            if !child.is_empty() {
                child.push(b'/');
            }
            let length = child
                .len()
                .checked_add(entry.name.len())
                .ok_or(TreeError::Limit("path bytes"))?;
            self.paths = self
                .paths
                .checked_sub(length)
                .ok_or(TreeError::Limit("path bytes"))?;
            child.extend_from_slice(&entry.name);
            if entry.mode == EntryMode::Tree {
                leaves.extend(self.read(entry.id, &child, depth + 1)?);
            } else {
                leaves.insert(
                    child,
                    TreeValue {
                        mode: entry.mode,
                        id: entry.id,
                    },
                );
            }
        }
        Ok(leaves)
    }
}
