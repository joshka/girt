//! Explicit, opt-in filesystem-monitor hook queries.
//!
//! A monitor reports changed paths since an index checkpoint. It is trusted executable code:
//! [`Commands::Refuse`] is the default, and querying a configured hook then returns
//! [`Error::Refused`]. [`Repository::fsmonitor`] resolves configuration without executing anything.
//! [`Monitor::query`] runs protocols one and two; callers must fall back to a full scan on query
//! failure or when [`Changes::full_scan`] is true. A successful response is a monitor promise, not
//! an independent filesystem observation. The built-in daemon is explicitly unsupported.
//!
//! ```
//! use girt::fsmonitor::Commands;
//! use girt::workflow::Environment;
//! use girt::{InitKind, ObjectFormat, Repository};
//!
//! let directory = tempfile::tempdir()?;
//! let repo = Repository::init(
//!     ObjectFormat::Sha1,
//!     directory.path().join("r"),
//!     InitKind::Worktree,
//! )?;
//! // No core.fsmonitor configuration means there is no monitor to query.
//! assert!(
//!     repo.fsmonitor(&Environment::default(), Commands::Refuse)?
//!         .is_none()
//! );
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::Repository;
use crate::index::MonitorToken;
use crate::workflow::Environment;

/// Whether filesystem-monitor hooks may execute with the current process's permissions.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Commands {
    /// Return [`Error::Refused`] rather than executing configured code.
    #[default]
    Refuse,
    /// Run the configured hook in the working-tree root with the supplied environment.
    ///
    /// Hooks can modify files or start other programs. This policy is not a sandbox.
    Run,
}

/// Bounds for one monitor response, independent of the hook's own resource consumption.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum stdout bytes, including token and NUL separators; default 16 MiB.
    pub max_bytes: usize,
    /// Maximum changed paths; default one million.
    pub max_paths: usize,
    /// Maximum bytes in a path or opaque token; default one MiB.
    pub max_path_bytes: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 16 << 20,
            max_paths: 1_000_000,
            max_path_bytes: 1 << 20,
        }
    }
}

impl Limits {
    /// Sets [`Self::max_bytes`].
    #[must_use]
    pub fn with_max_bytes(mut self, value: usize) -> Self {
        self.max_bytes = value;
        self
    }
    /// Sets [`Self::max_paths`].
    #[must_use]
    pub fn with_max_paths(mut self, value: usize) -> Self {
        self.max_paths = value;
        self
    }
    /// Sets [`Self::max_path_bytes`].
    #[must_use]
    pub fn with_max_path_bytes(mut self, value: usize) -> Self {
        self.max_path_bytes = value;
        self
    }
}

/// A repository's configured hook and its explicit execution policy.
///
/// This snapshot owns the working-tree location, command, protocol choice and environment.
/// Refresh it when configuration changes. It does not start a persistent process.
#[derive(Clone, Debug)]
pub struct Monitor {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    command: Vec<u8>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    root: PathBuf,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    environment: Environment,
    commands: Commands,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    version: Option<u8>,
}

/// Paths changed since the queried checkpoint and the checkpoint for the next query.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Changes {
    token: MonitorToken,
    paths: Vec<Vec<u8>>,
    full_scan: bool,
}
impl Changes {
    /// The checkpoint to record after completing the corresponding full or incremental scan.
    pub fn token(&self) -> &MonitorToken {
        &self.token
    }
    /// Changed repository-relative paths; a trailing `/` invalidates the directory's descendants.
    pub fn paths(&self) -> &[Vec<u8>] {
        &self.paths
    }
    /// Whether all paths must be scanned, including when no compatible old checkpoint was supplied.
    pub fn full_scan(&self) -> bool {
        self.full_scan
    }
}

/// Monitor configuration, trust, execution or response failure.
///
/// A caller must never reuse clean index bits after one of these errors. A status operation can
/// recover from hook failure or invalid output by scanning the entire working tree.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The caller has not authorized executing the configured hook.
    #[error("filesystem-monitor hook execution was refused")]
    Refused,
    /// A requested monitor implementation or host is unsupported.
    #[error("unsupported filesystem monitor: {0}")]
    Unsupported(&'static str),
    /// Configuration does not name a supported hook protocol.
    #[error("invalid filesystem-monitor configuration: {0}")]
    Config(&'static str),
    /// Starting the hook or operating its owned pipes failed.
    #[error("filesystem-monitor hook I/O failed")]
    Io(#[source] std::io::Error),
    /// The hook returned an unsuccessful exit status.
    #[error("filesystem-monitor hook failed")]
    Failed,
    /// The hook response cannot safely identify changed paths or a checkpoint.
    #[error("invalid filesystem-monitor response: {0}")]
    Response(&'static str),
    /// A response exceeds a configured resource ceiling.
    #[error("filesystem-monitor limit exceeded: {0}")]
    Limit(&'static str),
    /// Cancellation was observed before execution or during pipe or exit waits.
    #[error("filesystem-monitor query cancelled")]
    Cancelled,
}

impl Repository {
    /// Resolves `core.fsmonitor` and `core.fsmonitorHookVersion` without executing a program.
    ///
    /// An absent or false monitor yields `None`. A hook path supports explicitly configured
    /// versions 1 and 2; when the version is absent, querying tries version 2 and retries version 1
    /// if the hook exits unsuccessfully. `environment` supplies the child environment, with
    /// `GIT_DIR`, `GIT_COMMON_DIR` and `GIT_WORK_TREE` set to this repository.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Unsupported`] for `core.fsmonitor=true` (the built-in daemon) or a bare
    /// repository, and [`Error::Config`] for unsupported protocol versions or NUL command bytes.
    pub fn fsmonitor(
        &self,
        environment: &Environment,
        commands: Commands,
    ) -> Result<Option<Monitor>, Error> {
        let config = self.config();
        match config.boolean("core", None, "fsmonitor") {
            Ok(None | Some(false)) => return Ok(None),
            Ok(Some(true)) => return Err(Error::Unsupported("Git built-in daemon IPC")),
            Err(_) => {}
        }

        let command = config.string("core", None, "fsmonitor").unwrap_or_default();
        if command.contains(&0) {
            return Err(Error::Config("NUL in hook command"));
        }

        let version = match config.string("core", None, "fsmonitorhookversion") {
            None => None,
            Some(b"1") => Some(1),
            Some(b"2") => Some(2),
            Some(_) => return Err(Error::Config("hook version must be 1 or 2")),
        };

        let root = self
            .worktree()
            .ok_or(Error::Unsupported("bare repository"))?
            .to_path_buf();
        let environment = environment
            .clone()
            .with("GIT_DIR", self.git_dir())
            .with("GIT_COMMON_DIR", self.common_dir())
            .with("GIT_WORK_TREE", &root);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let _ = (command, root, environment, version);

        Ok(Some(Monitor {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            command: command.to_vec(),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            root,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            environment,
            commands,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            version,
        }))
    }
}

impl Monitor {
    /// Queries with default response limits and a cancellation flag that is never set.
    ///
    /// See [`Self::query_with`] for execution, protocol and freshness contracts.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::query_with`] except cancellation.
    pub fn query(&self, checkpoint: Option<&MonitorToken>) -> Result<Changes, Error> {
        self.query_with(checkpoint, Limits::default(), &AtomicBool::new(false))
    }

    /// Queries changes since `checkpoint`, or requests a full scan when no compatible token exists.
    ///
    /// Version-one checkpoints are captured immediately before launching the hook; version-two
    /// tokens come from its response. The hook receives the version and old checkpoint as separate
    /// arguments, through `sh -c` as Git runs configured commands. Stdout is bounded and stderr is
    /// drained and discarded. On macOS/Linux an owned process group is killed and reaped on every
    /// failure or cancellation, and same-group helpers are killed after successful exit too.
    /// A hook that escapes that group is outside this lifecycle contract. No deadline is imposed:
    /// a background helper retaining stdout can keep the query waiting until cancellation, even
    /// after its parent exits. Cancellation interrupts pipe and exit waits, subject to scheduling.
    /// Other hosts return unsupported before starting a program.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Refused`] without executing when the policy refuses commands, or a typed
    /// configuration, execution, cancellation, resource or response error. Callers must full-scan
    /// after a failed query; errors never authorize reusing clean FSMN bits.
    pub fn query_with(
        &self,
        checkpoint: Option<&MonitorToken>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Changes, Error> {
        check(cancel)?;
        if self.commands == Commands::Refuse {
            return Err(Error::Refused);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let version = self.version.unwrap_or(2);
            let result = self.query_version(version, checkpoint, limits, cancel);
            if self.version.is_none() && matches!(result, Err(Error::Failed)) {
                return self.query_version(1, checkpoint, limits, cancel);
            }
            result
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (checkpoint, limits);
            Err(Error::Unsupported("hook execution requires macOS/Linux"))
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn query_version(
        &self,
        version: u8,
        checkpoint: Option<&MonitorToken>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Changes, Error> {
        use std::io::Read as _;
        use std::os::unix::ffi::OsStringExt as _;
        use std::process::Command;

        let valid_opaque = |token: &[u8]| {
            !token.is_empty() && !token.contains(&0) && token.len() <= limits.max_path_bytes
        };
        let compatible = match (version, checkpoint) {
            (1, Some(MonitorToken::Timestamp(_))) => true,
            (2, Some(MonitorToken::Opaque(token))) => valid_opaque(token),
            _ => false,
        };

        let old = match (version, checkpoint) {
            (1, Some(MonitorToken::Timestamp(time))) => time.to_string().into_bytes(),
            (2, Some(MonitorToken::Opaque(token))) if valid_opaque(token) => token.clone(),
            _ => b"0".to_vec(),
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
            .ok_or(Error::Response("clock cannot form a protocol-one token"))?;

        let mut script = self.command.clone();
        script.extend_from_slice(b" \"$@\"");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(std::ffi::OsString::from_vec(script))
            .arg("--")
            .arg(version.to_string())
            .arg(std::ffi::OsString::from_vec(old))
            .current_dir(&self.root)
            .env_clear()
            .envs(self.environment.vars());

        let io = |source| {
            if cancel.load(Ordering::Relaxed) {
                Error::Cancelled
            } else {
                Error::Io(source)
            }
        };

        let mut server = crate::transport::Server::spawn(
            &mut command,
            crate::transport::TransportControl::new(cancel),
        )
        .map_err(io)?;

        let (reader, writer) = server.streams();
        drop(writer);
        let mut output = Vec::new();
        reader
            .take(
                u64::try_from(limits.max_bytes)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1),
            )
            .read_to_end(&mut output)
            .map_err(io)?;

        if output.len() > limits.max_bytes {
            return Err(Error::Limit("stdout bytes"));
        }
        if !server.wait().map_err(io)?.success() {
            return Err(Error::Failed);
        }

        response(version, now, compatible, &output, limits)
    }
}

fn check(cancel: &AtomicBool) -> Result<(), Error> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(any(test, target_os = "linux", target_os = "macos"))]
fn response(
    version: u8,
    now: u64,
    compatible: bool,
    bytes: &[u8],
    limits: Limits,
) -> Result<Changes, Error> {
    if bytes.len() > limits.max_bytes {
        return Err(Error::Limit("stdout bytes"));
    }

    let mut fields = bytes.split(|byte| *byte == 0);
    let token = if version == 2 {
        let token = fields
            .next()
            .filter(|token| !token.is_empty())
            .ok_or(Error::Response("missing checkpoint"))?;
        if token.len() > limits.max_path_bytes {
            return Err(Error::Limit("checkpoint bytes"));
        }
        MonitorToken::Opaque(token.to_vec())
    } else {
        MonitorToken::Timestamp(now)
    };

    let paths = if version == 1 {
        bytes
    } else {
        bytes
            .get(
                match &token {
                    MonitorToken::Opaque(token) => token.len() + 1,
                    _ => 0,
                }..,
            )
            .ok_or(Error::Response("unterminated checkpoint"))?
    };

    if !paths.is_empty() && paths.last() != Some(&0) {
        return Err(Error::Response("unterminated path"));
    }

    let mut full_scan = !compatible;
    let mut changed = Vec::new();
    for path in paths
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        if path == b"/" {
            full_scan = true;
            continue;
        }
        if path.len() > limits.max_path_bytes {
            return Err(Error::Limit("path bytes"));
        }
        if changed.len() >= limits.max_paths {
            return Err(Error::Limit("paths"));
        }

        let path_without_slash = path.strip_suffix(b"/").unwrap_or(path);
        if path_without_slash.split(|byte| *byte == b'/').any(|part| {
            part.is_empty() || part == b"." || part == b".." || part.eq_ignore_ascii_case(b".git")
        }) {
            return Err(Error::Response("unsafe changed path"));
        }

        changed.push(path.to_vec());
    }

    Ok(Changes {
        token,
        paths: changed,
        full_scan,
    })
}

#[cfg(test)]
mod tests;
