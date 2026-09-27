//! Git references, conditional transactions and reflogs across files and reftable storage.
//!
//! # Names, reads, and updates
//!
//! - [`RefName`] validates a full reference name. [`References`] reads stored [`Target`] values and
//!   resolves symbolic chains without loading their objects.
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

mod enumerate;
mod imported;
mod name;
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
pub use name::{InvalidRefName, RefName};
pub use reflog::{Reflog, ReflogEntry, ReflogRecord};
pub use store::{Backend, Expected, ReferenceError, References, Resolution, Target};
pub use transaction::{LogOutcome, RefEdit, RefEditOutcome, RefOutcome, TransactionError};
