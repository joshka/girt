//! Git's files reference backend: names, conditional transactions, and reflogs.
//!
//! [`References`] reads loose refs before packed refs and separates symbolic resolution from
//! object lookup. [`References::transaction`] checks a batch before sequential publication and
//! exposes partial results. [`Reflog`] selects explicit history policy; the separately named
//! no-reflog methods remain available for callers that deliberately omit recovery records.

mod enumerate;
mod name;
mod packed;
mod reflog;
mod store;
mod transaction;

pub use enumerate::Reference;
pub use name::{InvalidRefName, RefName};
pub use reflog::{Reflog, ReflogEntry};
pub use store::{Expected, ReferenceError, References, Resolution, Target};
pub use transaction::{LogOutcome, RefEdit, RefEditOutcome, RefOutcome, TransactionError};
