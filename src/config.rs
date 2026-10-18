//! Byte-oriented Git configuration parsing and explicit layered resolution.
//!
//! Configuration has three distinct forms. [`Config`] is an ordered set of decoded occurrences
//! for lookup. [`ConfigInputs`] selects sources and include context for a fresh effective snapshot.
//! [`Document`] retains one file's syntax for editing. An effective snapshot can contain many
//! sources; editing it as if it were one file would lose both provenance and precedence.
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
//! # Values and precedence
//!
//! Section and variable names fold ASCII case; quoted subsection bytes compare exactly. Scalar
//! lookup uses the last occurrence, while [`Config::values`] preserves repeated occurrences.
//! Repeated variables have consumer-specific semantics: a remote's URLs or refspecs are not
//! necessarily a last-value scalar. [`Entry::origin`] identifies the source and include ancestry
//! after resolution, which helps explain why an effective setting won.
//!
//! Absence, an implicit assignment, and an explicit empty value are different. [`Config::value`]
//! returns `None`, `Some(None)`, and `Some(Some(b""))` respectively. [`Config::boolean`] interprets
//! implicit as true and empty as false. [`Config::string`] treats implicit as absent. Select typed
//! interpretation according to the variable, then apply policy separately.
//!
//! Resolution orders files by scope, preserving caller order within each scope, then applies
//! supplied environment and command overrides. Includes expand in place by default. Repository
//! opening adds local/worktree sources but keeps direct format bootstrap separate: overrides
//! cannot reinterpret stored SHA-1 objects as SHA-256. Supply user and system sources explicitly;
//! [`ConfigInputs::git_default`] selects the ones `git` reads from a caller-supplied environment.
//!
//! # Errors, refresh, and editing
//!
//! [`ConfigError`] identifies invalid syntax and its physical line. [`InvalidValue`] means syntax
//! parsed but the requested type cannot represent the selected value. [`ResolveError`] adds source
//! and include ancestry around I/O, cycles, missing expansion context, and exhausted budgets.
//! Fix the indicated source or context; increase budgets only after considering trusted input and
//! resource cost. Parsing and resolution run no commands and never mutate the process environment.
//!
//! [`ConfigEdit`] locks a selected direct file until commit or abort. Its draft is a [`Document`],
//! not the effective snapshot. Edits preserve unrelated bytes but do not follow includes or update
//! repository handles. After publication, re-resolve or reopen before using the new settings.
//! [`EditError`] distinguishes lock contention, changed source, invalid edits, publication, and
//! cleanup failures. Obtain a fresh lock and reread before retrying; inspect residual owned locks
//! rather than removing another writer's lock. See [`ConfigEdit`] for filesystem assumptions.
//!
//! Parsing allocates with input size. [`ResolveLimits`] bounds source bytes, entries, include
//! depth, and matching work; snapshots are owned and are not automatically refreshed. Files read in
//! one resolution need not come from a single moment under concurrent external writers.
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
pub(crate) mod values;
pub use parse::{Config, ConfigError, ConfigSection, Entry, InvalidValue};
pub use resolution::{ResolveError, ResolveFailure};
pub(crate) use sources::system_file;
pub use sources::{
    ConfigFile, ConfigInputs, ConfigScope, IncludeContext, Origin, ResolveLimits, SourceLocation,
};
pub use values::parse_boolean;
pub(crate) use values::{boolean, integer};

#[cfg(test)]
mod options_tests;

#[cfg(test)]
mod user_home_tests;

#[cfg(doctest)]
#[doc = include_str!("../site/src/content/docs/guides/configuration.md")]
mod website_configuration_guide {}
