//! Tree changes combined with inferred renames and copies.
//! This composes [`Objects::compare_trees`] and [`Objects::detect_rewrites_or_exact`], retaining
//! unconsumed changes and sorting by destination path. Rename inference uses stored bytes and no
//! attributes.
//!
//! ```
//! use girt::tree::TreeEditor;
//! use girt::tree_diff::Entry;
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Bare,
//! )?;
//!
//! let loose = repo.loose_objects();
//! let blob = loose.write_blob(b"retained content\n")?;
//! let mut editor = TreeEditor::new(&loose);
//! editor.set(b"old", blob)?;
//! let old = editor.write()?;
//!
//! editor.remove(b"old")?;
//! editor.set(b"new", blob)?;
//! let new = editor.write()?;
//!
//! let objects = repo.objects()?;
//! let changes = objects.diff_trees(Some(old), Some(new))?;
//!
//! assert!(matches!(
//!     &changes.entries[0],
//!     Entry::Rewrite {
//!         similarity: 100,
//!         ..
//!     }
//! ));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::AtomicBool;

use crate::pathspec::Pathspec;
pub use crate::rewrites::Limits;
use crate::rewrites::{Kind, Options};
use crate::{ObjectId, Objects, TreeChange, TreeValue};

/// One line of the comparison: a plain change or an inferred rename/copy.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Entry {
    /// An unconsumed structural change.
    Change(TreeChange),
    /// An inferred rename or copy, carrying both original tree values.
    Rewrite {
        /// Rename or copy.
        kind: Kind,
        /// Similarity percentage, from 0 through 100.
        similarity: u8,
        /// Original byte path.
        source: Vec<u8>,
        /// Destination byte path.
        target: Vec<u8>,
        /// Original entry.
        old: TreeValue,
        /// Destination entry.
        new: TreeValue,
    },
}

impl Entry {
    /// Returns the content sides as a structural change for [`crate::content_diff::BlobContent`].
    /// For rewrites the path is the destination; the source remains available on this entry.
    pub fn content_change(&self) -> TreeChange {
        match self {
            Self::Change(change) => change.clone(),
            Self::Rewrite {
                target, old, new, ..
            } => TreeChange {
                path: target.clone(),
                old: Some(*old),
                new: Some(*new),
            },
        }
    }

    /// The path Git sorts output by: the destination for rewrites.
    pub fn sort_path(&self) -> &[u8] {
        match self {
            Entry::Change(change) => &change.path,
            Entry::Rewrite { target, .. } => target,
        }
    }
}

/// Combined changes ordered by destination bytes, with an explicit rename-limit diagnostic.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Comparison {
    /// Unconsumed changes and inferred rewrites. Exact matches survive candidate-limit skips.
    pub entries: Vec<Entry>,
    /// Exhaustive edited-content inference was skipped; unmatched paths remain plain changes.
    pub inexact_skipped: Option<crate::rewrites::InexactSkipped>,
}

/// A structural comparison or rewrite inference failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Structural tree comparison failed.
    #[error(transparent)]
    Trees(#[from] crate::TreeCompareError),
    /// Rename/copy inference failed.
    #[error(transparent)]
    Rewrites(#[from] crate::rewrites::Error),
    /// Inference returned a path absent from the structural comparison.
    #[error("rewrite paths missing from tree comparison")]
    MissingPath,
}
impl Objects {
    /// Compares trees and infers renames using default options and limits, without cancellation.
    ///
    /// Wraps [`Self::diff_trees_with`] with default options and limits.
    ///
    /// # Errors
    ///
    /// Returns storage, malformed-tree, resource or inference errors from the two explicit APIs.
    pub fn diff_trees(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
    ) -> Result<Comparison, Error> {
        self.diff_trees_with(
            old,
            new,
            Options::default(),
            Limits::default(),
            &AtomicBool::new(false),
        )
    }
    /// Compares trees and infers rewrites under explicit policy, limits and cancellation.
    /// Copies retain their source change; renames consume deleted sources and added targets.
    /// The existing rewrite limits supply tree bounds for one structural traversal, reused during
    /// inference. Candidate-limit skips retain exact matches and are reported on the result;
    /// resource-budget exhaustion still returns an error. Memory grows with admitted tree changes
    /// and candidate content; path lookups use ordered maps and sets.
    ///
    /// # Errors
    ///
    /// Returns storage, malformed-tree, resource, cancellation or inference errors. Nothing is
    /// written.
    pub fn diff_trees_with(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        options: Options,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Comparison, Error> {
        self.diff_trees_matching(old, new, &Pathspec::all(), options, limits, cancel)
    }

    /// [`Self::diff_trees_with`], limited to paths `paths` selects.
    ///
    /// As in `git diff -M <old> <new> -- <pathspec>`, the pathspec applies before inference: a
    /// file moved from outside the pathspec to inside it is an addition, not a rename.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::diff_trees_with`], and an unsupported-pathspec tree
    /// comparison error for `attr:` magic.
    pub fn diff_trees_matching(
        &self,
        old: Option<ObjectId>,
        new: Option<ObjectId>,
        paths: &Pathspec,
        options: Options,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Comparison, Error> {
        let objects = self;
        let changes = objects.compare_trees_matching(old, new, paths, limits.trees, cancel)?;
        let detection =
            objects.detect_rewrites_from_changes(&changes, options, limits, None, cancel)?;
        let by_path: BTreeMap<_, _> = changes
            .iter()
            .map(|change| (change.path.as_slice(), change))
            .collect();
        let find = |path: &[u8]| by_path.get(path).copied();
        let mut renamed_sources = BTreeSet::new();
        let mut targets = BTreeSet::new();
        let mut entries = Vec::new();
        for rewrite in detection.rewrites {
            let (Some(old), Some(new)) = (
                find(&rewrite.source).and_then(|change| change.old),
                find(&rewrite.target).and_then(|change| change.new),
            ) else {
                return Err(Error::MissingPath);
            };
            if rewrite.kind == Kind::Rename {
                renamed_sources.insert(rewrite.source.clone());
            }
            targets.insert(rewrite.target.clone());
            entries.push(Entry::Rewrite {
                kind: rewrite.kind,
                similarity: rewrite.similarity,
                source: rewrite.source,
                target: rewrite.target,
                old,
                new,
            });
        }
        drop(by_path);
        for change in changes {
            let consumed = (change.new.is_none() && renamed_sources.contains(&change.path))
                || (change.old.is_none() && targets.contains(&change.path));
            if !consumed {
                entries.push(Entry::Change(change));
            }
        }
        entries.sort_by(|a, b| a.sort_path().cmp(b.sort_path()));
        Ok(Comparison {
            entries,
            inexact_skipped: detection.inexact_skipped,
        })
    }
}
