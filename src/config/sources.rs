use std::path::{Path, PathBuf};

use super::Config;

/// Precedence class, ordered from lowest to highest.
///
/// Use scope to model precedence among explicitly selected sources, not to grant permissions.
/// Within a scope caller order still matters; user configuration requires XDG before home sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ConfigScope {
    /// Installation configuration.
    System,
    /// User configuration (XDG first, home second).
    Global,
    /// Common repository configuration.
    Local,
    /// Per-worktree configuration, when enabled by repository bootstrap.
    Worktree,
    /// Explicitly supplied runtime environment pairs.
    Environment,
    /// Caller overrides, applied last.
    Command,
}

/// A file selected by the caller; relative paths use the process working directory at resolution.
///
/// ```
/// use girt::Config;
/// use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
/// let directory = tempfile::tempdir()?;
/// let source = ConfigFile::new(directory.path().join("not-created"), ConfigScope::Global);
/// assert!(Config::resolve(&ConfigInputs::default().with_files(vec![source.clone()])).is_err());
/// // Optional means NotFound is allowed; syntax and permission failures still fail.
/// let optional = source.with_optional(true);
/// let config = Config::resolve(&ConfigInputs::default().with_files(vec![optional]))?;
/// assert!(config.entries().is_empty());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ConfigFile {
    /// OS-native source path.
    pub path: PathBuf,
    /// System, global, local or worktree; inherited by recursively included sources.
    /// Environment/command scopes are rejected for files.
    pub scope: ConfigScope,
    /// Ignore NotFound only; permission and parse failures always propagate.
    pub optional: bool,
    /// Also skip this file when opening it is denied, as Git skips an unreadable global file.
    /// Applies to this root only: an unreadable include and parse failures still propagate.
    pub skip_unreadable: bool,
}

impl ConfigFile {
    /// Creates a required configuration file source; use [`Self::with_optional`] to allow absence.
    pub fn new(path: PathBuf, scope: ConfigScope) -> Self {
        Self {
            path,
            scope,
            optional: false,
            skip_unreadable: false,
        }
    }

    /// Sets [`Self::path`].
    #[must_use]
    pub fn with_path(mut self, path: PathBuf) -> Self {
        self.path = path;
        self
    }

    /// Sets [`Self::scope`].
    #[must_use]
    pub fn with_scope(mut self, scope: ConfigScope) -> Self {
        self.scope = scope;
        self
    }

    /// Sets [`Self::optional`].
    #[must_use]
    pub fn with_optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
    }

    /// Sets [`Self::skip_unreadable`].
    #[must_use]
    pub fn with_skip_unreadable(mut self, skip_unreadable: bool) -> Self {
        self.skip_unreadable = skip_unreadable;
        self
    }
}

/// A physical variable location. Paths and values are never emitted through tracing.
///
/// Use the path and line in diagnostics or a source editor. Runtime assignments have no file path;
/// source locations do not authorize writing the effective snapshot back to disk.
///
/// ```
/// use girt::Config;
/// use girt::config::{ConfigInputs, ConfigScope};
/// let command = Config::parse(b"[core]\neditor = command-editor\n")?;
/// let resolved = Config::resolve(&ConfigInputs::default().with_command(Some(command)))?;
/// let origin = resolved.entries()[0]
///     .origin
///     .as_ref()
///     .ok_or("origin absent")?;
/// assert_eq!(origin.scope, ConfigScope::Command);
/// assert!(origin.location.path.is_none()); // Supplied bytes have no physical file.
/// assert_eq!(origin.location.line, 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    /// `None` denotes supplied runtime bytes rather than a file.
    pub path: Option<PathBuf>,
    /// One-based physical line (or environment pair ordinal).
    pub line: usize,
}

/// Provenance of one effective occurrence, including its outermost-first include ancestry.
///
/// Use provenance when explaining why a setting applies, or deciding which direct file a user may
/// edit. Scope is precedence, not trust; a Global source is not automatically safe to execute as a
/// program.
///
/// ```
/// use girt::Config;
/// use girt::config::{ConfigFile, ConfigInputs, ConfigScope};
/// let directory = tempfile::tempdir()?;
/// let root = directory.path().join("config");
/// let included = directory.path().join("editor");
/// std::fs::write(&root, b"[include]\npath = editor\n")?;
/// std::fs::write(&included, b"[core]\neditor = chosen\n")?;
/// let inputs = ConfigInputs::default()
///     .with_files(vec![ConfigFile::new(root.clone(), ConfigScope::Global)]);
/// let config = Config::resolve(&inputs)?;
/// let entry = config
///     .entries()
///     .iter()
///     .find(|entry| entry.name == b"editor")
///     .ok_or("absent")?;
/// let origin = entry.origin.as_ref().ok_or("origin absent")?;
/// assert_eq!(origin.scope, ConfigScope::Global); // Includes inherit their root's scope.
/// assert_eq!(origin.location.path.as_deref(), Some(included.as_path()));
/// assert_eq!(
///     origin.included_from[0].path.as_deref(),
///     Some(root.as_path())
/// );
/// assert_eq!(origin.included_from[0].line, 2); // The physical include directive.
///
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Origin {
    /// Root source precedence class.
    pub scope: ConfigScope,
    /// Location of this occurrence.
    pub location: SourceLocation,
    /// Include directive locations, excluding this occurrence.
    pub included_from: Vec<SourceLocation>,
}

/// Explicit context for path expansion and conditional includes; no ambient environment lookup.
///
/// Supply matching and expansion facts explicitly when configuration depends on branch,
/// Git-directory aliases, home directories, or an installation prefix. These facts are matching
/// inputs, not repository discovery or a trust check.
///
/// ```
/// use girt::Config;
/// use girt::config::{ConfigFile, ConfigInputs, ConfigScope, IncludeContext};
/// let directory = tempfile::tempdir()?;
/// let root = directory.path().join("root");
/// std::fs::write(&root, b"[includeIf \"onbranch:topic/**\"]\npath = topic\n")?;
/// std::fs::write(
///     directory.path().join("topic"),
///     b"[core]\neditor = topic-editor\n",
/// )?;
/// let inputs = ConfigInputs::default()
///     .with_files(vec![ConfigFile::new(root, ConfigScope::Global)])
///     .with_context(IncludeContext::default().with_branch(Some(b"topic/work".to_vec())));
/// let effective = Config::resolve(&inputs)?;
/// assert_eq!(
///     effective.string("core", None, "editor"),
///     Some(&b"topic-editor"[..])
/// );
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct IncludeContext {
    /// Home directory for `~/` expansion.
    pub home: Option<PathBuf>,
    /// Explicit homes for `~user/` expansion, with byte-oriented user names.
    pub user_homes: Vec<(Vec<u8>, PathBuf)>,
    /// Installation prefix for `%(prefix)/` expansion.
    pub prefix: Option<PathBuf>,
    /// Absolute Git directory spellings to match, including logical and canonical aliases.
    pub git_dirs: Vec<PathBuf>,
    /// Short current branch name; absent for detached HEAD or no repository.
    pub branch: Option<Vec<u8>>,
}

impl IncludeContext {
    /// Sets [`Self::home`].
    #[must_use]
    pub fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
    }

    /// Sets [`Self::user_homes`].
    #[must_use]
    pub fn with_user_homes(mut self, user_homes: Vec<(Vec<u8>, PathBuf)>) -> Self {
        self.user_homes = user_homes;
        self
    }

    /// Sets [`Self::prefix`].
    #[must_use]
    pub fn with_prefix(mut self, prefix: Option<PathBuf>) -> Self {
        self.prefix = prefix;
        self
    }

    /// Sets [`Self::git_dirs`].
    #[must_use]
    pub fn with_git_dirs(mut self, git_dirs: Vec<PathBuf>) -> Self {
        self.git_dirs = git_dirs;
        self
    }

    /// Sets [`Self::branch`].
    #[must_use]
    pub fn with_branch(mut self, branch: Option<Vec<u8>>) -> Self {
        self.branch = branch;
        self
    }
}

/// Finite work budgets applied independently to scan and effective expansion.
///
/// ```
/// use girt::config::{ConfigFile, ConfigInputs, ConfigScope, ResolveFailure, ResolveLimits};
/// let directory = tempfile::tempdir()?;
/// let path = directory.path().join("config");
/// std::fs::write(&path, b"[core]\neditor = too-many-bytes\n")?;
/// let inputs = ConfigInputs::default()
///     .with_files(vec![ConfigFile::new(path, ConfigScope::Global)])
///     .with_limits(ResolveLimits::default().with_bytes(4));
/// let error = girt::Config::resolve(&inputs).expect_err("source exceeds selected budget");
/// assert!(matches!(error.source, ResolveFailure::Limit(_)));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct ResolveLimits {
    /// Maximum nested include edges; roots have depth zero. Default: 10.
    pub depth: usize,
    /// Maximum loaded source bytes and, independently, expanded key/value bytes per pass.
    /// Cached file loads count unique path spellings. Independently caps section metadata per
    /// expansion pass: 128 logical bytes per occurrence plus section/subsection name lengths and
    /// 8 per member entry. These deterministic units bound retained membership, not allocator
    /// usage. Repeated include visits and empty headers count separately. Default: 16 MiB.
    pub bytes: usize,
    /// Maximum variable occurrences visited per pass. Default: 100,000.
    pub entries: usize,
    /// Maximum wildcard pattern × candidate work cells per match. Default: 1,000,000.
    pub match_cells: usize,
}

impl ResolveLimits {
    /// Sets [`Self::depth`].
    #[must_use]
    pub fn with_depth(mut self, depth: usize) -> Self {
        self.depth = depth;
        self
    }

    /// Sets [`Self::bytes`].
    #[must_use]
    pub fn with_bytes(mut self, bytes: usize) -> Self {
        self.bytes = bytes;
        self
    }

    /// Sets [`Self::entries`].
    #[must_use]
    pub fn with_entries(mut self, entries: usize) -> Self {
        self.entries = entries;
        self
    }

    /// Sets [`Self::match_cells`].
    #[must_use]
    pub fn with_match_cells(mut self, match_cells: usize) -> Self {
        self.match_cells = match_cells;
        self
    }
}

impl Default for ResolveLimits {
    fn default() -> Self {
        Self {
            depth: 10,
            bytes: 16 * 1024 * 1024,
            entries: 100_000,
            match_cells: 1_000_000,
        }
    }
}

/// Complete inputs for a fresh, read-only configuration snapshot.
///
/// Files are stably ordered by scope, preserving caller order within each scope. Supply XDG before
/// home configuration. Environment pairs follow files; command bytes follow environment pairs.
/// Repeated and empty values remain ordered; reset semantics belong to the consuming variable.
/// Each call rereads sources; an existing snapshot never changes. Concurrent external writers can
/// yield a mixed snapshot across files; no locks, writes, or environment mutation occur.
///
/// ```
/// use girt::Config;
/// use girt::config::ConfigInputs;
/// let inputs = ConfigInputs::default()
///     .with_environment(Some(Config::parse(
///         b"[core]\neditor = environment-editor\n",
///     )?))
///     .with_command(Some(Config::parse(b"[core]\neditor = caller-editor\n")?));
/// let effective = Config::resolve(&inputs)?;
/// assert_eq!(
///     effective.string("core", None, "editor"),
///     Some(&b"caller-editor"[..])
/// );
/// assert_eq!(effective.values("core", None, "editor").count(), 2);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ConfigInputs {
    /// Explicit system/global/local/worktree files. Optionality belongs to each source.
    pub files: Vec<ConfigFile>,
    /// Matching and expansion context.
    pub context: IncludeContext,
    /// Already parsed environment overrides, e.g. from [`Config::from_environment`].
    pub environment: Option<Config>,
    /// Parsed caller overrides, in occurrence order.
    pub command: Option<Config>,
    /// Resource budgets.
    pub limits: ResolveLimits,
}

impl ConfigInputs {
    /// Sets [`Self::files`].
    #[must_use]
    pub fn with_files(mut self, files: Vec<ConfigFile>) -> Self {
        self.files = files;
        self
    }

    /// Sets [`Self::context`].
    #[must_use]
    pub fn with_context(mut self, context: IncludeContext) -> Self {
        self.context = context;
        self
    }

    /// Sets [`Self::environment`].
    #[must_use]
    pub fn with_environment(mut self, environment: Option<Config>) -> Self {
        self.environment = environment;
        self
    }

    /// Sets [`Self::command`].
    #[must_use]
    pub fn with_command(mut self, command: Option<Config>) -> Self {
        self.command = command;
        self
    }

    /// Sets [`Self::limits`].
    #[must_use]
    pub fn with_limits(mut self, limits: ResolveLimits) -> Self {
        self.limits = limits;
        self
    }
}

impl ConfigInputs {
    /// Selects system/global files and runtime pairs from an explicit environment snapshot.
    ///
    /// `system` is the caller's installation default. `GIT_CONFIG_SYSTEM` replaces it;
    /// `GIT_CONFIG_NOSYSTEM` disables it. `GIT_CONFIG_GLOBAL` replaces both XDG and home files.
    /// Otherwise XDG (`XDG_CONFIG_HOME/git/config`, or `HOME/.config/git/config`) precedes
    /// `HOME/.gitconfig`. Missing files are optional, including environment-selected files.
    /// Repository opening supplies local/worktree files. `GIT_CONFIG` is a Git-config CLI selector
    /// and is deliberately not interpreted. User-home and installation-prefix expansion mappings
    /// remain caller-supplied. No process-global environment is accessed.
    ///
    /// # Errors
    ///
    /// Returns invalid runtime-pair or NOSYSTEM boolean errors. On non-Unix platforms, runtime
    /// pairs must be Unicode; file-selection paths remain OS-native.
    ///
    /// ```
    /// use girt::config::ConfigInputs;
    /// let directory = tempfile::tempdir()?;
    /// let selected = directory.path().join("global");
    /// std::fs::write(&selected, b"[core]\neditor = explicit\n")?;
    /// let inputs = ConfigInputs::from_environment(
    ///     None,
    ///     |name| (name == "GIT_CONFIG_GLOBAL").then(|| selected.clone().into_os_string()),
    ///     4,
    /// )?;
    /// let config = girt::Config::resolve(&inputs)?;
    /// assert_eq!(
    ///     config.string("core", None, "editor"),
    ///     Some(&b"explicit"[..])
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn from_environment(
        system: Option<PathBuf>,
        mut get: impl FnMut(&str) -> Option<std::ffi::OsString>,
        max_pairs: usize,
    ) -> Result<Self, super::ConfigError> {
        let mut inputs = Self::default();
        let home = get("HOME").map(PathBuf::from);
        inputs.context.home = home.clone();
        let no_system = get("GIT_CONFIG_NOSYSTEM");
        let no_system = match no_system {
            None => false,
            Some(value) => {
                #[cfg(unix)]
                let bytes = {
                    use std::os::unix::ffi::OsStrExt;
                    value.as_bytes()
                };
                #[cfg(not(unix))]
                let bytes = value
                    .to_str()
                    .ok_or(super::ConfigError {
                        line: 1,
                        reason: "invalid GIT_CONFIG_NOSYSTEM",
                    })?
                    .as_bytes();
                super::boolean(Some(bytes)).ok_or(super::ConfigError {
                    line: 1,
                    reason: "invalid GIT_CONFIG_NOSYSTEM",
                })?
            }
        };
        let system = get("GIT_CONFIG_SYSTEM").map(PathBuf::from).or(system);
        if !no_system && let Some(path) = system {
            inputs.files.push(ConfigFile {
                path,
                scope: ConfigScope::System,
                optional: true,
                skip_unreadable: false,
            });
        }
        if let Some(path) = get("GIT_CONFIG_GLOBAL") {
            inputs.files.push(ConfigFile {
                path: path.into(),
                scope: ConfigScope::Global,
                optional: true,
                skip_unreadable: false,
            });
        } else {
            let xdg = get("XDG_CONFIG_HOME")
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(".config")));
            if let Some(xdg) = xdg {
                inputs.files.push(ConfigFile {
                    path: xdg.join("git/config"),
                    scope: ConfigScope::Global,
                    optional: true,
                    skip_unreadable: false,
                });
            }
            if let Some(home) = home {
                inputs.files.push(ConfigFile {
                    path: home.join(".gitconfig"),
                    scope: ConfigScope::Global,
                    optional: true,
                    skip_unreadable: false,
                });
            }
        }
        #[cfg(not(unix))]
        let mut invalid_encoding = false;
        inputs.environment = Some(Config::from_environment(
            |key| {
                get(key).map(|value| {
                    #[cfg(unix)]
                    {
                        use std::os::unix::ffi::OsStringExt;
                        value.into_vec()
                    }
                    #[cfg(not(unix))]
                    {
                        match value.into_string() {
                            Ok(value) => value.into_bytes(),
                            Err(_) => {
                                invalid_encoding = true;
                                Vec::new()
                            }
                        }
                    }
                })
            },
            max_pairs,
        )?);
        #[cfg(not(unix))]
        if invalid_encoding {
            return Err(super::ConfigError {
                line: 1,
                reason: "environment pair is not Unicode",
            });
        }
        Ok(inputs)
    }
}

impl ConfigInputs {
    /// Selects the configuration sources `git` reads, from an explicit environment snapshot.
    ///
    /// Use this to behave like Git: pass the result to [`crate::Repository::open_with_config`] or
    /// [`crate::Repository::discover_with_config`], which add the repository's own files, or to
    /// [`Config::resolve`] for settings outside a repository (identity, `init.defaultBranch`).
    /// Sources, lowest precedence first:
    ///
    /// 1. **System**: `GIT_CONFIG_SYSTEM` if set, otherwise the installation file below. A true
    ///    `GIT_CONFIG_NOSYSTEM` removes it.
    /// 2. **Global**: `GIT_CONFIG_GLOBAL` if set. Otherwise `$XDG_CONFIG_HOME/git/config` (or
    ///    `$HOME/.config/git/config` when `XDG_CONFIG_HOME` is unset or empty), then
    ///    `$HOME/.gitconfig`.
    /// 3. **Local and worktree**: added by repository opening, not by this constructor.
    /// 4. **Environment**: `GIT_CONFIG_COUNT` with `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>`,
    ///    then the `GIT_CONFIG_PARAMETERS` that `git -c` passes to child processes.
    ///
    /// Missing files are skipped, as Git skips them, and an empty `GIT_CONFIG_GLOBAL` or
    /// `GIT_CONFIG_SYSTEM` selects no file. Global files that cannot be opened for lack of
    /// permission are skipped too ([`ConfigFile::skip_unreadable`]); an unreadable system file is
    /// an error, as in Git.
    ///
    /// A relative `GIT_CONFIG_GLOBAL` or `GIT_CONFIG_SYSTEM` resolves against the environment's
    /// current directory ([`Environment::with_current_dir`]). That matches Git outside a
    /// repository and in a bare one. Inside a repository with a working tree, Git resolves it
    /// against the top of the working tree instead: to match, discover the repository first and
    /// set the current directory to [`crate::Repository::worktree`].
    /// `HOME` also sets [`IncludeContext::home`] for `~/` include paths. Runtime pairs use the
    /// default [`ResolveLimits`] entry and byte budgets; call [`Self::from_environment`] to choose
    /// the system file or pair budget yourself.
    ///
    /// The installation file depends on how Git was built, so it is a platform convention:
    ///
    /// - macOS: the first existing of `/opt/homebrew/etc/gitconfig`, `/usr/local/etc/gitconfig`
    ///   (Homebrew) and `/etc/gitconfig`. This checks the files' existence.
    /// - Other Unix: `/etc/gitconfig`.
    /// - Windows: none; supply it with `GIT_CONFIG_SYSTEM` or [`Self::from_environment`].
    ///
    /// Apple's Xcode Git also reads a file inside the Xcode bundle, which is not selected here.
    /// The process environment is never read; the caller supplies `environment`, usually
    /// [`Environment::from_process`].
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`](super::ConfigError) for a `GIT_CONFIG_NOSYSTEM` that is not a
    /// boolean, malformed or over-budget runtime pairs, or invalid `GIT_CONFIG_PARAMETERS`
    /// quoting. On non-Unix platforms runtime pairs must be Unicode. Files are not read until
    /// resolution.
    ///
    /// # Examples
    ///
    /// ```
    /// use girt::Config;
    /// use girt::config::ConfigInputs;
    /// use girt::workflow::Environment;
    ///
    /// let home = tempfile::tempdir()?;
    /// std::fs::write(home.path().join(".gitconfig"), b"[user]\n\tname = Home\n")?;
    /// let environment = Environment::default()
    ///     .with("HOME", home.path())
    ///     .with("GIT_CONFIG_NOSYSTEM", "1")
    ///     .with("GIT_CONFIG_COUNT", "1")
    ///     .with("GIT_CONFIG_KEY_0", "user.email")
    ///     .with("GIT_CONFIG_VALUE_0", "home@example.com");
    /// let config = Config::resolve(&ConfigInputs::git_default(&environment)?)?;
    /// assert_eq!(config.string("user", None, "name"), Some(&b"Home"[..]));
    /// assert_eq!(
    ///     config.string("user", None, "email"),
    ///     Some(&b"home@example.com"[..])
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// [`Environment::with_current_dir`]: crate::workflow::Environment::with_current_dir
    /// [`Environment::from_process`]: crate::workflow::Environment::from_process
    pub fn git_default(
        environment: &crate::workflow::Environment,
    ) -> Result<Self, super::ConfigError> {
        let limits = ResolveLimits::default();
        let get = |name: &str| environment.get(name).map(std::ffi::OsStr::to_owned);
        let mut inputs = Self::from_environment(default_system_file(), get, limits.entries)?;
        // An empty selection names no file.
        inputs
            .files
            .retain(|file| !file.path.as_os_str().is_empty());
        for file in &mut inputs.files {
            file.skip_unreadable = file.scope == ConfigScope::Global;
            if let Some(directory) = environment.current_dir()
                && file.path.is_relative()
            {
                file.path = directory.join(&file.path);
            }
        }
        #[cfg(not(unix))]
        let mut invalid_encoding = false;
        let environment_config = Config::from_command_environment(
            |name| {
                let value = environment.get(name)?;
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    Some(value.as_bytes().to_vec())
                }
                #[cfg(not(unix))]
                {
                    let value = value.to_str();
                    invalid_encoding |= value.is_none();
                    Some(value.unwrap_or_default().as_bytes().to_vec())
                }
            },
            limits.entries,
            limits.bytes,
        )?;
        #[cfg(not(unix))]
        if invalid_encoding {
            return Err(super::ConfigError {
                line: 1,
                reason: "environment pair is not Unicode",
            });
        }
        inputs.environment = Some(environment_config);
        Ok(inputs)
    }
}

/// The installation configuration file Git's usual builds for this platform read.
fn default_system_file() -> Option<PathBuf> {
    system_file("gitconfig")
}

/// The system-wide Git file `name`, such as `gitconfig` or `gitattributes`.
///
/// Git compiles its system directory in; on macOS the first existing file among the usual
/// installation prefixes is chosen, elsewhere on Unix `/etc` is used.
pub(crate) fn system_file(name: &str) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        ["/opt/homebrew/etc", "/usr/local/etc", "/etc"]
            .into_iter()
            .map(|directory| Path::new(directory).join(name))
            .find(|path| path.exists())
    } else if cfg!(unix) {
        Some(Path::new("/etc").join(name))
    } else {
        None
    }
}
