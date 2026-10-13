//! Receive hooks run by a native local push, as `git-receive-pack` runs them.
//!
//! See githooks(5): `pre-receive` can reject the whole push, `update` can reject one reference,
//! and `post-receive`/`post-update` observe the result. Hook output is returned as progress
//! messages, which Git shows to the pushing user with a `remote:` prefix.

#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::cell::Cell;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod process;

use super::PushFailure;
use crate::refs::RefName;
use crate::transport::TransportControl;
use crate::{ObjectId, Repository};

/// One reference update as passed to hooks.
pub(super) struct HookUpdate<'a> {
    pub(super) old: ObjectId,
    pub(super) new: ObjectId,
    pub(super) name: &'a RefName,
}

/// The receive hooks of a destination repository.
pub(super) struct ReceiveHooks {
    directory: PathBuf,
    git_dir: PathBuf,
    push_options: Vec<Vec<u8>>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    output_remaining: Cell<usize>,
}

impl ReceiveHooks {
    /// Locates hooks in `core.hooksPath` (relative to the Git directory for a bare repository or
    /// the working tree otherwise) or the common `hooks` directory.
    pub(super) fn new(
        repository: &Repository,
        push_options: Vec<Vec<u8>>,
        output_limit: usize,
    ) -> Self {
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let _ = output_limit;
        let base = repository.worktree().unwrap_or(repository.git_dir());
        let directory = match repository.config().string("core", None, "hookspath") {
            Some(path) => {
                let path = PathBuf::from(String::from_utf8_lossy(path).into_owned());
                base.join(path)
            }
            None => repository.common_dir().join("hooks"),
        };
        Self {
            directory,
            git_dir: repository.git_dir().to_owned(),
            push_options,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            output_remaining: Cell::new(output_limit),
        }
    }

    /// Returns whether the named hook exists and is executable.
    pub(super) fn has(&self, name: &str) -> bool {
        self.path(name).is_some()
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        let path = self.directory.join(name);
        is_executable(&path).then_some(path)
    }

    /// Runs a hook if present, returning whether it succeeded (or is absent) and its output.
    pub(super) fn run(
        &self,
        name: &str,
        arguments: &[&[u8]],
        updates: &[HookUpdate<'_>],
        control: TransportControl<'_>,
        quarantine: Option<&Path>,
        primary: &Path,
    ) -> Result<(bool, Vec<u8>), PushFailure> {
        control.check()?;
        let Some(path) = self.path(name) else {
            return Ok((true, Vec::new()));
        };
        let mut command = Command::new(path);
        command
            .args(arguments.iter().map(|argument| os_string(argument)))
            .current_dir(&self.git_dir)
            .env("GIT_DIR", &self.git_dir)
            .env("GIT_PUSH_OPTION_COUNT", self.push_options.len().to_string())
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_QUARANTINE_PATH");
        if let Some(directory) = quarantine {
            command
                .env("GIT_OBJECT_DIRECTORY", directory)
                .env("GIT_QUARANTINE_PATH", directory)
                .env(
                    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
                    super::quarantine::quoted_path(primary),
                );
        }
        for (index, option) in self.push_options.iter().enumerate() {
            command.env(format!("GIT_PUSH_OPTION_{index}"), os_string(option));
        }
        let mut input = Vec::new();
        for update in updates {
            input.extend_from_slice(format!("{} {} ", update.old, update.new).as_bytes());
            input.extend_from_slice(update.name.as_bytes());
            input.push(b'\n');
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            let output = process::run(&mut command, &input, control, self.output_remaining.get())
                .map_err(|error| {
                if error.kind() == std::io::ErrorKind::FileTooLarge {
                    PushFailure::Limit("receive hook output")
                } else {
                    error.into()
                }
            })?;
            self.output_remaining
                .set(self.output_remaining.get() - output.1.len());
            Ok(output)
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (command, input);
            Err(PushFailure::Unsupported(
                "receive hook execution on this platform",
            ))
        }
    }
}

#[cfg(unix)]
fn os_string(bytes: &[u8]) -> OsString {
    use std::os::unix::ffi::OsStringExt as _;
    OsString::from_vec(bytes.to_vec())
}

#[cfg(not(unix))]
fn os_string(bytes: &[u8]) -> OsString {
    String::from_utf8_lossy(bytes).into_owned().into()
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
