//! Three-way merging of byte content without a repository or working tree.
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
mod content;
mod render;
mod types;

pub use content::{content, content_with};
pub use types::{
    ConflictStyle, ContentHunk, ContentMerge, ContentOptions, Error, Favor, MarkerOptions,
};
