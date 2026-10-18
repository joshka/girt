use std::collections::HashMap;

use super::{Error, FunctionLines};
use crate::Repository;
use crate::attributes::{LoadError, Lookup, State};
use crate::config::{Config, InvalidValue};
use crate::workflow::Environment;

/// The diff settings `.gitattributes` and `diff.<driver>.*` configuration give each path.
///
/// [`Repository::diff_attributes`] loads them as `git diff` does; pass the value to
/// [`Objects::write_patch_with`](crate::Objects::write_patch_with). For each side of a file, the
/// `diff` attribute chooses:
///
/// - `diff` set: compare as text even when the content contains NUL.
/// - `-diff` (including the `binary` macro): report `Binary files … differ`.
/// - `diff=<driver>`: `diff.<driver>.binary` true reports binary and false compares as text; unset
///   leaves NUL detection in charge. `diff.<driver>.xfuncname` (extended) or
///   `diff.<driver>.funcname` (basic), whichever is configured last, finds hunk-header function
///   lines; without either, [`FunctionLines::builtin`] supplies Git's built-in pattern for a driver
///   girt knows, and otherwise Git's default rule applies.
/// - unspecified: NUL detection and Git's default function rule.
///
/// A file is binary when either side is. Function lines come from the old side's driver, or the
/// new side's when the old side has no pattern, as Git chooses them. `text`, `eol` and other
/// attributes do not affect patches. girt never runs commands: a driver with
/// `diff.<driver>.textconv` or `diff.<driver>.command` is refused unless
/// [`Options::external`](super::Options::external) ignores those settings.
///
/// ```
/// use std::sync::atomic::AtomicBool;
///
/// use girt::patch::{Limits, Options};
/// use girt::tree::TreeEditor;
/// use girt::workflow::Environment;
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("w"),
///     InitKind::Worktree,
/// )?;
/// std::fs::write(
///     repo.worktree().unwrap().join(".gitattributes"),
///     "*.svg binary\n",
/// )?;
///
/// let loose = repo.loose_objects();
/// let mut editor = TreeEditor::new(&loose);
/// editor.set(b"logo.svg", loose.write_blob(b"<svg/>\n")?)?;
/// let old = editor.write()?;
/// editor.set(b"logo.svg", loose.write_blob(b"<svg></svg>\n")?)?;
/// let new = editor.write()?;
///
/// let objects = repo.objects()?;
/// let entries = objects.diff_trees(Some(old), Some(new))?.entries;
///
/// let mut attributes = repo.diff_attributes(&Environment::default())?;
/// let mut patch = Vec::new();
/// objects.write_patch_with(
///     &mut patch,
///     &entries,
///     Options::default(),
///     Some(&mut attributes),
///     Limits::default(),
///     &AtomicBool::new(false),
/// )?;
///
/// assert!(patch.ends_with(b"Binary files a/logo.svg and b/logo.svg differ\n"));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct DiffAttributes {
    lookup: Lookup,
    settings: HashMap<Vec<u8>, Settings>,
    functions: HashMap<Vec<u8>, Option<FunctionLines>>,
    /// The most recent path and its driver: both sides of a modification share a path.
    last: Option<(Vec<u8>, Driver)>,
}

/// The `diff.<driver>.*` values girt interprets for one driver.
#[derive(Clone, Debug, Default)]
struct Settings {
    binary: Option<bool>,
    /// The last of `xfuncname` (extended) and `funcname` (basic).
    function: Option<(Vec<u8>, bool)>,
    textconv: bool,
    command: bool,
}

/// How one side of a file is classified.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Content {
    /// Binary when the content contains NUL.
    #[default]
    Auto,
    /// Always text.
    Text,
    /// Always binary.
    Binary,
}

/// The settings for one side of one file.
#[derive(Clone, Debug, Default)]
pub(super) struct Driver {
    pub(super) name: Option<Vec<u8>>,
    pub(super) content: Content,
    pub(super) function: Option<FunctionLines>,
    /// `textconv` or `command`, when the driver configures one.
    pub(super) external: Option<&'static str>,
}

impl DiffAttributes {
    /// Combines attribute files with the `diff.<driver>.*` entries of `config`.
    ///
    /// Patterns are compiled the first time a path uses their driver.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] for a non-boolean `diff.<driver>.binary` or a pattern, textconv or
    /// command entry without a value.
    pub fn new(lookup: Lookup, config: &Config) -> Result<Self, InvalidValue> {
        let mut settings: HashMap<Vec<u8>, Settings> = HashMap::new();
        for entry in config.entries() {
            let Some(driver) = entry
                .subsection
                .as_ref()
                .filter(|_| entry.section.eq_ignore_ascii_case(b"diff"))
            else {
                continue;
            };
            let value = entry.value.as_deref();
            let name = entry.name.to_ascii_lowercase();
            let invalid = |name: &str| InvalidValue::new("diff", name, value);
            let driver = settings.entry(driver.clone()).or_default();
            match name.as_slice() {
                b"binary" => {
                    driver.binary = Some(crate::config::boolean(value).ok_or(invalid("binary"))?);
                }
                b"xfuncname" => {
                    driver.function = Some((value.ok_or(invalid("xfuncname"))?.to_vec(), true));
                }
                b"funcname" => {
                    driver.function = Some((value.ok_or(invalid("funcname"))?.to_vec(), false));
                }
                b"textconv" => driver.textconv = !value.ok_or(invalid("textconv"))?.is_empty(),
                b"command" => driver.command = !value.ok_or(invalid("command"))?.is_empty(),
                _ => {}
            }
        }
        Ok(Self {
            lookup,
            settings,
            functions: HashMap::new(),
            last: None,
        })
    }

    /// Resolves the driver of the file at `path`.
    pub(super) fn driver(&mut self, path: &[u8]) -> Result<Driver, Error> {
        if let Some((last, driver)) = &self.last
            && last == path
        {
            return Ok(driver.clone());
        }
        let driver = self.resolve(path)?;
        self.last = Some((path.to_vec(), driver.clone()));
        Ok(driver)
    }

    fn resolve(&mut self, path: &[u8]) -> Result<Driver, Error> {
        let attributes = |source| Error::Attributes {
            path: path.to_vec(),
            source: Box::new(source),
        };
        let name = match self.lookup.get(path, b"diff").map_err(attributes)? {
            State::Unspecified => return Ok(Driver::default()),
            State::Set => {
                return Ok(Driver {
                    content: Content::Text,
                    ..Driver::default()
                });
            }
            State::Unset => {
                return Ok(Driver {
                    content: Content::Binary,
                    ..Driver::default()
                });
            }
            State::Value(name) => name.to_vec(),
        };
        let settings = self.settings.get(&name).cloned().unwrap_or_default();
        let function = match self.functions.get(&name) {
            Some(function) => function.clone(),
            None => {
                let function = match &settings.function {
                    Some((pattern, true)) => Some(FunctionLines::extended(pattern)),
                    Some((pattern, false)) => Some(FunctionLines::basic(pattern)),
                    None => FunctionLines::builtin(&name).map(Ok),
                }
                .transpose()
                .map_err(|source| Error::Function {
                    driver: name.clone(),
                    source,
                })?;
                self.functions.insert(name.clone(), function.clone());
                function
            }
        };
        Ok(Driver {
            content: match settings.binary {
                Some(true) => Content::Binary,
                Some(false) => Content::Text,
                None => Content::Auto,
            },
            function,
            external: if settings.textconv {
                Some("textconv")
            } else if settings.command {
                Some("command")
            } else {
                None
            },
            name: Some(name),
        })
    }
}

impl DiffAttributes {
    /// How `path`'s driver classifies content (`Some(true)` binary, `Some(false)` text, `None`
    /// by NUL detection) and its function-line pattern, for `git grep`.
    pub(crate) fn grep_driver(
        &mut self,
        path: &[u8],
    ) -> Result<(Option<bool>, Option<FunctionLines>), Error> {
        let driver = self.driver(path)?;
        let binary = match driver.content {
            Content::Auto => None,
            Content::Text => Some(false),
            Content::Binary => Some(true),
        };
        Ok((binary, driver.function))
    }

    /// The driver name of `path` when that driver configures textconv, for blame's refusal.
    pub(crate) fn textconv_driver(&mut self, path: &[u8]) -> Result<Option<Vec<u8>>, Error> {
        let driver = self.driver(path)?;
        Ok((driver.external == Some("textconv")).then(|| driver.name.unwrap_or_default()))
    }
}

impl Repository {
    /// Loads the diff attributes `git diff` applies in this repository.
    ///
    /// Reads attribute files from the [`Repository::attribute_source`] Git would choose, through
    /// [`Repository::attributes`] with default limits, and `diff.<driver>.*` from
    /// [`Self::config`]. Open the repository with
    /// [`ConfigInputs::git_default`](crate::config::ConfigInputs::git_default) to see drivers from
    /// the user's global configuration.
    ///
    /// # Errors
    ///
    /// Returns the [`LoadError`] of choosing the source or loading the files, and
    /// [`LoadError::Config`] for an invalid `diff.<driver>.*` value.
    pub fn diff_attributes(&self, environment: &Environment) -> Result<DiffAttributes, LoadError> {
        let source = self.attribute_source(environment)?;
        let lookup = self.attributes(environment, source)?;
        Ok(DiffAttributes::new(lookup, self.config())?)
    }
}
