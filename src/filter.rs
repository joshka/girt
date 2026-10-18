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
//! - **Drivers.** `filter=<name>` runs `filter.<name>.clean` when storing and `smudge` when
//!   writing, or sends the file to the long-running `filter.<name>.process` (protocol version 2,
//!   without the `delay` capability), which Git LFS installs. Running commands is a trust decision
//!   the caller makes with [`Commands`]: the default, [`Commands::Refuse`], returns
//!   [`Error::Refused`] instead of running one. A failing driver leaves content unchanged and is
//!   reported in [`Converted::skipped`], unless `filter.<name>.required` makes it an
//!   [`Error::Driver`].
//!
//! - **`working-tree-encoding`.** Working files in UTF-16 or UTF-32 (`UTF-16`, `UTF-16LE`,
//!   `UTF-16BE`, `UTF-16LE-BOM`, `UTF-32`, `UTF-32LE`, `UTF-32BE`, ignoring case) are stored as
//!   UTF-8, between the driver and line-ending steps. Other encodings, which Git converts with
//!   `iconv`, are refused with [`Error::Unsupported`] rather than converted partially.
//!
//! ```
//! use std::borrow::Cow;
//!
//! use girt::ObjectId;
//! use girt::attributes::{Attributes, Case, Limits, Source};
//! use girt::filter::{Drivers, Pipeline, Settings, Staged};
//!
//! let mut attributes = Attributes::new(Case::Sensitive, Limits::default());
//! attributes.add(Source::Directory(b""), b"*.bat text eol=crlf\n")?;
//! let mut filters =
//!     Pipeline::from_attributes(attributes, Settings::default(), Drivers::default());
//!
//! let stored = filters.to_repository(b"run.bat", b"echo\r\n", Staged::Absent)?;
//! assert_eq!(&*stored.bytes, b"echo\n");
//! let written = filters.to_worktree(
//!     b"run.bat",
//!     b"echo\n",
//!     ObjectId::null(girt::ObjectFormat::Sha1),
//! )?;
//! assert_eq!(&*written.bytes, b"echo\r\n");
//! assert!(matches!(written.bytes, Cow::Owned(_)));
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

use std::borrow::Cow;
use std::collections::HashMap;

use crate::attributes::{Attributes, LoadError, Lookup, State};
use crate::config::{Config, InvalidValue};
use crate::workflow::Environment;
use crate::{ObjectId, Objects, ReadBlobError, Repository};

mod driver;
mod encoding;
mod eol;
mod ident;
mod process;

pub use driver::{Commands, Direction, DriverError, Drivers, Limits};
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    text: Text,
    eol: LineEnding,
    ident: bool,
    safecrlf: SafeCrlf,
    driver: Option<Vec<u8>>,
    encoding: Option<(Vec<u8>, encoding::Encoding)>,
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

    /// The `working-tree-encoding` attribute's value, when it names an encoding other than
    /// UTF-8.
    pub fn encoding(&self) -> Option<&[u8]> {
        self.encoding.as_ref().map(|(name, _)| name.as_slice())
    }

    /// Whether the `ident` attribute is set.
    pub fn ident(&self) -> bool {
        self.ident
    }

    /// The `filter` attribute's driver name, when set.
    pub fn driver(&self) -> Option<&[u8]> {
        self.driver.as_deref()
    }

    fn builtin_to_worktree<'a>(&self, bytes: Cow<'a, [u8]>, id: ObjectId) -> Cow<'a, [u8]> {
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

    /// Decodes `bytes` from the path's `working-tree-encoding`, if any.
    fn decode<'a>(&self, path: &[u8], bytes: Cow<'a, [u8]>) -> Result<Cow<'a, [u8]>, Error> {
        let Some((name, encoding)) = &self.encoding else {
            return Ok(bytes);
        };
        encoding
            .decode(&bytes)
            .map(Cow::Owned)
            .map_err(|failure| Error::Encoding {
                path: path.to_vec(),
                encoding: name.clone(),
                reason: match failure {
                    encoding::Failure::BomRequired => "BOM is required",
                    encoding::Failure::BomProhibited => "BOM is prohibited",
                    encoding::Failure::Invalid => "invalid content",
                },
            })
    }

    fn builtin_to_repository<'a>(
        &self,
        path: &[u8],
        file: Cow<'a, [u8]>,
        staged: Staged<'_>,
    ) -> Result<(Cow<'a, [u8]>, Option<CrlfWarning>), Error> {
        let stats = Stats::new(&file);
        let convert = stats.crlf > 0
            && match self.text {
                Text::Binary => false,
                Text::Always => true,
                Text::Auto => !stats.binary() && !staged.has_crlf()?,
            };
        let converted = convert.then(|| eol::crlf_to_lf(&file));
        let warning = if self.text == Text::Binary {
            None
        } else {
            let stored = converted.as_deref().unwrap_or(&file);
            let written = self.eol_to_worktree(Cow::Borrowed(stored));
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
        let bytes = converted.map_or(file, Cow::Owned);
        let bytes = if self.ident {
            ident::collapse(bytes)
        } else {
            bytes
        };
        Ok((bytes, warning))
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

/// Converted content, from [`Pipeline::to_repository`] or [`Pipeline::to_worktree`].
#[derive(Debug)]
#[non_exhaustive]
pub struct Converted<'a> {
    /// The converted bytes; borrowed from the input when nothing changed.
    pub bytes: Cow<'a, [u8]>,
    /// Set when storing under [`SafeCrlf::Warn`] and writing the stored bytes back would change
    /// the file.
    pub warning: Option<CrlfWarning>,
    /// A driver that is not `filter.<name>.required` failed, and its conversion was skipped as
    /// Git skips it. Git reports this as an error message and continues.
    pub skipped: Option<DriverError>,
    /// The stored bytes were not valid UTF-8, so they were written without
    /// `working-tree-encoding`, as Git writes them after reporting an error.
    pub unencoded: bool,
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
    /// A driver would run a command under [`Commands::Refuse`].
    #[error(
        "{direction} filter {:?} for {} needs commands to run",
        String::from_utf8_lossy(driver),
        String::from_utf8_lossy(path)
    )]
    Refused {
        /// The path being converted.
        path: Vec<u8>,
        /// The driver name.
        driver: Vec<u8>,
        /// The direction that would run.
        direction: Direction,
    },
    /// A `filter.<name>.required` driver failed or has no command for the direction.
    #[error(
        "{} filter failed for {}",
        source_direction(source),
        String::from_utf8_lossy(path)
    )]
    Driver {
        /// The path being converted.
        path: Vec<u8>,
        /// What failed.
        #[source]
        source: DriverError,
    },
    /// Reading the staged blob for [`Text::Auto`] failed.
    #[error("reading the staged blob")]
    Staged(#[source] Box<ReadBlobError>),
    /// A working file is not valid in its `working-tree-encoding`, or lacks or has a byte order
    /// mark the encoding requires or forbids. Git refuses to add such a file.
    #[error(
        "{} in {} as {}",
        reason,
        String::from_utf8_lossy(path),
        String::from_utf8_lossy(encoding)
    )]
    Encoding {
        /// The path being stored.
        path: Vec<u8>,
        /// The `working-tree-encoding` value.
        encoding: Vec<u8>,
        /// What is wrong: `"BOM is required"`, `"BOM is prohibited"` or `"invalid content"`.
        reason: &'static str,
    },
    /// The path needs a conversion girt does not perform yet.
    #[error(
        "unsupported conversion for {}: {attribute}",
        String::from_utf8_lossy(path)
    )]
    Unsupported {
        /// The path being converted.
        path: Vec<u8>,
        /// The attribute that requests it, such as `working-tree-encoding`.
        attribute: &'static str,
    },
}

enum Rules {
    Lookup(Lookup),
    Attributes(Attributes),
}

fn source_direction(error: &DriverError) -> Direction {
    match error {
        DriverError::Io { direction, .. }
        | DriverError::Failed { direction, .. }
        | DriverError::TooLarge { direction, .. }
        | DriverError::Rejected { direction, .. }
        | DriverError::Aborted { direction, .. }
        | DriverError::Missing { direction, .. } => *direction,
    }
}

/// The conversions of one repository: its attribute rules, [`Settings`] and [`Drivers`].
///
/// [`Repository::filters`] loads a pipeline as Git would for that repository.
pub struct Pipeline {
    rules: Rules,
    settings: Settings,
    drivers: Drivers,
    /// Started filter processes by driver name; `None` after a process failed to start.
    processes: HashMap<Vec<u8>, process::Process>,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Pipeline")
            .field("settings", &self.settings)
            .field("drivers", &self.drivers)
            .finish_non_exhaustive()
    }
}

impl Pipeline {
    // Set only on a freshly loaded child pipeline, before a process filter can start.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn set_command_environment(&mut self, environment: &Environment) {
        self.drivers.environment = Some(environment.clone());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn environment(&self) -> Option<&Environment> {
        match &self.rules {
            Rules::Lookup(lookup) => Some(lookup.environment()),
            Rules::Attributes(_) => None,
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn driver_policy(&self) -> (Commands, Limits) {
        (self.drivers.commands, self.drivers.limits)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(crate) fn set_driver_limits(&mut self, limits: Limits) {
        self.drivers.limits = limits;
    }

    /// Uses attribute files read on demand by `lookup`.
    pub fn new(lookup: Lookup, settings: Settings, drivers: Drivers) -> Self {
        Self {
            rules: Rules::Lookup(lookup),
            settings,
            drivers,
            processes: HashMap::new(),
        }
    }

    /// Uses attribute files the caller has already read.
    pub fn from_attributes(attributes: Attributes, settings: Settings, drivers: Drivers) -> Self {
        Self {
            rules: Rules::Attributes(attributes),
            settings,
            drivers,
            processes: HashMap::new(),
        }
    }

    /// The configuration in use.
    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Replaces the configuration, for example to hash content without `core.safecrlf` as
    /// `git hash-object` does when it does not write the object.
    #[must_use]
    pub fn with_settings(mut self, settings: Settings) -> Self {
        self.settings = settings;
        self
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
    /// when the path's `working-tree-encoding` is not in the UTF-16 or UTF-32 families.
    pub fn plan(&mut self, path: &[u8]) -> Result<Plan, Error> {
        let encoding = match self.get(path, b"working-tree-encoding")? {
            State::Value(name) => {
                let name = name.to_vec();
                match encoding::Encoding::parse(&name) {
                    Ok(Some(encoding)) => Some((name, encoding)),
                    Ok(None) => None,
                    Err(()) => {
                        return Err(Error::Unsupported {
                            path: path.to_vec(),
                            attribute: "working-tree-encoding",
                        });
                    }
                }
            }
            _ => None,
        };
        let driver = match self.get(path, b"filter")? {
            State::Value(name) => Some(name.to_vec()),
            _ => None,
        };
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
            driver,
            encoding,
        })
    }

    /// Converts `blob`, a stored blob with ID `id`, to the bytes Git writes at `path`.
    ///
    /// Applies `ident`, then line endings, then `working-tree-encoding`, then the `filter`
    /// driver's smudge command.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::plan`], [`Error::Refused`] when a smudge command would run
    /// under [`Commands::Refuse`], and [`Error::Driver`] when a required driver fails.
    pub fn to_worktree<'a>(
        &mut self,
        path: &[u8],
        blob: &'a [u8],
        id: ObjectId,
    ) -> Result<Converted<'a>, Error> {
        let plan = self.plan(path)?;
        let bytes = plan.builtin_to_worktree(Cow::Borrowed(blob), id);
        let (bytes, unencoded) = match &plan.encoding {
            Some((_, encoding)) => match encoding.encode(&bytes) {
                Ok(encoded) => (Cow::Owned(encoded), false),
                Err(_) => (bytes, true),
            },
            None => (bytes, false),
        };
        let (bytes, skipped) = self.drive(&plan, path, Direction::Smudge, Some(id), bytes)?;
        Ok(Converted {
            bytes,
            warning: None,
            skipped,
            unencoded,
        })
    }

    /// Converts `file`, the working file at `path`, to the content Git stores.
    ///
    /// Applies the `filter` driver's clean command, then `working-tree-encoding`, then line
    /// endings, then `ident`. `staged`
    /// supplies what [`Text::Auto`] consults: the path's currently staged blob.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::plan`], [`Error::Refused`] when a clean command would run
    /// under [`Commands::Refuse`], [`Error::Driver`] when a required driver fails,
    /// [`Error::UnsafeCrlf`] under [`SafeCrlf::Refuse`] when the result would not be written back
    /// as the file, [`Error::Encoding`] when the file is not valid in its
    /// `working-tree-encoding`, and [`Error::Staged`] when reading the staged blob fails.
    pub fn to_repository<'a>(
        &mut self,
        path: &[u8],
        file: &'a [u8],
        staged: Staged<'_>,
    ) -> Result<Converted<'a>, Error> {
        let plan = self.plan(path)?;
        let (bytes, skipped) =
            self.drive(&plan, path, Direction::Clean, None, Cow::Borrowed(file))?;
        let bytes = plan.decode(path, bytes)?;
        let (bytes, warning) = plan.builtin_to_repository(path, bytes, staged)?;
        Ok(Converted {
            bytes,
            warning,
            skipped,
            unencoded: false,
        })
    }

    /// Runs the path's driver for `direction`, if it has a command.
    fn drive<'a>(
        &mut self,
        plan: &Plan,
        path: &[u8],
        direction: Direction,
        blob: Option<ObjectId>,
        bytes: Cow<'a, [u8]>,
    ) -> Result<(Cow<'a, [u8]>, Option<DriverError>), Error> {
        let Some(name) = plan.driver() else {
            return Ok((bytes, None));
        };
        let Some(driver) = self.drivers.get(name) else {
            return Ok((bytes, None));
        };
        let required = driver.required;
        let failure = |source| Error::Driver {
            path: path.to_vec(),
            source,
        };
        let process = driver.process.clone();
        let command = driver.command(direction).map(<[u8]>::to_vec);
        if process.is_none() && command.is_none() {
            if required {
                return Err(failure(DriverError::Missing {
                    driver: name.to_vec(),
                    direction,
                }));
            }
            return Ok((bytes, None));
        }
        if self.drivers.commands == Commands::Refuse {
            return Err(Error::Refused {
                path: path.to_vec(),
                driver: name.to_vec(),
                direction,
            });
        }
        let result = match (process, command) {
            (Some(process), _) => self.request(name, &process, direction, path, blob, &bytes),
            (None, Some(command)) => {
                driver::run(name, direction, &command, path, &bytes, &self.drivers).map(Some)
            }
            (None, None) => Ok(None),
        };
        match result {
            Ok(Some(output)) => Ok((Cow::Owned(output), None)),
            Ok(None) if required => Err(failure(DriverError::Missing {
                driver: name.to_vec(),
                direction,
            })),
            Ok(None) => Ok((bytes, None)),
            Err(error) if required => Err(failure(error)),
            Err(error) => Ok((bytes, Some(error))),
        }
    }

    /// Sends one file to the driver's long-running process, starting it on first use. `None`
    /// means the process lacks the capability.
    fn request(
        &mut self,
        name: &[u8],
        command: &[u8],
        direction: Direction,
        path: &[u8],
        blob: Option<ObjectId>,
        content: &[u8],
    ) -> Result<Option<Vec<u8>>, DriverError> {
        let io = |source| DriverError::Io {
            driver: name.to_vec(),
            direction,
            source,
        };
        if !self.processes.contains_key(name) {
            let process = process::Process::start(command, &self.drivers).map_err(io)?;
            self.processes.insert(name.to_vec(), process);
        }
        let Some(process) = self.processes.get_mut(name) else {
            return Err(io(std::io::Error::other("filter process missing")));
        };
        if !process.supports(direction) {
            return Ok(None);
        }
        let limit = self.drivers.limits.max_output_bytes();
        match process.request(direction, path, blob, content, limit) {
            Ok(process::Reply::Converted(output)) => Ok(Some(output)),
            Ok(process::Reply::Error) => Err(DriverError::Rejected {
                driver: name.to_vec(),
                direction,
            }),
            Ok(process::Reply::Abort) => {
                process.disable(direction);
                Err(DriverError::Aborted {
                    driver: name.to_vec(),
                    direction,
                })
            }
            Err(error) => {
                // The stream is out of step; a later request starts a new process.
                self.processes.remove(name);
                Err(error.into_driver_error(name, direction))
            }
        }
    }
}

// Only the macOS/Linux status walk converts files for comparison.
#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Pipeline {
    /// [`Self::to_repository`] without `core.safecrlf`, as Git compares files for status.
    pub(crate) fn stored_for_status<'a>(
        &mut self,
        path: &[u8],
        file: &'a [u8],
        staged: Staged<'_>,
    ) -> Result<Cow<'a, [u8]>, Error> {
        let mut plan = self.plan(path)?;
        plan.safecrlf = SafeCrlf::Allow;
        let (bytes, _) = self.drive(&plan, path, Direction::Clean, None, Cow::Borrowed(file))?;
        let bytes = plan.decode(path, bytes)?;
        Ok(plan.builtin_to_repository(path, bytes, staged)?.0)
    }
}

impl Repository {
    /// Loads the conversions Git applies in this repository.
    ///
    /// Reads attribute files from the [`Repository::attribute_source`] Git would choose, through
    /// [`Repository::attributes`] with default limits, and [`Settings`] and [`Drivers`] from
    /// [`Self::config`]. `commands` decides whether driver commands may run; they run in the
    /// working tree root. Open the repository with
    /// [`ConfigInputs::git_default`](crate::config::ConfigInputs::git_default) to see drivers from
    /// the user's global configuration, such as Git LFS installs.
    ///
    /// # Errors
    ///
    /// Returns the [`LoadError`] of choosing the source or loading the files, and
    /// [`LoadError::Config`] for an invalid `core.autocrlf`, `core.eol`, `core.safecrlf` or
    /// `filter.<name>.required`.
    pub fn filters(
        &self,
        environment: &Environment,
        commands: Commands,
    ) -> Result<Pipeline, LoadError> {
        let source = self.attribute_source(environment)?;
        self.filters_with_source(environment, source, commands)
    }

    /// Loads the conversions for writing `target` during a checkout, as Git chooses them.
    ///
    /// `GIT_ATTR_SOURCE` or `attr.tree` wins when set, as in [`Self::filters`]. Otherwise
    /// attributes come from `target` itself rather than the working tree, because Git reads them
    /// from the index it is checking out and the target's `.gitattributes` files are not written
    /// yet. Pass the result as `target_filters` to [`Self::checkout_tree_with_filters`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::filters`].
    pub fn filters_for_checkout(
        &self,
        environment: &Environment,
        target: ObjectId,
        commands: Commands,
    ) -> Result<Pipeline, LoadError> {
        let source = match self.attribute_source(environment)? {
            crate::attributes::DirectorySource::Worktree => {
                crate::attributes::DirectorySource::Tree(target)
            }
            chosen => chosen,
        };
        self.filters_with_source(environment, source, commands)
    }

    /// Loads conversions as [`Self::filters`] does, reading per-directory `.gitattributes` from
    /// `source`.
    ///
    /// Use [`DirectorySource::Tree`](crate::attributes::DirectorySource::Tree) with the target
    /// tree for a checkout into an empty working tree, where the target's `.gitattributes` files
    /// are not written yet; Git reads them from the index it is checking out.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::filters`].
    pub fn filters_with_source(
        &self,
        environment: &Environment,
        source: crate::attributes::DirectorySource,
        commands: Commands,
    ) -> Result<Pipeline, LoadError> {
        let lookup = self.attributes(environment, source)?;
        let mut drivers = Drivers::from_config(self.config())?.with_commands(commands);
        if let Some(worktree) = self.worktree() {
            drivers = drivers.with_directory(worktree);
        }
        Ok(Pipeline::new(
            lookup,
            Settings::from_config(self.config())?,
            drivers,
        ))
    }
}
