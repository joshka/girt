//! Git attributes (`.gitattributes`) over repository-relative byte paths.
//!
//! [`Attributes`] holds attribute files the caller has already read: add each one with its
//! [`Source`], then ask for one attribute of a path with [`Attributes::get`]. To read the files
//! Git consults for a repository instead, call
//! [`Repository::attributes`](crate::Repository::attributes), which returns a [`Lookup`] that reads
//! per-directory `.gitattributes` files on demand.
//!
//! Precedence follows Git, lowest first: the system file, the global file (`core.attributesFile`),
//! per-directory `.gitattributes` files from the root down, then `info/attributes`. Within one file
//! a later line wins, and within one line a later assignment wins. A line's pattern is matched as
//! in `.gitignore`, relative to the directory of its file, except that a pattern ending in `/`
//! matches no file and negative patterns are ignored. Macros (`[attr]name ...`) are honoured in
//! every source except a `.gitattributes` below the root, as Git does; `binary` is the built-in
//! macro for `-diff -merge -text`. A line naming an invalid attribute is ignored entirely.
//!
//! [`Attributes`] reads no filesystem, index, environment or configuration. Paths are
//! slash-separated bytes on every platform. Lines are LF- or CRLF-terminated and an initial UTF-8
//! BOM is skipped. Quoted patterns use Git's C-style escapes.
//!
//! ```
//! use girt::attributes::{Attributes, Case, Limits, Source, State};
//!
//! let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
//! attributes.add(Source::Directory(b""), b"*.png binary\n*.rs diff=rust\n")?;
//! attributes.add(Source::Directory(b"vendor"), b"*.rs -diff\n")?;
//! assert_eq!(
//!     attributes.get(b"src/main.rs", b"diff")?,
//!     State::Value(b"rust")
//! );
//! assert_eq!(attributes.get(b"vendor/lib.rs", b"diff")?, State::Unset);
//! assert_eq!(attributes.get(b"logo.png", b"diff")?, State::Unset); // From the `binary` macro.
//! assert_eq!(attributes.get(b"README", b"diff")?, State::Unspecified);
//! # Ok::<(), girt::attributes::Error>(())
//! ```

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository input (#103)"
)]

mod lookup;
mod rules;

pub use lookup::{DirectorySource, LoadError, Lookup};
pub(crate) use rules::valid_name;
pub use rules::{Attributes, Error, Limits, Source, State};

pub use crate::ignore::Case;

#[cfg(test)]
mod tests;
