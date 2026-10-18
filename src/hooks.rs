//! Repository hooks: programs a repository asks Git to run at set points.
//!
//! A hook is an executable file named after its event, such as `pre-commit`, `commit-msg`,
//! `post-checkout` or `pre-push`. Git looks for hooks in the directory named by
//! `core.hooksPath` (a relative path is resolved against the top of the working tree, or the Git
//! directory of a bare repository) and otherwise in the `hooks` directory of the common Git
//! directory, which linked worktrees share. A file that exists but is not executable is ignored;
//! Git prints a hint about it.
//!
//! # Running hooks is a trust decision
//!
//! A hook is a program chosen by whoever controls the repository's files or configuration.
//! Running one executes that program with the caller's privileges and environment, so girt never
//! runs hooks unless the caller asks with [`Policy::Run`]. The default, [`Policy::Ignore`], skips
//! hooks as girt has always done. [`Policy::Refuse`] fails with [`Error::Refused`] when a hook
//! would run, for callers that must not silently differ from Git.
//!
//! # Run a hook
//!
//! [`Hooks::new`] locates the hook directory; [`Hooks::run`] runs one hook with the arguments,
//! standard input and extra environment of an [`Invocation`]. Hooks run in the top of the working
//! tree (the Git directory when bare) with exactly the variables of the supplied
//! [`Environment`], plus `GIT_PREFIX` and the invocation's additions. Standard output and standard
//! error are captured together, up to [`Hooks::with_output_limit`], and returned in [`Ran`].
//!
//! ```
//! # #[cfg(unix)] {
//! use std::os::unix::fs::PermissionsExt as _;
//!
//! use girt::hooks::{Hooks, Invocation, Outcome, Policy};
//! use girt::transport::TransportControl;
//! use girt::workflow::Environment;
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(ObjectFormat::Sha1, directory.path(), InitKind::Worktree)?;
//! let hook = repo.common_dir().join("hooks/commit-msg");
//! std::fs::create_dir_all(hook.parent().unwrap())?;
//! std::fs::write(&hook, "#!/bin/sh\necho \"checking $1\"\nexit 1\n")?;
//! std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))?;
//!
//! let environment: Environment = [("PATH", "/usr/bin:/bin")].into_iter().collect();
//! let hooks = Hooks::new(&repo, &environment)?;
//! let cancel = std::sync::atomic::AtomicBool::new(false);
//! let invocation = Invocation::new().with_argument("MSG");
//!
//! // Ignored unless the caller opts in.
//! let skipped = hooks.run(
//!     "commit-msg",
//!     &invocation,
//!     Policy::Ignore,
//!     TransportControl::new(&cancel),
//! )?;
//! assert!(matches!(skipped, Outcome::Ignored(_)));
//!
//! let Outcome::Ran(ran) = hooks.run(
//!     "commit-msg",
//!     &invocation,
//!     Policy::Run,
//!     TransportControl::new(&cancel),
//! )?
//! else {
//!     panic!("hook did not run");
//! };
//! assert!(!ran.success());
//! assert_eq!(ran.code(), Some(1));
//! assert_eq!(ran.output(), b"checking MSG\n");
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
//!
//! # Platforms
//!
//! Hooks run on Linux and macOS. Elsewhere a hook that would run fails with
//! [`Error::Unsupported`]; [`Policy::Ignore`] still skips it.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::Repository;
use crate::transport::TransportControl;
use crate::workflow::Environment;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(crate) mod process;

/// Default bound on captured hook output: 1 MiB.
const DEFAULT_OUTPUT_LIMIT: usize = 1 << 20;

/// Whether an operation runs the repository's hooks.
///
/// Library operations take a policy and default to [`Policy::Ignore`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Policy {
    /// Skip hooks without looking for them. This is the default.
    #[default]
    Ignore,
    /// Fail with [`Error::Refused`] when an executable hook exists, instead of running it or
    /// differing from Git silently.
    Refuse,
    /// Run hooks as Git does.
    Run,
}

/// The hook directory of one repository, and the environment hooks run with.
#[derive(Clone, Debug)]
pub struct Hooks {
    directory: PathBuf,
    working_directory: PathBuf,
    environment: Environment,
    prefix: OsString,
    output_limit: usize,
}

impl Hooks {
    /// Locates the hook directory of `repository`; hooks will run with `environment`.
    ///
    /// Uses `core.hooksPath` when set, resolving a relative value against the top of the working
    /// tree (the Git directory when bare) and a leading `~/` against `HOME` from `environment`,
    /// and otherwise `<common Git directory>/hooks`. Nothing is read from the directory until a
    /// hook is looked up. `GIT_PREFIX` is the environment's current directory relative to the top
    /// of the working tree, ending in `/`, or empty.
    ///
    /// # Errors
    ///
    /// Returns [`Error::HooksPath`] when `core.hooksPath` uses `~/` without `HOME` in
    /// `environment`, or `~user/`, which girt does not expand. Git would find hooks there, so
    /// skipping them silently could bypass a hook the repository relies on.
    pub fn new(repository: &Repository, environment: &Environment) -> Result<Self, Error> {
        let working_directory = repository
            .worktree()
            .unwrap_or(repository.git_dir())
            .to_owned();
        let directory = match repository.config().string("core", None, "hookspath") {
            Some(path) => {
                if let Some(rest) = path.strip_prefix(b"~/") {
                    let home = environment
                        .get("HOME")
                        .ok_or(Error::HooksPath("~/ needs HOME in the environment"))?;
                    Path::new(home).join(path_from_bytes(rest))
                } else if path.starts_with(b"~") {
                    return Err(Error::HooksPath("~user/ expansion is not supported"));
                } else {
                    working_directory.join(path_from_bytes(path))
                }
            }
            None => repository.common_dir().join("hooks"),
        };
        let prefix = environment
            .current_dir()
            .and_then(|current| current.strip_prefix(repository.worktree()?).ok())
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(|relative| {
                let mut prefix = relative.as_os_str().to_owned();
                prefix.push("/");
                prefix
            })
            .unwrap_or_default();
        Ok(Self {
            directory,
            working_directory,
            environment: environment.clone(),
            prefix,
            output_limit: DEFAULT_OUTPUT_LIMIT,
        })
    }

    /// Bounds captured standard output and error per hook; output beyond it is discarded while
    /// the hook keeps running. The default is 1 MiB.
    #[must_use]
    pub fn with_output_limit(mut self, bytes: usize) -> Self {
        self.output_limit = bytes;
        self
    }

    /// Returns the directory hooks are looked up in.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Returns the directory hooks run in.
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }

    /// Looks up the hook `name` without running it.
    pub fn find(&self, name: &str) -> Found {
        let path = self.directory.join(name);
        match path.metadata() {
            Ok(metadata) if metadata.is_file() => {
                if is_executable(&metadata) {
                    Found::Executable(path)
                } else {
                    Found::NotExecutable(path)
                }
            }
            _ => Found::Absent,
        }
    }

    /// Runs the hook `name` under `policy`.
    ///
    /// Waits for the hook and any process that keeps its output pipes open. `control` can cancel
    /// the wait or set a deadline; the hook's process group is then killed.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Refused`] under [`Policy::Refuse`] when the hook is executable,
    /// [`Error::Unsupported`] on platforms without hook execution, and [`Error::Process`] when
    /// the hook cannot be started, its pipes fail, or `control` cancels it. A hook that exits
    /// non-zero is not an error; inspect [`Ran::success`].
    pub fn run(
        &self,
        name: &str,
        invocation: &Invocation,
        policy: Policy,
        control: TransportControl<'_>,
    ) -> Result<Outcome, Error> {
        if policy == Policy::Ignore {
            return Ok(Outcome::Ignored(name.to_owned()));
        }
        let path = match self.find(name) {
            Found::Absent => return Ok(Outcome::Absent),
            Found::NotExecutable(path) => return Ok(Outcome::NotExecutable(path)),
            Found::Executable(path) if policy == Policy::Refuse => {
                return Err(Error::Refused {
                    hook: name.to_owned(),
                    path,
                });
            }
            Found::Executable(path) => path,
        };
        let mut command = std::process::Command::new(&path);
        command
            .args(&invocation.arguments)
            .current_dir(&self.working_directory)
            .env_clear()
            .envs(self.environment.vars())
            .env("GIT_PREFIX", &self.prefix)
            .envs(
                invocation
                    .environment
                    .iter()
                    .map(|(key, value)| (key, value)),
            );
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let (status, output) =
                process::run(&mut command, &invocation.stdin, control, self.output_limit).map_err(
                    |source| Error::Process {
                        hook: name.to_owned(),
                        source,
                    },
                )?;
            Ok(Outcome::Ran(Ran { status, output }))
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (command, control, self.output_limit);
            Err(Error::Unsupported(name.to_owned()))
        }
    }
}

impl Hooks {
    /// Runs a hook whose failure stops the operation, such as `pre-commit` or `pre-push`.
    ///
    /// Like [`Self::run`], but a hook that ran and exited unsuccessfully is an error.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Rejected`] with the hook's exit code and output when it fails, and the
    /// errors of [`Self::run`].
    pub fn run_blocking(
        &self,
        name: &str,
        invocation: &Invocation,
        policy: Policy,
        control: TransportControl<'_>,
    ) -> Result<Outcome, Error> {
        match self.run(name, invocation, policy, control)? {
            Outcome::Ran(ran) if !ran.success() => Err(Error::Rejected {
                hook: name.to_owned(),
                code: ran.code(),
                output: ran.output,
            }),
            outcome => Ok(outcome),
        }
    }

    /// Returns `path` as Git passes repository paths to hooks: relative to the directory hooks
    /// run in when it lies inside it, absolute otherwise.
    pub(crate) fn argument_path(&self, path: &Path) -> OsString {
        path.strip_prefix(&self.working_directory)
            .unwrap_or(path)
            .as_os_str()
            .to_owned()
    }
}

/// What [`Hooks::find`] found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Found {
    /// No file has the hook's name.
    Absent,
    /// A file exists but is not executable; Git ignores it with a hint.
    NotExecutable(PathBuf),
    /// An executable file that would run.
    Executable(PathBuf),
}

/// The arguments, standard input and extra environment variables of one hook run.
#[derive(Clone, Debug, Default)]
pub struct Invocation {
    arguments: Vec<OsString>,
    stdin: Vec<u8>,
    environment: Vec<(OsString, OsString)>,
}

impl Invocation {
    /// Returns an invocation with no arguments, empty input and no extra variables.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends an argument.
    #[must_use]
    pub fn with_argument(mut self, argument: impl Into<OsString>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    /// Sets the bytes written to the hook's standard input, which is then closed. A hook that
    /// exits without reading all of it is not an error.
    #[must_use]
    pub fn with_stdin(mut self, stdin: impl Into<Vec<u8>>) -> Self {
        self.stdin = stdin.into();
        self
    }

    /// Sets a variable in addition to the [`Hooks`] environment, overriding it.
    #[must_use]
    pub fn with_env(mut self, name: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((name.into(), value.into()));
        self
    }

    /// Returns the arguments.
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    /// Returns the standard input bytes.
    pub fn stdin(&self) -> &[u8] {
        &self.stdin
    }
}

/// The result of [`Hooks::run`] that did not fail.
#[derive(Debug)]
#[non_exhaustive]
pub enum Outcome {
    /// [`Policy::Ignore`] skipped the named hook without looking for it.
    Ignored(String),
    /// No hook of that name exists.
    Absent,
    /// The hook exists but is not executable, so it did not run. Git prints a hint.
    NotExecutable(PathBuf),
    /// The hook ran.
    Ran(Ran),
}

impl Outcome {
    /// Returns whether an operation that a failing hook blocks may continue: true unless the hook
    /// ran and exited unsuccessfully.
    pub fn allows(&self) -> bool {
        match self {
            Self::Ran(ran) => ran.success(),
            Self::Ignored(_) | Self::Absent | Self::NotExecutable(_) => true,
        }
    }
}

/// A hook that ran to completion.
#[derive(Debug)]
pub struct Ran {
    status: std::process::ExitStatus,
    output: Vec<u8>,
}

impl Ran {
    /// Returns whether the hook exited with status zero.
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// Returns the exit code, or `None` when a signal ended the hook.
    pub fn code(&self) -> Option<i32> {
        self.status.code()
    }

    /// Returns the exit status.
    pub fn status(&self) -> std::process::ExitStatus {
        self.status
    }

    /// Returns standard output followed by standard error, truncated to the output limit.
    pub fn output(&self) -> &[u8] {
        &self.output
    }
}

/// A hook could not be run.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// [`Policy::Refuse`] found an executable hook.
    #[error("hook {hook} would run, but running hooks was refused")]
    Refused {
        /// The hook name.
        hook: String,
        /// Its path.
        path: PathBuf,
    },
    /// `core.hooksPath` uses path expansion girt cannot perform.
    #[error("unsupported core.hooksPath: {0}")]
    HooksPath(&'static str),
    /// This platform cannot run hooks.
    #[error("running hook {0} is not supported on this platform")]
    Unsupported(String),
    /// Starting, feeding or waiting for the hook failed, or the wait was cancelled.
    #[error("running hook {hook}")]
    Process {
        /// The hook name.
        hook: String,
        /// The failure.
        #[source]
        source: std::io::Error,
    },
    /// A hook whose failure stops the operation exited unsuccessfully; see
    /// [`Hooks::run_blocking`].
    #[error("hook {hook} failed{}", code.map(|code| format!(" with exit code {code}")).unwrap_or_default())]
    Rejected {
        /// The hook name.
        hook: String,
        /// The exit code, or `None` when a signal ended the hook.
        code: Option<i32>,
        /// Standard output followed by standard error, truncated to the output limit.
        output: Vec<u8>,
    },
}

/// Converts configuration bytes to a hook argument; non-Unix platforms lose invalid UTF-8.
#[cfg(unix)]
pub(crate) fn os_string(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::OsStr::from_bytes(bytes).to_owned()
}

#[cfg(not(unix))]
pub(crate) fn os_string(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

#[cfg(unix)]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt as _;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_: &std::fs::Metadata) -> bool {
    true
}
