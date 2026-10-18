use std::sync::atomic::AtomicBool;

use super::{
    ConflictStyle, ContentHunk, ContentMerge, ContentOptions, Favor, MarkerOptions, content_with,
};
use crate::Config;
use crate::attributes::State;
use crate::content_diff::BinaryMode;

/// Attribute-selected content driver. Custom command execution belongs to the caller.
///
/// Selection borrows custom names from attributes or configuration. Match [`Self::Custom`] to
/// invoke a caller-owned driver; [`Self::merge`] refuses that variant without executing commands.
/// A custom implementation can return [`ContentMerge::resolved`] or
/// [`ContentMerge::conflicted`] and retain exact original sides.
///
/// ```
/// use girt::attributes::State;
/// use girt::merge::{ContentOptions, Driver, MarkerOptions};
/// let driver = Driver::from_attribute(State::Value(b"union"));
/// let result = driver.merge(
///     b"base\n",
///     b"ours\n",
///     b"theirs\n",
///     ContentOptions::default(),
///     &std::sync::atomic::AtomicBool::new(false),
/// )?;
/// assert!(result.is_clean());
/// assert_eq!(result.render(&MarkerOptions::default()), b"ours\ntheirs\n");
/// # Ok::<(), girt::merge::DriverError>(())
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Driver<'a> {
    /// Unspecified merge attribute and no configured default: detect binary automatically.
    Automatic,
    /// Built-in text driver with automatic binary detection.
    Text,
    /// Built-in binary driver, retaining unresolved whole-file sides.
    Binary,
    /// Built-in text union driver.
    Union,
    /// Named caller-owned driver; name bytes can be empty or non-UTF-8.
    Custom(&'a [u8]),
}

impl<'a> Driver<'a> {
    /// Interprets a `merge` attribute state without reading configuration.
    ///
    /// Set means text, unset means binary, and unspecified means automatic. String values name
    /// text/binary/union or a custom driver. Attribute names and values are case-sensitive.
    pub fn from_attribute(state: State<'a>) -> Self {
        match state {
            State::Unspecified => Self::Automatic,
            State::Set => Self::Text,
            State::Unset => Self::Binary,
            State::Value(value) => Self::named(value),
        }
    }

    /// Uses `merge.default` only when the merge attribute is unspecified.
    ///
    /// Config scalar precedence is provided by [`Config`]. Implicit `merge.default` is treated
    /// as absent, like [`Config::string`]; explicit empty values name an empty custom driver.
    pub fn configured(state: State<'a>, config: &'a Config) -> Self {
        match state {
            State::Unspecified => config
                .string("merge", None, "default")
                .map_or(Self::Automatic, Self::named),
            _ => Self::from_attribute(state),
        }
    }

    fn named(name: &'a [u8]) -> Self {
        match name {
            b"text" => Self::Text,
            b"binary" => Self::Binary,
            b"union" => Self::Union,
            _ => Self::Custom(name),
        }
    }

    /// Runs a built-in driver without filesystem or process access.
    ///
    /// Driver selection overrides binary classification; union also selects union favoring.
    /// Other content limits and favoring are preserved. A custom name returns an explicit error;
    /// match it and invoke a caller-owned implementation instead. No process is spawned.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::CustomRequired`] for named custom drivers or
    /// [`DriverError::Content`] for bounded comparison/cancellation failures.
    pub fn merge(
        self,
        base: &[u8],
        ours: &[u8],
        theirs: &[u8],
        mut options: ContentOptions,
        cancel: &AtomicBool,
    ) -> Result<ContentMerge, DriverError> {
        match self {
            Self::Automatic => options.binary = BinaryMode::Auto,
            Self::Text => options.binary = BinaryMode::Auto,
            Self::Binary => options.binary = BinaryMode::Binary,
            Self::Union => {
                options.binary = BinaryMode::Auto;
                options.favor = Favor::Union;
            }
            Self::Custom(name) => return Err(DriverError::CustomRequired(name.to_vec())),
        }
        Ok(content_with(base, ours, theirs, options, cancel)?)
    }
}

impl ContentMerge {
    /// Constructs a caller-resolved payload, including arbitrary non-UTF-8 and NUL bytes.
    ///
    /// No comparisons or validation run. This lets a caller-owned custom driver return accepted
    /// bytes through the same result type as built-in drivers. No conflict markers are parsed.
    pub fn resolved(bytes: Vec<u8>) -> Self {
        Self {
            hunks: vec![ContentHunk::Resolved(bytes)],
            binary: false,
        }
    }

    /// Constructs an unresolved whole-input conflict for a caller-owned driver.
    ///
    /// Set `binary` to render ours verbatim; otherwise marker presentation can be chosen later.
    /// All payloads remain exact and no equality simplification or validation runs.
    pub fn conflicted(base: Vec<u8>, ours: Vec<u8>, theirs: Vec<u8>, binary: bool) -> Self {
        Self {
            hunks: vec![ContentHunk::Conflict { base, ours, theirs }],
            binary,
        }
    }
}

impl MarkerOptions {
    /// Applies effective `merge.conflictStyle`, keeping other default marker options.
    ///
    /// Absent configuration selects merge. Accepted values are exactly `merge`, `diff3` and
    /// `zdiff3`; value bytes are case-sensitive, as observed through Git's executable.
    ///
    /// # Errors
    ///
    /// Returns [`DriverError::ConflictStyle`] for implicit, empty or unknown values.
    pub fn from_config(config: &Config) -> Result<Self, DriverError> {
        let style = match config.value("merge", None, "conflictstyle") {
            None => ConflictStyle::Merge,
            Some(Some(b"merge")) => ConflictStyle::Merge,
            Some(Some(b"diff3")) => ConflictStyle::Diff3,
            Some(Some(b"zdiff3")) => ConflictStyle::Zdiff3,
            Some(value) => return Err(DriverError::ConflictStyle(value.map(<[u8]>::to_vec))),
        };
        Ok(Self {
            style,
            ..Self::default()
        })
    }
}

/// Driver selection or built-in computation failed without executing a command.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DriverError {
    /// The named driver needs caller-owned execution; the bytes are retained for dispatch.
    #[error("custom merge driver requires caller execution")]
    CustomRequired(Vec<u8>),
    /// Invalid configured marker style; `None` denotes an implicit assignment.
    #[error("invalid merge.conflictStyle")]
    ConflictStyle(Option<Vec<u8>>),
    /// Content comparison failed with its original cause.
    #[error("running built-in merge driver")]
    Content(#[from] super::Error),
}
