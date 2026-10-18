//! Content conversion between the repository and the working tree.
//!
//! Git stores a file's content after converting it from its working-tree form, and converts it
//! back when writing the file. [`Pipeline`] makes the same conversions for one repository: call
//! [`Repository::filters`](crate::Repository::filters) to load its attribute files and
//! configuration, then [`Pipeline::to_repository`] before hashing or staging a working file and
//! [`Pipeline::to_worktree`] before writing a blob to the working tree. [`Pipeline::plan`] reports
//! what applies to one path without converting anything.
//!
//! Supported conversions, in the order applied when storing (writing reverses it):
//!
//! - **Line endings.** `.gitattributes` `text`, `-text`, `text=auto` and `eol=lf|crlf`, with
//!   `core.autocrlf` and `core.eol` choosing defaults as [`Text`] and [`Plan::eol`] describe.
//!   `core.safecrlf` decides whether a conversion that would not reproduce the working file is
//!   allowed, reported as a [`CrlfWarning`], or refused with [`Error::UnsafeCrlf`].
//! - **`ident`.** `$Id$` in a stored blob is written as `$Id: <blob id> $`, and collapsed back when
//!   storing.
//!
//! Filter drivers (`filter=<name>`) and `working-tree-encoding` are not applied yet: a path with
//! either is refused with [`Error::Unsupported`] rather than converted partially.
//!
//! ```
//! use std::borrow::Cow;
//!
//! use girt::ObjectId;
//! use girt::attributes::{Attributes, Case, Limits, Source};
//! use girt::filter::{Pipeline, Settings, Staged};
//!
//! let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
//! attributes.add(Source::Directory(b""), b"*.bat text eol=crlf\n")?;
//! let mut filters = Pipeline::from_attributes(attributes, Settings::default());
//!
//! let stored = filters.to_repository(b"run.bat", b"echo\r\n", Staged::Absent)?;
//! assert_eq!(&*stored.bytes, b"echo\n");
//! let written = filters.to_worktree(
//!     b"run.bat",
//!     b"echo\n",
//!     ObjectId::null(girt::ObjectFormat::Sha1),
//! )?;
//! assert_eq!(&*written, b"echo\r\n");
//! assert!(matches!(written, Cow::Owned(_)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::borrow::Cow;

use crate::attributes::{Attributes, LoadError, Lookup, State};
use crate::config::{Config, InvalidValue};
use crate::workflow::Environment;
use crate::{ObjectId, Objects, ReadBlobError, Repository};

mod eol;
mod ident;

use eol::Stats;

/// How `core.autocrlf` converts paths whose `text` attribute is unspecified.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum AutoCrlf {
    /// `false`: no conversion unless attributes ask for it.
    #[default]
    False,
    /// `true`: text files are stored with LF and written with CRLF.
    True,
    /// `input`: text files are stored with LF and written unchanged.
    Input,
}

/// A line ending written to the working tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum LineEnding {
    /// `\n`.
    Lf,
    /// `\r\n`.
    Crlf,
}

impl LineEnding {
    /// The platform's line ending, which `core.eol=native` selects: CRLF on Windows, LF elsewhere.
    pub const NATIVE: Self = if cfg!(windows) { Self::Crlf } else { Self::Lf };
}

/// What `core.safecrlf` does when a stored file would not be written back byte for byte.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum SafeCrlf {
    /// `false`: convert silently.
    Allow,
    /// `warn`, Git's default: convert and report a [`CrlfWarning`].
    #[default]
    Warn,
    /// `true`: refuse with [`Error::UnsafeCrlf`].
    Refuse,
}

/// The configuration that conversion consults.
///
/// [`Settings::default`] is Git's default: `core.autocrlf=false`, `core.eol=native`,
/// `core.safecrlf=warn`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settings {
    autocrlf: AutoCrlf,
    eol: LineEnding,
    safecrlf: SafeCrlf,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            autocrlf: AutoCrlf::False,
            eol: LineEnding::NATIVE,
            safecrlf: SafeCrlf::Warn,
        }
    }
}

impl Settings {
    /// Reads `core.autocrlf`, `core.eol` and `core.safecrlf`, the last value of each winning.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] for a value Git rejects: `core.autocrlf` other than a boolean or
    /// `input`, `core.eol` other than `lf`, `crlf` or `native`, and `core.safecrlf` other than a
    /// boolean or `warn`. Each comparison ignores ASCII case.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::filter::{AutoCrlf, Settings};
    ///
    /// let config = Config::parse(b"[core]\nautocrlf = input\n")?;
    /// assert_eq!(Settings::from_config(&config)?.autocrlf(), AutoCrlf::Input);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn from_config(config: &Config) -> Result<Self, InvalidValue> {
        let mut settings = Self::default();
        if let Some(value) = config.value("core", None, "autocrlf") {
            settings.autocrlf = if value.is_some_and(|value| value.eq_ignore_ascii_case(b"input")) {
                AutoCrlf::Input
            } else {
                match crate::config::boolean(value) {
                    Some(true) => AutoCrlf::True,
                    Some(false) => AutoCrlf::False,
                    None => return Err(InvalidValue::new("core", "autocrlf", value)),
                }
            };
        }
        if let Some(value) = config.value("core", None, "eol") {
            settings.eol = match value.map(<[u8]>::to_ascii_lowercase).as_deref() {
                Some(b"lf") => LineEnding::Lf,
                Some(b"crlf") => LineEnding::Crlf,
                Some(b"native") => LineEnding::NATIVE,
                _ => return Err(InvalidValue::new("core", "eol", value)),
            };
        }
        if let Some(value) = config.value("core", None, "safecrlf") {
            settings.safecrlf = if value.is_some_and(|value| value.eq_ignore_ascii_case(b"warn")) {
                SafeCrlf::Warn
            } else {
                match crate::config::boolean(value) {
                    Some(true) => SafeCrlf::Refuse,
                    Some(false) => SafeCrlf::Allow,
                    None => return Err(InvalidValue::new("core", "safecrlf", value)),
                }
            };
        }
        Ok(settings)
    }

    /// `core.autocrlf`.
    pub fn autocrlf(&self) -> AutoCrlf {
        self.autocrlf
    }

    /// `core.eol`, with `native` resolved to [`LineEnding::NATIVE`].
    pub fn eol(&self) -> LineEnding {
        self.eol
    }

    /// `core.safecrlf`.
    pub fn safecrlf(&self) -> SafeCrlf {
        self.safecrlf
    }

    /// Replaces `core.autocrlf`.
    #[must_use]
    pub fn with_autocrlf(mut self, autocrlf: AutoCrlf) -> Self {
        self.autocrlf = autocrlf;
        self
    }

    /// Replaces `core.eol`.
    #[must_use]
    pub fn with_eol(mut self, eol: LineEnding) -> Self {
        self.eol = eol;
        self
    }

    /// Replaces `core.safecrlf`.
    #[must_use]
    pub fn with_safecrlf(mut self, safecrlf: SafeCrlf) -> Self {
        self.safecrlf = safecrlf;
        self
    }
}

/// Whether a path's line endings are converted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Text {
    /// Never converted: `-text`, the `binary` macro, or an unspecified `text` with
    /// `core.autocrlf=false` and no `eol` attribute.
    Binary,
    /// Converted when the content looks like text: `text=auto`, or an unspecified `text` with
    /// `core.autocrlf` `true` or `input`.
    ///
    /// Content is binary when it contains NUL, a CR not followed by LF, or more than one
    /// nonprintable byte per 128 printable ones. A file is also stored unconverted when its staged
    /// blob already contains CRLF, and a blob containing CR is written unconverted.
    Auto,
    /// Always converted: `text`, or an `eol` attribute with `text` unspecified.
    Always,
}

/// The conversions that apply to one path, from [`Pipeline::plan`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Plan {
    text: Text,
    eol: LineEnding,
    ident: bool,
    safecrlf: SafeCrlf,
}

impl Plan {
    /// Whether line endings are converted.
    pub fn text(&self) -> Text {
        self.text
    }

    /// The line ending text files are written with, unless [`Self::text`] is [`Text::Binary`].
    ///
    /// The `eol` attribute wins; otherwise `core.autocrlf=true` writes CRLF, `input` writes LF,
    /// and `false` follows `core.eol`. Storing always converts CRLF to LF.
    pub fn eol(&self) -> LineEnding {
        self.eol
    }

    /// Whether the `ident` attribute is set.
    pub fn ident(&self) -> bool {
        self.ident
    }

    /// Converts `blob`, a stored blob with ID `id`, to its working-tree bytes.
    ///
    /// Borrows `blob` when nothing changes.
    pub fn to_worktree<'a>(&self, blob: &'a [u8], id: ObjectId) -> Cow<'a, [u8]> {
        let bytes = Cow::Borrowed(blob);
        let bytes = if self.ident {
            ident::expand(bytes, id)
        } else {
            bytes
        };
        self.eol_to_worktree(bytes)
    }

    fn eol_to_worktree<'a>(&self, bytes: Cow<'a, [u8]>) -> Cow<'a, [u8]> {
        if self.eol != LineEnding::Crlf {
            return bytes;
        }
        let needed = match self.text {
            Text::Binary => false,
            Text::Always => Stats::new(&bytes).lone_lf > 0,
            Text::Auto => {
                let stats = Stats::new(&bytes);
                stats.lone_lf > 0 && !stats.has_cr() && !stats.binary()
            }
        };
        eol::convert_if(bytes, needed, eol::lf_to_crlf)
    }

    /// Converts `file`, the bytes of the working file at `path`, to the content Git stores.
    ///
    /// `staged` supplies what [`Text::Auto`] consults: the path's currently staged blob.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsafeCrlf`] under [`SafeCrlf::Refuse`] when the result would not be
    /// written back as `file`, and [`Error::Staged`] when reading the staged blob fails.
    pub fn to_repository<'a>(
        &self,
        path: &[u8],
        file: &'a [u8],
        staged: Staged<'_>,
    ) -> Result<Stored<'a>, Error> {
        let stats = Stats::new(file);
        let convert = stats.crlf > 0
            && match self.text {
                Text::Binary => false,
                Text::Always => true,
                Text::Auto => !stats.binary() && !staged.has_crlf()?,
            };
        let bytes = eol::convert_if(Cow::Borrowed(file), convert, eol::crlf_to_lf);
        let warning = if self.text == Text::Binary {
            None
        } else {
            let written = self.eol_to_worktree(Cow::Borrowed(&*bytes));
            (*written != *file).then(|| {
                if stats.crlf > 0 && self.eol == LineEnding::Lf {
                    CrlfWarning::CrlfToLf
                } else {
                    CrlfWarning::LfToCrlf
                }
            })
        };
        // Renormalizing neither warns nor refuses, as `git add --renormalize` does.
        let safecrlf = if matches!(staged, Staged::Renormalize) {
            SafeCrlf::Allow
        } else {
            self.safecrlf
        };
        let warning = match (warning, safecrlf) {
            (Some(change), SafeCrlf::Refuse) => {
                return Err(Error::UnsafeCrlf {
                    path: path.to_vec(),
                    change,
                });
            }
            (warning, SafeCrlf::Warn) => warning,
            _ => None,
        };
        let bytes = if self.ident {
            ident::collapse(bytes)
        } else {
            bytes
        };
        Ok(Stored { bytes, warning })
    }
}

/// The staged blob [`Text::Auto`] consults when storing a file.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub enum Staged<'a> {
    /// The path has no staged blob.
    Absent,
    /// The staged blob's bytes.
    Blob(&'a [u8]),
    /// The staged blob's ID, read from `objects` only when the rule needs it.
    Object(&'a Objects, ObjectId),
    /// Ignore the staged blob and `core.safecrlf`, as `git add --renormalize` does.
    Renormalize,
}

impl Staged<'_> {
    fn has_crlf(self) -> Result<bool, Error> {
        let crlf = |bytes: &[u8]| bytes.windows(2).any(|pair| pair == b"\r\n");
        Ok(match self {
            Self::Absent | Self::Renormalize => false,
            Self::Blob(bytes) => crlf(bytes),
            Self::Object(objects, id) => crlf(
                &objects
                    .read_blob(id)
                    .map_err(|source| Error::Staged(Box::new(source)))?,
            ),
        })
    }
}

/// Content to store, from [`Pipeline::to_repository`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Stored<'a> {
    /// The bytes to hash and store; borrowed from the file when nothing changed.
    pub bytes: Cow<'a, [u8]>,
    /// Set under [`SafeCrlf::Warn`] when writing the stored bytes back would change the file.
    pub warning: Option<CrlfWarning>,
}

/// How writing a stored file back would change its line endings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CrlfWarning {
    /// The file's CRLF line endings would be written as LF.
    CrlfToLf,
    /// The file's LF line endings would be written as CRLF.
    LfToCrlf,
}

impl std::fmt::Display for CrlfWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CrlfToLf => "CRLF would be replaced by LF",
            Self::LfToCrlf => "LF would be replaced by CRLF",
        })
    }
}

/// Converting content failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Reading attributes for the path failed.
    #[error(transparent)]
    Attributes(#[from] LoadError),
    /// `core.safecrlf=true` refused a conversion.
    #[error("{change} in {}", String::from_utf8_lossy(path))]
    UnsafeCrlf {
        /// The path being stored.
        path: Vec<u8>,
        /// What writing the stored content back would change.
        change: CrlfWarning,
    },
    /// Reading the staged blob for [`Text::Auto`] failed.
    #[error("reading the staged blob")]
    Staged(#[source] Box<ReadBlobError>),
    /// The path needs a conversion girt does not perform yet.
    #[error(
        "unsupported conversion for {}: {attribute}",
        String::from_utf8_lossy(path)
    )]
    Unsupported {
        /// The path being converted.
        path: Vec<u8>,
        /// The attribute that requests it: `filter` or `working-tree-encoding`.
        attribute: &'static str,
    },
}

enum Rules {
    Lookup(Lookup),
    Attributes(Attributes),
}

/// The conversions of one repository: its attribute rules and [`Settings`].
///
/// [`Repository::filters`] loads a pipeline as Git would for that repository.
pub struct Pipeline {
    rules: Rules,
    settings: Settings,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pipeline")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl Pipeline {
    /// Uses attribute files read on demand by `lookup`.
    pub fn new(lookup: Lookup, settings: Settings) -> Self {
        Self {
            rules: Rules::Lookup(lookup),
            settings,
        }
    }

    /// Uses attribute files the caller has already read.
    pub fn from_attributes(attributes: Attributes, settings: Settings) -> Self {
        Self {
            rules: Rules::Attributes(attributes),
            settings,
        }
    }

    /// The configuration in use.
    pub fn settings(&self) -> Settings {
        self.settings
    }

    fn get(&mut self, path: &[u8], name: &[u8]) -> Result<State<'_>, Error> {
        match &mut self.rules {
            Rules::Lookup(lookup) => Ok(lookup.get(path, name)?),
            Rules::Attributes(attributes) => attributes
                .get(path, name)
                .map_err(|source| LoadError::Rules {
                    path: path.to_vec(),
                    source,
                })
                .map_err(Error::from),
        }
    }

    /// Resolves the conversions for `path`, a slash-separated repository-relative path.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Attributes`] when attributes cannot be read and [`Error::Unsupported`]
    /// when the path has a `filter` or `working-tree-encoding` attribute.
    pub fn plan(&mut self, path: &[u8]) -> Result<Plan, Error> {
        for attribute in ["filter", "working-tree-encoding"] {
            if matches!(self.get(path, attribute.as_bytes())?, State::Value(_)) {
                return Err(Error::Unsupported {
                    path: path.to_vec(),
                    attribute,
                });
            }
        }
        let eol = match self.get(path, b"eol")? {
            State::Value(b"lf") => Some(LineEnding::Lf),
            State::Value(b"crlf") => Some(LineEnding::Crlf),
            _ => None,
        };
        let autocrlf = self.settings.autocrlf;
        let text = match self.get(path, b"text")? {
            State::Set => Text::Always,
            State::Unset => Text::Binary,
            State::Value(b"auto") => Text::Auto,
            // Unspecified, or a value other than `auto`.
            _ if eol.is_some() => Text::Always,
            _ if autocrlf == AutoCrlf::False => Text::Binary,
            _ => Text::Auto,
        };
        let ident = self.get(path, b"ident")? == State::Set;
        let settings = self.settings;
        Ok(Plan {
            text,
            eol: eol.unwrap_or(match settings.autocrlf {
                AutoCrlf::True => LineEnding::Crlf,
                AutoCrlf::Input => LineEnding::Lf,
                AutoCrlf::False => settings.eol,
            }),
            ident,
            safecrlf: settings.safecrlf,
        })
    }

    /// Converts `blob`, a stored blob with ID `id`, to the bytes Git writes at `path`.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::plan`].
    pub fn to_worktree<'a>(
        &mut self,
        path: &[u8],
        blob: &'a [u8],
        id: ObjectId,
    ) -> Result<Cow<'a, [u8]>, Error> {
        Ok(self.plan(path)?.to_worktree(blob, id))
    }

    /// Converts `file`, the working file at `path`, to the content Git stores.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::plan`] and [`Plan::to_repository`].
    pub fn to_repository<'a>(
        &mut self,
        path: &[u8],
        file: &'a [u8],
        staged: Staged<'_>,
    ) -> Result<Stored<'a>, Error> {
        self.plan(path)?.to_repository(path, file, staged)
    }
}

impl Repository {
    /// Loads the conversions Git applies in this repository.
    ///
    /// Reads attribute files from the [`Repository::attribute_source`] Git would choose, through
    /// [`Repository::attributes`] with default limits, and [`Settings`] from [`Self::config`].
    ///
    /// # Errors
    ///
    /// Returns the [`LoadError`] of choosing the source or loading the files, and
    /// [`LoadError::Config`] for an invalid `core.autocrlf`, `core.eol` or `core.safecrlf`.
    pub fn filters(&self, environment: &Environment) -> Result<Pipeline, LoadError> {
        let source = self.attribute_source(environment)?;
        let lookup = self.attributes(environment, source)?;
        Ok(Pipeline::new(lookup, Settings::from_config(self.config())?))
    }
}
