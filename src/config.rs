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
//!
//! # Example
//!
//! Section and key names compare case-insensitively; the last occurrence of a scalar wins, and a
//! key without `=` is an implicit true:
//!
//! ```
//! use girt::Config;
//!
//! let config = Config::parse(b"[core]\n\tbare = false\n[Core]\n\tBare = true\n\tquiet\n")?;
//! assert_eq!(config.value("core", None, "bare"), Some(Some(&b"true"[..])));
//! assert_eq!(config.boolean("core", None, "bare")?, Some(true));
//! assert_eq!(config.value("core", None, "quiet"), Some(None));
//! assert_eq!(config.boolean("core", None, "quiet")?, Some(true));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
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
pub use parse::{Config, ConfigError, ConfigSection, Entry, InvalidValue};
pub use resolution::{ResolveError, ResolveFailure};
pub use sources::{
    ConfigFile, ConfigInputs, ConfigScope, IncludeContext, Origin, ResolveLimits, SourceLocation,
};
pub(crate) use values::{boolean, integer};
pub(crate) use wildmatch::matches_without_pathname;

#[cfg(test)]
mod options_tests;

#[cfg(test)]
mod user_home_tests;
