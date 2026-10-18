//! `filter.<name>.*` drivers: configuration, trust policy and one-shot commands.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::{Config, InvalidValue};

/// Whether filter drivers may run the commands configured for them.
///
/// Running a driver executes `filter.<name>.clean`, `smudge` or `process` from the configuration
/// with the permissions of the current process. Git runs them for any repository it operates on;
/// those settings come from local, global or system configuration, never from a clone, but a
/// repository's `.gitattributes` decides which driver a path uses.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum Commands {
    /// Refuse a conversion that would run a command, with
    /// [`Error::Refused`](super::Error::Refused). Paths whose driver configures no command for
    /// the direction are converted without it, as Git does.
    #[default]
    Refuse,
    /// Run configured commands as Git does: through `sh -c` in the working tree root. A one-shot
    /// `clean` or `smudge` command gets `%f` replaced by the shell-quoted path and `%%` by `%`. A
    /// `process` command, which takes precedence, is started once per [`Pipeline`] and stopped
    /// when the pipeline is dropped.
    ///
    /// [`Pipeline`]: super::Pipeline
    Run,
}

/// Resource limits for running filter drivers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    max_output_bytes: usize,
}

impl Default for Limits {
    /// Allows 1 GiB of output from one command.
    fn default() -> Self {
        Self {
            max_output_bytes: 1 << 30,
        }
    }
}

impl Limits {
    /// The most bytes one driver may produce for one file.
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    /// Replaces [`Self::max_output_bytes`].
    #[must_use]
    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }
}

/// The direction a driver converts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Direction {
    /// `clean`: working-tree bytes to stored bytes.
    Clean,
    /// `smudge`: stored bytes to working-tree bytes.
    Smudge,
}

impl Direction {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Smudge => "smudge",
        }
    }
}

impl std::fmt::Display for Direction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.name())
    }
}

/// A driver command failed, or produced more than [`Limits::max_output_bytes`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DriverError {
    /// The command could not be started, or reading or writing its pipes failed.
    #[error("running {direction} filter {:?}", String::from_utf8_lossy(driver))]
    Io {
        /// The driver name.
        driver: Vec<u8>,
        /// The direction being run.
        direction: Direction,
        /// Operating-system cause.
        #[source]
        source: std::io::Error,
    },
    /// The command exited unsuccessfully.
    #[error(
        "{direction} filter {:?} failed: {status}",
        String::from_utf8_lossy(driver)
    )]
    Failed {
        /// The driver name.
        driver: Vec<u8>,
        /// The direction being run.
        direction: Direction,
        /// How the command exited.
        status: std::process::ExitStatus,
    },
    /// The command wrote more than [`Limits::max_output_bytes`].
    #[error(
        "{direction} filter {:?} exceeded the output limit",
        String::from_utf8_lossy(driver)
    )]
    TooLarge {
        /// The driver name.
        driver: Vec<u8>,
        /// The direction being run.
        direction: Direction,
    },
    /// The filter process answered `status=error` for this file.
    #[error(
        "{direction} filter {:?} reported an error",
        String::from_utf8_lossy(driver)
    )]
    Rejected {
        /// The driver name.
        driver: Vec<u8>,
        /// The direction being run.
        direction: Direction,
    },
    /// The filter process answered `status=abort`, or aborted earlier, and takes no more requests
    /// in this direction.
    #[error("{direction} filter {:?} aborted", String::from_utf8_lossy(driver))]
    Aborted {
        /// The driver name.
        driver: Vec<u8>,
        /// The direction being run.
        direction: Direction,
    },
    /// `filter.<name>.required` is set but no command is configured for the direction.
    #[error(
        "required {direction} filter {:?} has no command",
        String::from_utf8_lossy(driver)
    )]
    Missing {
        /// The driver name.
        driver: Vec<u8>,
        /// The direction being run.
        direction: Direction,
    },
}

/// One driver's `filter.<name>.*` values.
#[derive(Clone, Debug, Default)]
pub(super) struct Driver {
    pub(super) clean: Option<Vec<u8>>,
    pub(super) smudge: Option<Vec<u8>>,
    pub(super) process: Option<Vec<u8>>,
    pub(super) required: bool,
}

impl Driver {
    pub(super) fn command(&self, direction: Direction) -> Option<&[u8]> {
        match direction {
            Direction::Clean => self.clean.as_deref(),
            Direction::Smudge => self.smudge.as_deref(),
        }
    }
}

/// The `filter.<name>.*` drivers of a configuration and the policy for running their commands.
///
/// [`Drivers::default`] has no drivers: a `filter` attribute naming an unconfigured driver leaves
/// content unchanged, as in Git, unless the attribute names a driver Git would require.
///
/// ```
/// use girt::Config;
/// use girt::filter::{Commands, Drivers};
///
/// let config = Config::parse(b"[filter \"lfs\"]\nclean = git-lfs clean -- %f\nrequired\n")?;
/// let drivers = Drivers::from_config(&config)?.with_commands(Commands::Run);
/// assert_eq!(drivers.commands(), Commands::Run);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct Drivers {
    pub(super) drivers: HashMap<Vec<u8>, Driver>,
    pub(super) commands: Commands,
    pub(super) limits: Limits,
    pub(super) directory: Option<PathBuf>,
}

impl Default for Drivers {
    fn default() -> Self {
        Self {
            drivers: HashMap::new(),
            commands: Commands::Refuse,
            limits: Limits::default(),
            directory: None,
        }
    }
}

impl Drivers {
    /// Reads `filter.<name>.clean`, `smudge`, `process` and `required`, with [`Commands::Refuse`]
    /// and default [`Limits`]. An empty command configures none.
    ///
    /// # Errors
    ///
    /// Returns [`InvalidValue`] for a non-boolean `filter.<name>.required`.
    pub fn from_config(config: &Config) -> Result<Self, InvalidValue> {
        let mut drivers: HashMap<Vec<u8>, Driver> = HashMap::new();
        for entry in config.entries() {
            let Some(name) = entry
                .subsection
                .as_ref()
                .filter(|_| entry.section.eq_ignore_ascii_case(b"filter"))
            else {
                continue;
            };
            let value = entry.value.as_deref();
            let key = entry.name.to_ascii_lowercase();
            let driver = drivers.entry(name.clone()).or_default();
            // An implicit value configures no command, as an empty one does in Git.
            let command = || value.filter(|value| !value.is_empty()).map(<[u8]>::to_vec);
            match key.as_slice() {
                b"clean" => driver.clean = command(),
                b"smudge" => driver.smudge = command(),
                b"process" => driver.process = command(),
                b"required" => {
                    driver.required = crate::config::boolean(value)
                        .ok_or_else(|| InvalidValue::new("filter", "required", value))?;
                }
                _ => {}
            }
        }
        Ok(Self {
            drivers,
            ..Self::default()
        })
    }

    /// Whether configured commands may run.
    pub fn commands(&self) -> Commands {
        self.commands
    }

    /// Replaces [`Self::commands`].
    #[must_use]
    pub fn with_commands(mut self, commands: Commands) -> Self {
        self.commands = commands;
        self
    }

    /// The limits for each command.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Replaces [`Self::limits`].
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Runs commands in `directory`, the working tree root; without one they run in the current
    /// directory.
    #[must_use]
    pub fn with_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.directory = Some(directory.into());
        self
    }

    pub(super) fn get(&self, name: &[u8]) -> Option<&Driver> {
        self.drivers.get(name)
    }
}

/// Replaces `%f` with the shell-quoted `path` and `%%` with `%`.
pub(super) fn substitute(command: &[u8], path: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(command.len() + path.len() + 2);
    let mut bytes = command.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        match (byte, bytes.peek()) {
            (b'%', Some(b'f')) => {
                bytes.next();
                out.push(b'\'');
                for &byte in path {
                    if byte == b'\'' {
                        out.extend_from_slice(b"'\\''");
                    } else {
                        out.push(byte);
                    }
                }
                out.push(b'\'');
            }
            (b'%', Some(b'%')) => {
                bytes.next();
                out.push(b'%');
            }
            _ => out.push(byte),
        }
    }
    out
}

/// Runs `command` through the shell with `input` on stdin and returns its stdout.
pub(super) fn run(
    name: &[u8],
    direction: Direction,
    command: &[u8],
    path: &[u8],
    input: &[u8],
    drivers: &Drivers,
) -> Result<Vec<u8>, DriverError> {
    let io = |source| DriverError::Io {
        driver: name.to_vec(),
        direction,
        source,
    };
    let script = substitute(command, path);
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(os_string(script));
    if let Some(directory) = &drivers.directory {
        shell.current_dir(directory);
    }
    let mut child = shell
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(io)?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io(std::io::Error::other("no stdin")))?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| io(std::io::Error::other("no stdout")))?;
    let limit = drivers.limits.max_output_bytes;
    let (written, read) = std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            // A filter may exit without reading all of its input, as Git allows.
            match stdin.write_all(input) {
                Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                result => result,
            }
        });
        let mut output = Vec::new();
        let read = (&mut stdout)
            .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut output)
            .map(|_| output);
        if read.as_ref().is_ok_and(|output| output.len() > limit) {
            let _ = child.kill();
        }
        drop(stdout);
        (writer.join(), read)
    });
    let status = child.wait().map_err(io)?;
    let output = read.map_err(io)?;
    if output.len() > limit {
        return Err(DriverError::TooLarge {
            driver: name.to_vec(),
            direction,
        });
    }
    if !status.success() {
        return Err(DriverError::Failed {
            driver: name.to_vec(),
            direction,
            status,
        });
    }
    match written {
        Ok(result) => result.map_err(io)?,
        Err(_) => return Err(io(std::io::Error::other("stdin writer panicked"))),
    }
    Ok(output)
}

#[cfg(unix)]
pub(super) fn os_string(bytes: Vec<u8>) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    std::ffi::OsString::from_vec(bytes)
}

#[cfg(not(unix))]
pub(super) fn os_string(bytes: Vec<u8>) -> std::ffi::OsString {
    String::from_utf8_lossy(&bytes).into_owned().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitution_matches_git() {
        // Observed: `%% %f %x` with path `a b.p` reaches the shell as `% 'a b.p' %x`.
        assert_eq!(substitute(b"%% %f %x", b"a b.p"), b"% 'a b.p' %x");
        assert_eq!(substitute(b"cat %f", b"it's"), b"cat 'it'\\''s'");
    }
}
