//! SHA-1/SHA-256 Git index v2/v3/v4 entries and synchronous per-worktree storage.
//!
//! An index records candidate tree entries and cached filesystem metadata. [`Index`] validates
//! format structure without resolving objects or touching working files. Paths remain bytes;
//! format validity does not establish checkout safety on any host filesystem.
//!
//! Intent-to-add, skip-worktree and assume-valid are retained as data; no staging, sparse-checkout
//! or stat-skipping policy is implemented. Sparse directory entries and mandatory extensions
//! (including split/sparse indexes) are explicitly unsupported. Optional extensions are opaque and
//! round-trip unchanged. Editing discards derived caches, retains resolve-undo records and refuses
//! unknown optional extensions. See [`Index::replace_entries`].
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

pub use codec::{Error, Extension, Index, Limits, Version};
pub use entries::{Entry, Mode, Stage, Stat, Timestamp};
pub use store::{IndexEdit, StorageError};
