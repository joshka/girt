//! Git references, conditional transactions and reflogs across files and reftable storage.
//!
//! # Names, reads, and updates
//!
//! - [`RefName`] validates a full reference name. [`References`] reads stored [`Target`] values and
//!   resolves symbolic chains without loading their objects. [`References::read_observation`]
//!   captures a stored target together with its backend-provided peeled hint.
//! - [`References::transaction`] checks a batch of [`RefEdit`] values before sequential
//!   publication. Choose [`Reflog`] policy explicitly and inspect [`RefEditOutcome`] after a
//!   partial failure; the no-reflog methods deliberately omit history.
//! - [`References::imported_reflog`] reads existing history for retention or analysis.
//!   [`ImportedReflog::is_complete`] distinguishes a full read from recovered records.
//! - [`reftable`] exposes bounded stack snapshots and compaction for repositories using that
//!   backend. Files and reftable have different publication and cleanup contracts.
//!
//! The files backend reads loose refs before packed refs and separates symbolic resolution from
//! object lookup.
//! [`References::list_controlled`] bounds files enumeration before a GC inventory retains names;
//! reftable uses its separate stack budgets.
//!
//! [`References::imported_reflog`] reads bounded imported history. [`ImportedRecord`] preserves raw
//! data and separates [`ImportedRecord::fields`] from canonical append validation.
//!
//! # Example
//!
//! Publish a first commit through `HEAD` onto the unborn `main` branch, then resolve it. The
//! transaction checks every expected value before publishing anything:
//!
//! ```
//! use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
//! use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
//!
//! let directory = tempfile::tempdir()?;
//! let repository = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("repo.git"),
//!     InitKind::Bare,
//! )?;
//! let loose = repository.loose_objects();
//! let person = Signature {
//!     name: b"A. Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//! let commit = loose.write_commit(&Commit::new(CommitFields {
//!     tree: loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?,
//!     parents: vec![],
//!     author: person.clone(),
//!     committer: person.clone(),
//!     extra_headers: vec![],
//!     message: b"Initial commit\n".to_vec(),
//! })?)?;
//!
//! let references = repository.references()?;
//! let head = RefName::new("HEAD")?;
//! references.transaction(&[RefEdit {
//!     name: head.clone(),
//!     dereference: true,
//!     target: Some(Target::Direct(commit)),
//!     expected: Expected::Absent,
//!     reflog: Reflog::Append {
//!         committer: person,
//!         message: b"commit (initial)".to_vec(),
//!     },
//! }])?;
//!
//! let main = RefName::new("refs/heads/main")?;
//! assert_eq!(references.read(&main)?, Some(Target::Direct(commit)));
//! let resolved = references.resolve(&head, 5)?;
//! assert_eq!((resolved.name, resolved.id), (main.clone(), Some(commit)));
//! assert_eq!(references.reflog(&main)?.map(|log| log.len()), Some(1));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod enumerate;
#[cfg(test)]
pub(crate) use enumerate::LOOSE_SCANS;
mod imported;
mod leaf_symlink;
mod lock_wait;
mod name;
mod pack;
mod packed;
mod reflog;
pub mod reftable;
mod store;
mod transaction;

pub use enumerate::Reference;
pub use imported::{
    ImportedRecord, ImportedReflog, ReflogFields, ReflogInterpretationError, ReflogLimits,
    ReflogReadEnd,
};
pub use lock_wait::{FilesTransactionOptions, LockWait};
pub use name::{InvalidRefName, RefName};
pub use pack::{PackRefsError, PackRefsOptions, PackRefsReport, PackScope, PackTrigger};
pub use reflog::{Reflog, ReflogEntry, ReflogRecord};
pub use store::{
    Backend, Expected, ReferenceError, ReferenceObservation, References, Resolution, Target,
};
pub use transaction::{LogOutcome, RefEdit, RefEditOutcome, RefOutcome, TransactionError};

#[cfg(test)]
mod observation_tests;
