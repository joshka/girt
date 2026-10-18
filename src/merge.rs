//! Three-way merging of byte content and tree entries without a working tree.
//!
//! [`content()`] compares each side with a common base and keeps nonoverlapping edits. Unresolved
//! regions retain all three inputs in [`ContentHunk::Conflict`]; marker text is a separate
//! presentation chosen by [`ContentMerge::render`]. Tree and sequencer consumers can retain those
//! sides instead of interpreting markers. Paths, attributes and configuration are caller policy.
//!
//! ```
//! use girt::merge::{ContentOptions, MarkerOptions, content};
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
//! renames; rename-aware composition remains a separate operation.
//! [`Driver`] selects built-in or caller-owned custom drivers from an attribute state.
//! [`MarkerOptions::from_config`] applies an explicit conflict-style configuration snapshot.
mod content;
mod driver;
pub use driver::{Driver, DriverError};
mod render;
mod tree;
mod types;

pub use content::{content, content_with};
pub use tree::{ConflictKind, TreeConflict, TreeError, TreeMerge, TreeOptions};
pub use types::{
    ConflictStyle, ContentHunk, ContentMerge, ContentOptions, Error, Favor, MarkerOptions,
};
