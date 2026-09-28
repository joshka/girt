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
