//! Submodule definitions from caller-selected `.gitmodules` bytes.
//!
//! A definition's logical name selects configuration and absorbed repository storage; its path
//! selects a gitlink in the parent tree or index. They need not be equal. [`Modules::parse`] reads
//! one byte source without following includes, opening repositories, fetching, or running commands.
//! [`Modules::read`] loads a caller-selected [`Source`]: worktree, index, commit, or the explicit
//! worktree-then-index fallback used by Git init. No loading operation reads ambient configuration.
//!
//! Parsing preserves names and paths for inspection. Call [`Definition::validate`] before using
//! either in a filesystem operation. Validation is lexical; it does not establish that existing
//! directories are free of symlinks or that a concurrent writer cannot replace them. This module
//! computes initialization/update plans without changing a checkout. [`status()`] inspects existing
//! checkouts; configured parent status includes these observations in [`crate::status::Report`].
//! [`Definition::plan_init`] proposes local config assignments; [`Definition::plan_update`] selects
//! a recorded commit and update method. [`InitPlan::apply`] publishes config after checking inputs;
//! [`UpdatePlan::apply`] performs filtered checkout/none updates in existing children. Fetch, clone
//! are available through explicit [`UpdatePlan::populate`], including absorbed metadata layout.
//! Recursion remains a separate operation. External
//! update commands are always refused in `.gitmodules`; user configuration additionally requires
//! resolved provenance and separate [`Commands`] permission.
//!
//! ```
//! use girt::submodule::{Modules, Update};
//!
//! let modules = Modules::parse(b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\n")?;
//! let math = modules.get(b"math").ok_or("definition missing")?;
//! math.validate()?;
//! assert_eq!(math.path(), Some(b"vendor/math".as_slice()));
//! assert_eq!(math.url(), Some(b"../math.git".as_slice()));
//! assert_eq!(math.update(), Update::Checkout);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod execute;
mod load;
mod plan;
mod populate;
pub use populate::{PopulateError, PopulateFailure, PopulateReport};
mod status;
mod url;
pub use execute::{InitError, UpdateError, UpdateOptions, UpdateReport};
pub use load::{LoadError, Source};
pub use plan::{Commands, InitPlan, PlanError, UpdateMethod, UpdatePlan};
pub use status::{Checkout, StatusError, status, status_with};
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) use status::{complete_status, ignored_gitlinks};
pub use url::UrlError;

use crate::config::{Config, ConfigError};

/// Owned definitions, ordered by logical name bytes.
///
/// Repeated scalar assignments use the last occurrence, as Git's worktree `.gitmodules` reader
/// does. Unknown settings and unrelated sections are ignored. Includes are never expanded.
#[derive(Clone, Debug)]
pub struct Modules {
    definitions: Vec<Definition>,
}

impl Modules {
    /// Parses configuration syntax and interprets recognized submodule settings.
    ///
    /// Names, paths and URLs remain byte strings. Missing path/URL and unsafe filesystem names
    /// remain inspectable until [`Definition::validate`]. An invalid `ignore` value is retained as
    /// [`Ignore::Unspecified`], matching Git's fallback (without emitting Git's warning).
    ///
    /// # Errors
    ///
    /// Rejects malformed configuration, invalid booleans, unknown update/fetch-recursion values,
    /// and every external `!command` update method. Commands are forbidden in `.gitmodules` even
    /// when an application trusts its local configuration.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let config = Config::parse(bytes)?;
        validate_assignments(&config)?;
        let definitions = config
            .subsection_names("submodule")
            .into_iter()
            .map(|name| Definition::from_config(&config, name))
            .collect::<Result<_, _>>()?;
        Ok(Self { definitions })
    }

    /// Borrows definitions in logical-name order, including incomplete definitions.
    pub fn definitions(&self) -> &[Definition] {
        &self.definitions
    }

    /// Looks up a case-sensitive logical name, independently of its worktree path.
    pub fn get(&self, name: &[u8]) -> Option<&Definition> {
        self.definitions.iter().find(|entry| entry.name == name)
    }
}

/// One interpreted definition; parsing alone does not authorize filesystem use.
#[derive(Clone, Debug)]
pub struct Definition {
    name: Vec<u8>,
    path: Option<Vec<u8>>,
    url: Option<Vec<u8>>,
    branch: Option<Vec<u8>>,
    update: Update,
    update_explicit: bool,
    ignore: Ignore,
    shallow: Option<bool>,
    fetch_recurse: Option<FetchRecurse>,
}

impl Definition {
    fn from_config(config: &Config, name: &[u8]) -> Result<Self, Error> {
        let value = |key| config.string("submodule", Some(name), key);
        let update = match value("update") {
            None | Some(b"checkout") => Update::Checkout,
            Some(b"rebase") => Update::Rebase,
            Some(b"merge") => Update::Merge,
            Some(b"none") => Update::None,
            Some(_) => {
                return Err(Error::InvalidSetting {
                    name: name.to_vec(),
                    key: "update",
                });
            }
        };
        let ignore = match value("ignore") {
            Some(b"none") => Ignore::None,
            Some(b"untracked") => Ignore::Untracked,
            Some(b"dirty") => Ignore::Dirty,
            Some(b"all") => Ignore::All,
            _ => Ignore::Unspecified,
        };
        let fetch_recurse = match value("fetchRecurseSubmodules") {
            Some(b"on-demand") => Some(FetchRecurse::OnDemand),
            _ => config
                .boolean("submodule", Some(name), "fetchRecurseSubmodules")
                .map_err(|_| invalid_setting(name, "fetchRecurseSubmodules"))?
                .map(|enabled| {
                    if enabled {
                        FetchRecurse::Always
                    } else {
                        FetchRecurse::Never
                    }
                }),
        };
        Ok(Self {
            name: name.to_vec(),
            path: value("path").map(<[u8]>::to_vec),
            url: value("url").map(<[u8]>::to_vec),
            branch: value("branch").map(<[u8]>::to_vec),
            update,
            update_explicit: value("update").is_some(),
            ignore,
            shallow: config
                .boolean("submodule", Some(name), "shallow")
                .map_err(|_| invalid_setting(name, "shallow"))?,
            fetch_recurse,
        })
    }

    /// Logical, case-sensitive name used in `submodule.<name>` configuration.
    pub fn name(&self) -> &[u8] {
        &self.name
    }
    /// Worktree-relative gitlink path, if supplied; no normalization is performed.
    pub fn path(&self) -> Option<&[u8]> {
        self.path.as_deref()
    }
    /// Fetch URL bytes, if supplied; relative URLs remain unresolved.
    pub fn url(&self) -> Option<&[u8]> {
        self.url.as_deref()
    }
    /// Remote branch, with `.` meaning the superproject's current branch; absence means remote
    /// HEAD.
    pub fn branch(&self) -> Option<&[u8]> {
        self.branch.as_deref()
    }
    /// Update strategy, defaulting to checking out the recorded commit detached.
    pub fn update(&self) -> Update {
        self.update
    }
    /// Dirtiness visibility preference before local configuration overrides.
    pub fn ignore(&self) -> Ignore {
        self.ignore
    }
    /// Initial shallow-clone recommendation; absence leaves the caller's default in effect.
    pub fn shallow(&self) -> Option<bool> {
        self.shallow
    }
    /// Fetch recursion preference; absence leaves the caller's default in effect.
    pub fn fetch_recurse(&self) -> Option<FetchRecurse> {
        self.fetch_recurse
    }

    /// Checks lexical prerequisites for using this definition operationally.
    ///
    /// Names and paths must be nonempty relative slash-separated paths with no empty, `.` or `..`
    /// component, backslash, colon, control byte, leading dash, or case-insensitive `.git`
    /// component. These portable restrictions deliberately exceed Git init's checks: for example,
    /// Git init can register an absolute logical name, which is unsafe to append to modules
    /// storage. Aliases such as `.git.`, `git~1` and `.git` followed by a space are not detected.
    /// URLs must be nonempty and cannot begin with a dash or contain control bytes. No
    /// URL scheme, transport permission, symlink containment, path alias, or network access is
    /// authorized here.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsafeName`], [`Error::UnsafePath`] or [`Error::UnsafeUrl`] for missing or
    /// rejected values. Do not use unvalidated names/paths to construct mutation destinations.
    pub fn validate(&self) -> Result<(), Error> {
        if !safe_relative_path(&self.name) {
            return Err(Error::UnsafeName);
        }
        if !self.path.as_deref().is_some_and(safe_relative_path) {
            return Err(Error::UnsafePath);
        }
        let url = self.url.as_deref().ok_or(Error::UnsafeUrl)?;
        if url.is_empty() || url.starts_with(b"-") || url.iter().any(u8::is_ascii_control) {
            return Err(Error::UnsafeUrl);
        }
        Ok(())
    }
}

// Validate every occurrence before scalar lookup: a later assignment must not hide an invalid
// earlier command or implicit string value from the repository's untrusted metadata.
fn validate_assignments(config: &Config) -> Result<(), Error> {
    for entry in config.entries() {
        if !entry.section.eq_ignore_ascii_case(b"submodule") {
            continue;
        }
        let Some(name) = entry.subsection.as_deref() else {
            continue;
        };
        let keys = [
            "path",
            "url",
            "branch",
            "update",
            "ignore",
            "shallow",
            "fetchRecurseSubmodules",
        ];
        let key = keys
            .into_iter()
            .find(|key| entry.name.eq_ignore_ascii_case(key.as_bytes()));
        let Some(key) = key else {
            continue;
        };
        let value = entry.value.as_deref();
        let valid = match key {
            "shallow" => crate::config::boolean(value).is_some(),
            "fetchRecurseSubmodules" => {
                value == Some(b"on-demand") || crate::config::boolean(value).is_some()
            }
            "update" => matches!(value, Some(b"checkout" | b"rebase" | b"merge" | b"none")),
            _ => value.is_some(),
        };
        if !valid {
            return Err(invalid_setting(name, key));
        }
    }
    Ok(())
}

fn invalid_setting(name: &[u8], key: &'static str) -> Error {
    Error::InvalidSetting {
        name: name.to_vec(),
        key,
    }
}

fn safe_relative_path(path: &[u8]) -> bool {
    !path.is_empty()
        && !path.starts_with(b"-")
        && !path
            .iter()
            .any(|byte| byte.is_ascii_control() || matches!(byte, b'\\' | b':'))
        && path.split(|byte| *byte == b'/').all(|component| {
            !component.is_empty()
                && component != b"."
                && component != b".."
                && !component.eq_ignore_ascii_case(b".git")
        })
}

/// Built-in update method; external commands are never accepted from `.gitmodules`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Update {
    /// Check out the recorded commit with a detached HEAD (the default).
    Checkout,
    /// Rebase the current submodule branch onto the recorded commit.
    Rebase,
    /// Merge the recorded commit into the current submodule branch.
    Merge,
    /// Leave the submodule unchanged.
    None,
}

/// Which nested changes parent status should ignore.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Ignore {
    /// No valid setting supplied; use the enclosing operation's default.
    Unspecified,
    /// Inspect new commits, tracked changes and untracked content.
    None,
    /// Ignore untracked content only.
    Untracked,
    /// Ignore working-tree changes, but inspect commit identity.
    Dirty,
    /// Ignore every submodule change.
    All,
}

/// Configured fetch recursion, interpreted without performing any fetch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum FetchRecurse {
    /// Fetch populated submodules whenever the parent is fetched.
    Always,
    /// Fetch only when fetched parent commits require missing submodule commits.
    OnDemand,
    /// Do not fetch submodules.
    Never,
}

/// Syntax, interpretation or lexical validation failure; no mutation has occurred.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Invalid Git configuration syntax.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A recognized setting is missing its value, invalid, or supplies an external command.
    #[error("invalid submodule {:?} setting {key}", String::from_utf8_lossy(name))]
    InvalidSetting {
        /// Logical submodule name bytes.
        name: Vec<u8>,
        /// Rejected configuration key.
        key: &'static str,
    },
    /// Logical name cannot safely select portable absorbed storage.
    #[error("unsafe submodule name")]
    UnsafeName,
    /// Worktree path is missing or cannot safely select a relative destination.
    #[error("unsafe or missing submodule path")]
    UnsafePath,
    /// URL is missing, empty, an option, or contains control bytes.
    #[error("unsafe or missing submodule URL")]
    UnsafeUrl,
}

#[cfg(doctest)]
#[doc = include_str!("../site/src/content/docs/guides/submodules.md")]
mod website_submodules_guide {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn child_environment(
    child: &crate::Repository,
    source: &crate::workflow::Environment,
) -> crate::workflow::Environment {
    let mut environment = source.clone();
    // Git clears repository-local context but preserves user command/environment config layers.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_IMPLICIT_WORK_TREE",
        "GIT_GRAFT_FILE",
        "GIT_NO_REPLACE_OBJECTS",
        "GIT_REPLACE_REF_BASE",
        "GIT_PREFIX",
        "GIT_SHALLOW_FILE",
    ] {
        environment = environment.without(name);
    }
    environment = environment
        .with("GIT_DIR", child.git_dir().as_os_str())
        .with("GIT_PREFIX", "");
    if environment.get("HOME").is_none()
        && let Some(home) = &child.inherited_config().context.home
    {
        environment = environment.with("HOME", home.as_os_str());
    }
    environment
}
