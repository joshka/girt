//! Git ignore rules over repository-relative byte paths.
//!
//! [`Ignore`] combines caller-loaded sources in Git precedence order. It checks every ancestor
//! before the requested path: an excluded directory prevents child rules from re-including its
//! contents. Callers still own traversal, symlink classification and tracked-file selection.
//! No filesystem, index, environment or configuration is read here. Resolve `core.excludesFile`
//! (including home expansion/defaults) and load its bytes explicitly; do the same for the common
//! directory's `info/exclude` and encountered `.gitignore` files. Do not follow worktree
//! `.gitignore` symlinks. Missing optional sources can be omitted; propagate other loading errors.
//! Retained sources describe the supplied observations, not an atomic filesystem snapshot.
//!
//! ```
//! use girt::ignore::{Case, Ignore, Limits, Source};
//! let mut ignore = Ignore::new(Case::Sensitive, Limits::default());
//! ignore.add(Source::Global, b"*.log\n")?;
//! ignore.add(Source::Directory(b""), b"!keep.log\nbuild/\n!build/keep\n")?;
//! assert!(!ignore.check(b"keep.log", false)?.unwrap().ignored);
//! assert!(ignore.check(b"build/keep", false)?.unwrap().ignored);
//! # Ok::<(), girt::ignore::Error>(())
//! ```

mod pattern;
mod rules;

pub use rules::{Case, Error, Ignore, Limits, Match, Source};

#[cfg(test)]
mod tests;
