//! SHA-1/SHA-256 Git index v2 entries and synchronous per-worktree storage.
//!
//! An index records candidate tree entries and cached filesystem metadata. [`Index`] validates
//! format structure without resolving objects or touching working files. Paths remain bytes;
//! format validity does not establish checkout safety on any host filesystem.
//!
//! Versions 3/4, extended flags (intent-to-add and skip-worktree), sparse directory entries and
//! mandatory extensions (including split/sparse indexes) are unsupported. Assume-valid is
//! preserved as data; no stat-skipping policy is implemented. Optional extensions are opaque and
//! round-trip unchanged. Editing discards only the derived `TREE` cache; other extensions block
//! edits, including resolve-undo information. See [`Index::replace_entries`].
//!
//! [`crate::Repository::read_index`] distinguishes absence from an empty index.
//! [`crate::Repository::edit_index`] locks before reading; [`IndexEdit::commit`] publishes the
//! edited snapshot. These operations are synchronous and never stage content, scan a worktree,
//! run hooks, or honor environment overrides such as `GIT_INDEX_FILE`.
//!
//! ```
//! use girt::ObjectId;
//! use girt::index::{Entry, Index, Limits, Mode};
//! let entry = Entry::new(
//!     b"hello.txt".to_vec(),
//!     Mode::Regular,
//!     ObjectId::for_blob(girt::ObjectFormat::Sha1, b"hello\n"),
//! );
//! let index = Index::new(girt::ObjectFormat::Sha1, vec![entry], Limits::default())?;
//! let bytes = index.encode(Limits::default())?;
//! assert_eq!(
//!     Index::parse(girt::ObjectFormat::Sha1, &bytes, Limits::default())?,
//!     index
//! );
//! # Ok::<(), girt::index::Error>(())
//! ```
mod codec;
mod entries;
mod store;

pub use codec::{Error, Extension, Index, Limits};
pub use entries::{Entry, Mode, Stage, Stat, Timestamp};
pub use store::{IndexEdit, StorageError};
