//! Line origins across commit history, including inferred file renames.
//!
//! [`Objects::blame`](crate::Objects::blame) returns ordered [`Hunk`] values covering the requested
//! file. Lines pass to an identical parent file first, otherwise through unchanged spans to the
//! first matching parent in stored parent order; unmatched lines belong to the current commit.
//! Renames use the default rewrite similarity policy. Repeated lines can have different
//! shortest-edit alignment from Git ([issue 440](https://github.com/joshka/girt/issues/440)). Copy/move detection, ignore-revs,
//! textconv and incremental output are unsupported; attributes do not otherwise change blame, and
//! [`Repo::blame`](crate::Repo::blame) refuses a textconv driver. A NUL-containing requested file
//! is refused; older versions remain raw line inputs. Reached shallow boundaries are refused for
//! attributed lines. Empty files return no ranges.
//!
//! ```no_run
//! let repo = girt::Repository::open("repo")?;
//! let git = girt::Repo::new(repo);
//! for hunk in git.blame("HEAD", b"src/lib.rs")? {
//!     println!(
//!         "{}:{} ({})",
//!         hunk.commit, hunk.original_start, hunk.line_count
//!     );
//! }
//! # Ok::<(), girt::Error>(())
//! ```
mod types;
mod walk;

pub use types::{Error, Hunk, Limits};
pub(crate) use walk::valid_path;
