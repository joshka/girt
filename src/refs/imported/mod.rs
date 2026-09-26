//! Bounded imported history, independent of canonical append construction.

mod names;
mod read;
mod record;

pub use read::{ImportedReflog, ReflogLimits, ReflogReadEnd};
pub use record::{ImportedRecord, ReflogFields, ReflogInterpretationError};

#[cfg(test)]
mod tests;
