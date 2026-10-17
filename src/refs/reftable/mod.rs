//! Immutable reftable records and bounded table decoding.
//!
//! [`Table`] reads or writes individual records under [`Limits`]; [`RefRecord`] and [`LogRecord`]
//! expose reference targets and decoded reflog fields. [`Snapshot`] reads a bounded stack under
//! [`StackLimits`] when one generation must be retained across lookups. [`compact`] publishes a
//! replacement stack; ordinary repository reference reads and edits belong to
//! [`crate::refs::References`].
//!
//! This codec preserves stored reference and reflog records, including tombstones. Stack
//! precedence and filesystem publication are separate operations. Binary log timestamps retain
//! their full unsigned range independently of [`crate::Signature`]'s signed interpretation.
//!
//! Timezone fields use Git's observed signed decimal `HHMM` encoding; public offsets remain
//! semantic minutes. This interim choice differs from the published signed-minute description.
//! Earlier girt minute-encoded tables have no discriminator and now receive Git's interpretation.
//! Noncanonical minute digits are arithmetic and normalize when re-encoded. See the
//! [compatibility decision](https://github.com/joshka/girt/blob/main/docs/reference-transactions.md#reflog-timezone-interoperability).
//!
//! # Choose the repository or codec layer
//!
//! A reftable repository stores refs and reflogs together in immutable tables selected by
//! `tables.list`. Later tables override older keys; tombstones hide deletions. Use
//! [`crate::refs::References`] for ordinary reads and conditional transactions, regardless of
//! backend. Use [`Table`] to inspect/encode individual byte buffers, [`Snapshot`] for repeated
//! reads of one merged generation, and [`compact`] to reduce a stack without expiring history.
//!
//! Compaction is separate from object GC and reflog expiry. It retains all live history,
//! publishes a replacement list, then tries to remove old unlisted tables. Successful
//! [`Compaction`] can contain cleanup failures: inspect `retained`. No fsync or power-loss
//! durability is promised by ordinary compaction; destructive history expiry uses the
//! maintenance path and its isolation/durability contract instead.
//!
//! # Example
//!
//! Each reference update in a reftable repository appends a table to the stack. A [`Snapshot`]
//! merges the stack, and [`compact`] replaces it with one table:
//!
//! ```
//! use std::sync::atomic::AtomicBool;
//!
//! use girt::refs::reftable::{Snapshot, StackLimits, compact};
//! use girt::refs::{Backend, Expected, RefName, Target};
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let path = directory.path().join("repo.git");
//! let repository =
//!     Repository::init_with_backend(ObjectFormat::Sha1, path, InitKind::Bare, Backend::Reftable)?;
//! let id = repository.loose_objects().write_blob(b"tagged content\n")?;
//! let references = repository.references()?;
//! for name in ["refs/tags/one", "refs/tags/two"] {
//!     references.update_without_reflog(
//!         &RefName::new(name)?,
//!         Target::Direct(id),
//!         Expected::Absent,
//!     )?;
//! }
//!
//! let stack = repository.git_dir().join("reftable");
//! let cancel = AtomicBool::new(false);
//! let snapshot = Snapshot::read(&stack, ObjectFormat::Sha1, StackLimits::default(), &cancel)?;
//! let tags = snapshot
//!     .table
//!     .references
//!     .iter()
//!     .filter(|record| record.target == Some(Target::Direct(id)));
//! assert_eq!(tags.count(), 2);
//!
//! let compaction = compact(&stack, ObjectFormat::Sha1, StackLimits::default(), &cancel)?;
//! assert!(compaction.input_tables >= 2);
//! // `tables.list` names the stack's tables; one remains after compaction.
//! assert_eq!(
//!     std::fs::read_to_string(stack.join("tables.list"))?
//!         .lines()
//!         .count(),
//!     1
//! );
//! assert_eq!(
//!     references.read(&RefName::new("refs/tags/two")?)?,
//!     Some(Target::Direct(id))
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub(crate) mod backend;
mod decode;
mod encode;
mod records;
mod stack;
mod timezone;
pub use records::{Error, Limits, LogRecord, LogValue, RecordName, RefRecord, Table};
pub use stack::{Compaction, Snapshot, StackLimits, compact};
#[cfg(unix)]
pub(crate) use stack::{ExpireStackError, expire_logs};

#[cfg(test)]
mod codec_tests;
#[cfg(test)]
mod decode_tests;

#[cfg(test)]
mod stack_tests;
