//! Three-way merging of byte content and tree entries without a working tree.
//!
//! [`content()`] compares each side with a common base and keeps nonoverlapping edits. Unresolved
//! regions retain all three inputs in [`ContentHunk::Conflict`]; marker text is a separate
//! presentation chosen by [`ContentMerge::render`]. Tree and sequencer consumers can retain those
//! sides instead of interpreting markers. Paths, attributes and configuration are caller policy.
//!
//! ```
//! use girt::merge::{MarkerOptions, content};
//! let merged = content(b"a\nb\nc\n", b"A\nb\nc\n", b"a\nb\nC\n")?;
//! assert!(merged.is_clean());
//! assert_eq!(merged.render(&MarkerOptions::default()), b"A\nb\nC\n");
//! # Ok::<(), girt::merge::Error>(())
//! ```
//!
//! Comparisons use bounded Myers line edits. Ambiguous repeated-line placement and internal
//! common lines in conflict regions can differ from Git's refinement. Adjacent edits from
//! opposite sides form one region. Binary conflicts retain whole inputs and render ours without
//! markers. No references, index entries, objects or working files are written.
//! [`crate::Objects::merge_tree_entries`] merges entries at matching paths and returns a
//! [`TreeMerge`] with a candidate root, generated objects and original conflict sides. Install
//! generated objects explicitly before publishing a clean root. This primitive does not infer
//! renames. [`crate::Objects::merge_trees`] composes rename inference and directory relocation
//! with the same in-memory result, explicitly refusing ambiguous normalization layouts.
//! [`Driver`] selects built-in or caller-owned custom drivers from an attribute state.
//! [`TreeDriver`] routes tree and commit content merges through explicit per-path policy,
//! including recursive virtual bases, without implicit attribute snapshots or process execution.
//! [`MarkerOptions::from_config`] applies an explicit conflict-style configuration snapshot.
//! [`crate::Objects::merge_commits`] discovers ancestors and recursively synthesizes clean
//! virtual bases, retaining all generated objects without publication. Conflicted virtual bases
//! return an explicit error with the intermediate result.
//! [`TreeMerge::prepare_checkout`] prepares marker working bytes and original conflict stages.
//! [`crate::Repository::checkout_merge`] publishes them through guarded checkout; computation
//! remains separate from mutable working-file and index publication.
//! [`Whitespace`] changes line matching while retaining original payload bytes and final-LF
//! distinctions. Its comparison-only keys never become stored blob content.
mod commits;
mod content;
mod driver;
mod memory;
mod path_driver;
mod publication;
mod renames;
mod render;
mod tree;
mod types;
mod whitespace;

pub use commits::{CommitError, CommitMerge, CommitOptions};
pub use content::{content, content_with};
pub use driver::{Driver, DriverError};
pub use path_driver::TreeDriver;
pub use publication::{MergeCheckout, PreparationLimits, PublicationError};
pub use renames::{RenameError, TreeMergeOptions};
pub use tree::{ConflictKind, TreeConflict, TreeError, TreeMerge, TreeOptions};
pub use types::{
    ConflictStyle, ContentHunk, ContentMerge, ContentOptions, Error, Favor, MarkerOptions,
};
pub use whitespace::Whitespace;

/// Object overlay retained across replay steps; never published by computation.
#[derive(Default)]
pub(crate) struct ReplayTrees(memory::Memory);

impl ReplayTrees {
    pub(crate) fn merge<'a, 'd>(
        &'a mut self,
        objects: &'a crate::Objects,
        roots: [crate::ObjectId; 3],
        empty: crate::Tree,
        options: TreeMergeOptions,
        cancel: &'a std::sync::atomic::AtomicBool,
        driver: Option<&'a mut (dyn TreeDriver + 'd)>,
    ) -> Result<TreeMerge, RenameError> {
        // Earlier steps own their generated objects; only this step's normalization trees need
        // to be repeated in its result. Lookup maps remain available for later comparisons.
        self.0.order.clear();
        self.0.trees.insert(empty.id(), empty);
        let result = renames::merge_with_memory_and_driver(
            objects,
            roots,
            options,
            cancel,
            &mut self.0,
            driver,
        )?;
        for tree in &result.trees {
            self.0.trees.insert(tree.id(), tree.clone());
        }
        self.0.blobs.extend(result.blobs.iter().cloned());
        Ok(result)
    }
}
