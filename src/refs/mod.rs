//! Git references, conditional transactions and reflogs across files and reftable storage.
//!
//! References give mutable names to immutable Git objects. A branch is a direct reference such as
//! `refs/heads/main`; a symbolic reference stores another name. `HEAD` often names a branch that
//! does not exist yet (an unborn branch). A lightweight tag is a reference; an annotated tag also
//! needs a [`Tag`](crate::Tag) object. Reading a reference does not establish object existence or
//! kind. Resolve symbolic chains first, then use [`Objects::peel`](crate::Objects::peel) if the
//! resulting object may be an annotated tag.
//!
//! # Choose the operation
//!
//! | Need | Operation |
//! | --- | --- |
//! | Inspect the stored value, including symbolic names | [`References::read`] |
//! | Follow symbolic names to a terminal name and optional ID | [`References::resolve`] |
//! | Enumerate `refs/` names, excluding `HEAD` | [`References::list`] |
//! | Enumerate one namespace, or names by byte prefix | [`References::list_namespace`], [`References::list_prefix`] |
//! | Publish with preconditions and explicit history policy | [`References::transaction`] |
//! | Inspect complete interpreted history | [`References::reflog`] |
//! | Recover bounded imported records and detect incompleteness | [`imported_reflog`](References::imported_reflog) |
//!
//! # Names, reads, and updates
//!
//! - [`RefName`] validates a full reference name; [`RefName::branch`], [`RefName::tag`] and
//!   [`RefName::remote_branch`] build the conventional ones from short names. [`References`] reads
//!   stored [`Target`] values and resolves symbolic chains without loading their objects.
//!   [`References::read_observation`] captures a stored target together with its backend-provided
//!   peeled hint.
//! - [`References::transaction`] checks a batch of [`RefEdit`] values before sequential
//!   publication. [`RefEdit::create`], [`RefEdit::update`] and [`RefEdit::delete`] build the common
//!   direct edits. Choose [`Reflog`] policy explicitly, or take Git's from
//!   [`Repository::reflog_for`](crate::Repository::reflog_for), and inspect [`RefEditOutcome`]
//!   after a partial failure; the no-reflog methods deliberately omit history.
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
//! # Preconditions and recovery
//!
//! Choose [`Expected::Absent`] for creation or [`Expected::Value`] for compare-and-swap updates.
//! These conditions are checked under locks; a preceding read alone cannot protect against another
//! writer. A mismatch means re-read and reconsider the intended update. Using [`Expected::Any`]
//! deliberately gives up that protection. [`Expected::Exists`] permits replacement of any existing
//! value, while [`Expected::AbsentOr`] permits absence or one specific value; neither is a
//! fast-forward policy. Object installation must precede reference publication;
//! transactions do not validate the objects they name.
//!
//! [`RefEdit`] defaults to editing the stored name. With [`Dereference::Follow`], editing `HEAD`
//! changes its terminal branch while preserving `HEAD`'s symbolic spelling. Choose [`Reflog`]
//! explicitly; low-level transactions do not infer logging policy from Git configuration.
//!
//! [`Reflog::AppendExistingIfChanged`] appends only changed values to already-existing logs;
//! [`Reflog::Delete`] removes the edited name's log on deletion. Reflogs describe name changes,
//! not commit ancestry, and may contain IDs no longer present in storage.
//!
//! [`TransactionError::Prepare`] changes no ref or log contents. [`TransactionError::Publish`]
//! includes effects already made; files publication is sequential and has no rollback. Inspect
//! outcomes and read fresh state before retrying. Files transactions already wait for
//! [`FilesTransactionOptions::GIT_DEFAULT`] before reporting contended reference/packed locks;
//! reflog locks fail immediately. Use [`References::transaction_files_with_options`] for explicit
//! waits or cancellation. A remaining lock conflict requires a caller policy decision, never
//! deleting another writer's lock. Malformed loose refs
//! shadow packed refs and fail explicitly; they do not disappear into a fallback value.
//!
//! Reads are synchronous. Files reads are live across names; reftable reads take bounded stack
//! snapshots, with separate reads not necessarily sharing a generation. See [`References`] for
//! trusted-filesystem and backend-specific atomicity contracts. [`reftable::Snapshot`] is the
//! lower-level choice for retaining one complete stack generation.
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
//!
//! let loose = repository.loose_objects();
//! let person = Signature {
//!     name: b"A. Writer".to_vec(),
//!     email: b"writer@example.com".to_vec(),
//!     seconds: 1_700_000_000,
//!     offset_minutes: 0,
//! };
//!
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
//! let head = RefName::HEAD;
//! let edit = RefEdit::new(
//!     head.clone(),
//!     Some(Target::Direct(commit)),
//!     Expected::Absent,
//!     Reflog::Append {
//!         committer: person,
//!         message: b"commit (initial)".to_vec(),
//!     },
//! );
//! references.transaction(&[edit.with_dereference(girt::refs::Dereference::Follow)])?;
//!
//! let main = RefName::new("refs/heads/main")?;
//! assert_eq!(references.read(&main)?, Some(Target::Direct(commit)));
//! let resolved = references.resolve(&head)?;
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
pub(crate) use packed::PackedCache;
mod reflog;
pub mod reftable;
mod store;
mod transaction;

pub use enumerate::Reference;
pub use imported::{
    ImportedRecord, ImportedReflog, ReflogFields, ReflogInterpretationError, ReflogLimits,
    ReflogReadEnd,
};
pub use lock_wait::{FilesTransactionLimits, FilesTransactionOptions, LockWait};
pub use name::{InvalidRefName, RefName};
pub use pack::{
    PackRefsError, PackRefsLimits, PackRefsOptions, PackRefsReport, PackScope, PackTrigger,
};
pub use reflog::{Reflog, ReflogEntry, ReflogRecord};
pub use store::{
    Backend, Expected, ReferenceError, ReferenceObservation, References, Resolution, Target,
};
pub use transaction::{
    Dereference, LogOutcome, RefEdit, RefEditOutcome, RefOutcome, TransactionError,
};

#[cfg(test)]
mod observation_tests;

/// Parses `bytes` as one files-backend format and checks re-encoding where girt encodes it.
///
/// `kind` selects a loose reference, a packed-refs file, a reflog or a reference name; see
/// [`crate::fuzzing::refs`].
#[cfg(any(fuzzing, test))]
pub(crate) fn fuzz_files(format: crate::ObjectFormat, kind: u8, bytes: &[u8]) {
    let path = std::path::Path::new("<fuzz>");
    match kind % 4 {
        0 => {
            if let Ok(Target::Symbolic(name)) = store::parse_loose(format, bytes, path) {
                assert!(bytes.trim_ascii_end().ends_with(name.as_bytes()));
            }
        }
        1 => {
            let _ = packed::parse(format, bytes, path);
        }
        2 => {
            let Ok(records) = ReflogRecord::parse(format, bytes) else {
                return;
            };
            for record in &records {
                let Ok(encoded) = record.entry().encode() else {
                    continue;
                };
                let reparsed = ReflogEntry::parse(format, &encoded).expect("encoded entry parses");
                assert_eq!(reparsed.as_slice(), std::slice::from_ref(record.entry()));
            }
        }
        _ => {
            if let Ok(name) = RefName::new(bytes) {
                assert_eq!(name.as_bytes(), bytes);
            }
        }
    }
}

#[cfg(doctest)]
#[doc = include_str!("../../site/src/content/docs/guides/refs.md")]
mod website_refs_guide {}

// Share the existing Git lock protocol with stash log publication.
pub(crate) use store::Lock as FileLock;
