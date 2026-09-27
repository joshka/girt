//! Inferred file renames and copies between Git trees.
//!
//! Call [`Objects::detect_rewrites`](crate::Objects::detect_rewrites) with [`Options`] to select
//! candidate comparison policy and [`Limits`] to bound work. The returned [`Rewrite`] values carry
//! inferred kind and score; [`Copies`] controls copy inference. A match describes stored content,
//! not recorded history or a working-tree operation.
//!
//! Inference is a heuristic, not recorded history: callers retain merge and copy-history policy.
//! Paths and payloads remain bytes. Attributes, worktree conversion and external diff/filter
//! commands are not consulted.
mod detect;
mod similarity;
mod types;

pub use types::{Copies, Error, Kind, Limits, Options, Rewrite};
