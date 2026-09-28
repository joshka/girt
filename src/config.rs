//! Byte-oriented Git configuration parsing and explicit layered resolution.
//!
//! # Parsing, resolving, and editing configuration
//!
//! - [`Config::parse`] reads one supplied byte source without I/O.
//! - [`Config::from_decoded_sections`] preserves supplied decoded occurrences without parsing.
//! - [`Config::resolve`] reads explicit [`ConfigInputs`], expands includes, and records [`Origin`]
//!   for each occurrence. Re-resolve when the caller needs a fresh snapshot.
//! - [`Document`] preserves direct-file syntax for editing; [`ConfigEdit`] holds the file lock
//!   through publication. Typed interpretation and policy remain with the caller.
//!
//! [`crate::Repository::open_with_config`] adds repository sources while keeping format bootstrap
//! separate. No operation reads or mutates process-global environment.
mod command_environment;
mod decoded;
pub use decoded::{DecodedEntry, DecodedSection};
mod document;
mod edit;
pub use edit::{ConfigEdit, EditError};
mod parse;
pub use document::{Document, DocumentSection};
mod options;
pub use options::{
    IncludeConditionVisibility, IncludeDirectiveCase, ResolveOptions, UnresolvedIncludePath,
};
mod placement;
pub use placement::IncludePlacement;
mod resolution;
mod sources;
mod values;
mod wildmatch;
pub use parse::{Config, ConfigError, ConfigSection, Entry};
pub use resolution::{ResolveError, ResolveFailure};
pub use sources::{
    ConfigFile, ConfigInputs, ConfigScope, IncludeContext, Origin, ResolveLimits, SourceLocation,
};
pub(crate) use values::{boolean, integer};

#[cfg(test)]
mod options_tests;
