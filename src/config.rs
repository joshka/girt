//! Byte-oriented Git configuration parsing and explicit layered resolution.
//!
//! [`Config::parse`] is pure. [`Config::resolve`] reads only supplied [`ConfigInputs`], expands
//! includes, and retains ordered occurrences with [`Origin`] provenance. Consumers own typed
//! interpretation and reset semantics. Re-resolve to refresh an immutable snapshot.
//!
//! [`crate::Repository::open_with_config`] adds repository sources while keeping format bootstrap
//! separate. [`Document`] edits direct-file syntax; [`ConfigEdit`] holds an exclusive file lock
//! through publication. No operation reads or mutates process-global environment.
mod document;
mod edit;
pub use edit::{ConfigEdit, EditError};
mod parse;
pub use document::Document;
mod resolution;
mod sources;
mod values;
mod wildmatch;
pub use parse::{Config, ConfigError, Entry};
pub use resolution::{ResolveError, ResolveFailure};
pub use sources::{
    ConfigFile, ConfigInputs, ConfigScope, IncludeContext, Origin, ResolveLimits, SourceLocation,
};
pub(crate) use values::integer;
