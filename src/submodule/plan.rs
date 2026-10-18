use super::{Definition, Update};
use crate::{Config, ObjectId};

/// Permission to include a user-configured shell command in an update plan.
///
/// This never permits commands in `.gitmodules`. Planning executes nothing; an executor must
/// retain this authorization separately from network and worktree-write permission.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Commands {
    /// Refuse external update commands (the default).
    #[default]
    Refuse,
    /// Permit commands from explicitly resolved user configuration.
    Run,
}

/// Configuration assignments needed to initialize one caller-selected submodule.
///
/// Append [`Self::settings`] to the parent's local config under `submodule.<name>` using
/// [`crate::config::ConfigEdit`]. These are proposed assignments, not a transaction: recheck the
/// effective configuration under caller exclusion before publishing, and reopen afterwards.
/// No URL rewriting, transport authorization, file creation or network access occurs. Derived
/// `Debug` includes unredacted proposed URLs; treat it like the original configuration.
#[derive(Clone, Debug)]
pub struct InitPlan {
    name: Vec<u8>,
    path: Vec<u8>,
    settings: Vec<(&'static str, Vec<u8>)>,
}

impl InitPlan {
    /// Logical configuration subsection name, independently of the worktree path.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Validated parent-relative worktree path.
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// Ordered key/value assignments for this name, with explicit string values.
    ///
    /// An empty iterator means no initialization assignments are needed in the supplied snapshot.
    /// Existing URL/update values are preserved. An inactive definition receives `active=true`.
    pub fn settings(&self) -> impl ExactSizeIterator<Item = (&'static str, &[u8])> {
        self.settings
            .iter()
            .map(|(key, value)| (*key, value.as_slice()))
    }
}

/// A selected update method, without executing it.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum UpdateMethod {
    /// Git's checkout, rebase, merge or none method.
    BuiltIn(Update),
    /// User-configured shell text after `!`; execution appends the recorded commit argument.
    ///
    /// Only resolved configuration and [`Commands::Run`] can produce this variant. It can contain
    /// credentials or other secrets; avoid logging the plan.
    Command(Vec<u8>),
}

/// A recorded commit and caller-selected submodule update policy.
///
/// The URL comes from initialized effective configuration. Checkout detaches HEAD; rebase and
/// merge preserve the child's branch; none leaves it unchanged. The plan does not establish that
/// objects are present, content is clean, paths are unaliased, or a write is safe. Fetch/clone,
/// checkout filters, reference publication and bounded recursion belong to explicit execution.
/// Derived `Debug` includes the unredacted URL and command text.
#[derive(Clone, Debug)]
pub struct UpdatePlan {
    name: Vec<u8>,
    path: Vec<u8>,
    recorded: ObjectId,
    url: Vec<u8>,
    method: UpdateMethod,
}

impl UpdatePlan {
    /// Logical configuration subsection name.
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    /// Validated parent-relative worktree path.
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// Commit selected from the parent's index by the caller; never a remote-tracking tip.
    pub fn recorded(&self) -> ObjectId {
        self.recorded
    }

    /// Initialized effective URL, preserved verbatim; this can contain credentials.
    pub fn url(&self) -> &[u8] {
        &self.url
    }

    /// Update behavior after effective configuration overrides `.gitmodules`.
    pub fn method(&self) -> &UpdateMethod {
        &self.method
    }
}

impl Definition {
    /// Computes initialization assignments against an explicit effective configuration snapshot.
    ///
    /// Select the definition from the parent's gitlinks before calling. `superproject_url` is the
    /// explicitly selected default remote (or authoritative local repository path). Relative URLs
    /// follow [`Self::resolve_url`]. No repository or config file is opened or written.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError`] for unsafe names/paths, implicit configured URLs, or missing/unsafe
    /// unresolved URLs. An existing
    /// URL is preserved even if metadata omits its URL; no transport validity is implied.
    ///
    /// ```
    /// use girt::Config;
    /// use girt::submodule::Modules;
    /// let modules = Modules::parse(
    ///     b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\nupdate = merge\n",
    /// )?;
    /// let plan = modules
    ///     .get(b"math")
    ///     .unwrap()
    ///     .plan_init(&Config::parse(b"")?, b"https://example.com/team/parent.git")?;
    /// assert_eq!(plan.name(), b"math");
    /// assert!(
    ///     plan.settings()
    ///         .any(|(key, value)| key == "update" && value == b"merge")
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn plan_init(
        &self,
        config: &Config,
        superproject_url: &[u8],
    ) -> Result<InitPlan, PlanError> {
        let path = checked_path(self)?;
        if config.value("submodule", Some(self.name()), "url") == Some(None) {
            return Err(PlanError::InvalidSetting("url"));
        }
        let mut settings = Vec::new();
        if !active(config, self.name(), &path)? {
            settings.push(("active", b"true".to_vec()));
        }
        if config
            .value("submodule", Some(self.name()), "url")
            .is_none()
        {
            let url = self.resolve_url(superproject_url)?;
            settings.push(("url", url));
        }
        if self.update_explicit
            && config
                .value("submodule", Some(self.name()), "update")
                .is_none()
        {
            let value = match self.update() {
                Update::Checkout => b"checkout".as_slice(),
                Update::Rebase => b"rebase",
                Update::Merge => b"merge",
                Update::None => b"none",
            };
            settings.push(("update", value.to_vec()));
        }
        Ok(InitPlan {
            name: self.name().to_vec(),
            path,
            settings,
        })
    }

    /// Selects the initialized URL, recorded commit and update method without side effects.
    ///
    /// The caller selects `recorded` from a normal gitlink entry and owns object-format checking.
    /// Effective config overrides `.gitmodules`. A `!command` requires resolved provenance from
    /// [`Config::resolve`] or repository opening, plus [`Commands::Run`]. All explicitly selected
    /// config scopes are eligible; scope alone grants no command permission. Bare parsed config
    /// has no provenance and cannot authorize an external command. `.gitmodules` commands are
    /// always rejected by [`super::Modules::parse`], independently of this permission.
    ///
    /// # Errors
    ///
    /// Returns [`PlanError::Uninitialized`] when no configured URL exists. Invalid URL/update
    /// values, unsafe names/paths, unprovenanced commands and denied command permission are
    /// distinct failures. Planning neither fetches missing objects nor validates a populated
    /// checkout.
    pub fn plan_update(
        &self,
        config: &Config,
        recorded: ObjectId,
        commands: Commands,
    ) -> Result<UpdatePlan, PlanError> {
        let path = checked_path(self)?;
        let url = match config.value("submodule", Some(self.name()), "url") {
            None => return Err(PlanError::Uninitialized),
            Some(Some(url)) if valid_url(url) => url.to_vec(),
            _ => return Err(PlanError::InvalidSetting("url")),
        };
        let entry = config.entries().iter().rev().find(|entry| {
            entry.section.eq_ignore_ascii_case(b"submodule")
                && entry.subsection.as_deref() == Some(self.name())
                && entry.name.eq_ignore_ascii_case(b"update")
        });
        let method = match entry {
            None => UpdateMethod::BuiltIn(self.update()),
            Some(entry) => match entry.value.as_deref() {
                Some(b"checkout") => UpdateMethod::BuiltIn(Update::Checkout),
                Some(b"rebase") => UpdateMethod::BuiltIn(Update::Rebase),
                Some(b"merge") => UpdateMethod::BuiltIn(Update::Merge),
                Some(b"none") => UpdateMethod::BuiltIn(Update::None),
                Some(value) if value.starts_with(b"!") => {
                    let command = &value[1..];
                    if command.is_empty() || command.contains(&0) {
                        return Err(PlanError::InvalidSetting("update"));
                    }
                    if entry.origin.is_none() {
                        return Err(PlanError::UntrustedCommand);
                    }
                    if commands == Commands::Refuse {
                        return Err(PlanError::CommandRefused);
                    }
                    UpdateMethod::Command(command.to_vec())
                }
                _ => return Err(PlanError::InvalidSetting("update")),
            },
        };
        Ok(UpdatePlan {
            name: self.name().to_vec(),
            path,
            recorded,
            url,
            method,
        })
    }
}

fn checked_path(definition: &Definition) -> Result<Vec<u8>, PlanError> {
    if !super::safe_relative_path(definition.name()) {
        return Err(super::Error::UnsafeName.into());
    }
    let path = definition
        .path()
        .filter(|path| super::safe_relative_path(path))
        .ok_or(super::Error::UnsafePath)?;
    Ok(path.to_vec())
}

fn active(config: &Config, name: &[u8], path: &[u8]) -> Result<bool, PlanError> {
    if let Some(active) = config
        .boolean("submodule", Some(name), "active")
        .map_err(|_| PlanError::InvalidSetting("active"))?
    {
        return Ok(active);
    }
    let specs: Vec<_> = config.values("submodule", None, "active").collect();
    if specs.is_empty() {
        return Ok(config.string("submodule", Some(name), "url").is_some());
    }
    let specs: Option<Vec<_>> = specs.into_iter().collect();
    let specs = specs.ok_or(PlanError::InvalidSetting("active"))?;
    let paths = crate::pathspec::Pathspec::parse(specs, &crate::pathspec::Context::new())?;
    paths.matches(path).map_err(|_| PlanError::ActiveAttributes)
}

fn valid_url(url: &[u8]) -> bool {
    !url.is_empty() && !url.starts_with(b"-") && !url.iter().any(u8::is_ascii_control)
}

/// Initialization/update policy cannot be planned from the selected inputs.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlanError {
    /// Definition name or path is unsafe for filesystem use.
    #[error(transparent)]
    Definition(#[from] super::Error),
    /// An unresolved metadata URL cannot be interpreted against the selected remote.
    #[error(transparent)]
    Url(#[from] super::UrlError),
    /// Configured activation pathspec syntax is invalid.
    #[error(transparent)]
    ActivePathspec(#[from] crate::pathspec::ParseError),
    /// Activation needs attributes, which this pure planner does not load.
    #[error("submodule activation requires attribute lookup")]
    ActiveAttributes,
    /// No effective configured URL exists; initialize explicitly before updating.
    #[error("submodule is not initialized")]
    Uninitialized,
    /// The selected key is implicit, empty or otherwise unsupported.
    #[error("invalid submodule {0} setting")]
    InvalidSetting(&'static str),
    /// External update text has no resolved user-configuration provenance.
    #[error("external submodule update method has no trusted source provenance")]
    UntrustedCommand,
    /// Configuration source is eligible, but the caller did not permit command execution.
    #[error("external submodule update commands are refused")]
    CommandRefused,
}
