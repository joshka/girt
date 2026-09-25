//! Inferred file renames and copies between Git trees.
//!
//! [`Objects::detect_rewrites`](crate::Objects::detect_rewrites) compares stored blob content.
//! Inference is a heuristic, not recorded history: callers retain merge and copy-history policy.
//! Paths and payloads remain bytes. Attributes, worktree conversion and external diff/filter
//! commands are not consulted. See [`Options`] for candidate selection and [`Rewrite`] for scores.
mod detect;
mod similarity;
mod types;

pub use types::{Copies, Error, Kind, Limits, Options, Rewrite};
