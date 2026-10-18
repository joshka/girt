//! Search file contents as `git grep` does.
//!
//! A search has three parts:
//!
//! - A [`Query`] compiles the patterns of an [`Expression`] with [`Matching`] rules and decides
//!   which lines match.
//! - A [`Searcher`] applies [`Options`] to one file at a time: inverted selection, context, the
//!   function lines of `-p` and `-W`, `-m`, and binary handling, using a repository's diff
//!   attributes when given them.
//! - [`Repository::grep`](crate::Repository::grep) reads the files a [`Source`] and [`Scope`]
//!   select (the working tree, the index or a tree), and [`grep_directory`] reads a plain directory
//!   as `--no-index` does. [`GitFormat`] prints results as `git grep` does.
//!
//! # Differences from Git
//!
//! - Patterns follow the rules of [`Query`]: back-references, collating elements and equivalence
//!   classes are refused, and Perl-compatible patterns (`-P`) are not offered.
//! - girt never runs commands, so `diff.<driver>.textconv` (`--textconv`) is not applied.
//! - Submodules are not searched (`--recurse-submodules`).
//!
//! ```
//! use girt::grep::{Expression, GitFormat, Matching, Options, Query, Searcher, Syntax};
//!
//! // git grep -n -E -e 'fn [a-z]+' --and --not -e test
//! let expression = Expression::pattern("fn [a-z]+").and(!Expression::pattern("test"));
//! let query = Query::new(&expression, Matching::new().with_syntax(Syntax::Extended))?;
//! let mut searcher = Searcher::new(query, Options::new());
//!
//! let content = b"fn main() {}\nfn test_main() {}\n";
//! let matches = searcher.search(b"src/main.rs", content)?;
//! let mut out = Vec::new();
//! GitFormat::new().with_line_numbers(true).write(
//!     &mut out,
//!     b"src/main.rs",
//!     content,
//!     &matches,
//!     &searcher,
//! )?;
//! assert_eq!(out, b"src/main.rs:1:fn main() {}\n");
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::path::PathBuf;

use crate::display::quote_path;

mod files;
mod format;
mod query;
mod search;
#[cfg(test)]
mod tests;

pub use files::{Found, Scope, Source, grep_directory};
pub use format::{GitFormat, Listing};
pub use query::{Expression, Matching, PatternError, Query, Syntax};
pub use search::{Binary, FileMatches, Line, LineKind, Options, Searcher};

/// A search could not finish.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The attributes of a path could not be read, or its diff driver's function pattern does
    /// not compile. An empty path stands for loading the attribute files.
    #[error("reading attributes of {}", quote_path(.path))]
    Attributes {
        /// Repository-relative path.
        path: Vec<u8>,
        /// Attribute or driver failure.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The index, a tree or a blob could not be read.
    #[error("reading objects")]
    Object(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// A file or directory exists but could not be read.
    #[error("reading {}", .path.display())]
    Read {
        /// Native path.
        path: PathBuf,
        /// I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A path has no native form on this platform.
    #[error("path {} cannot be represented here", quote_path(.0))]
    Path(Vec<u8>),
    /// Ignore files could not be read.
    #[error("reading ignore rules")]
    Ignore(#[source] Box<crate::ignore::LoadError>),
    /// Untracked files could not be listed.
    #[error("listing untracked files")]
    Untracked(#[source] Box<crate::status::Error>),
}

impl Error {
    fn object(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Object(Box::new(source))
    }
}
