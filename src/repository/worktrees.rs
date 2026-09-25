//! Read-only registered worktree inventory, independent of checkout availability.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, io};

use super::{OpenError, Repository, canonical, io_error, metadata_path, read, read_gitfile};

/// One linked registration, including damaged or inaccessible entries.
#[derive(Debug)]
pub struct Worktree {
    /// Absolute registration directory. This identity is retained even when inspection fails.
    pub git_dir: PathBuf,
    /// Checkout path from the backlink, when readable. May be absent or inaccessible.
    pub path: Option<PathBuf>,
    /// Observed availability; no repair or pruning is performed.
    pub state: WorktreeState,
}

/// Checkout availability at the instant it was inspected, not a lasting filesystem guarantee.
#[derive(Debug)]
pub enum WorktreeState {
    /// The backlink and forward gitfile agree and the checkout is a directory.
    Available,
    /// A required checkout path or forward gitfile is absent.
    Missing,
    /// Access failed; retains the original OS cause and path.
    Inaccessible(OpenError),
    /// Registration metadata or its relationship to the checkout is invalid.
    Invalid(OpenError),
}

/// Inventory-wide failure. Individual registration failures remain in [`Worktree::state`].
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    /// Could not enumerate the registrations directory.
    #[error(transparent)]
    Open(#[from] OpenError),
    /// More registrations exist than the caller permits.
    #[error("worktree registration limit exceeded")]
    Limit,
    /// Cancellation was observed between directory entries or inspections.
    #[error("worktree enumeration cancelled")]
    Cancelled,
}

impl Repository {
    /// Lists linked registrations in OS filename order, retaining unavailable checkouts.
    ///
    /// The main repository is not a linked registration and is excluded. Each entry retains its
    /// metadata directory so callers can open it to inspect private HEAD/config/index even when
    /// its checkout is missing. A damaged entry does not hide other entries. Directory symlinks
    /// are refused as invalid registrations. Paths preserve native OS semantics; no Unicode or
    /// case folding is performed. The default main handle is available through `common_dir()`.
    ///
    /// # Errors
    ///
    /// Directory I/O failures, `max_entries` exhaustion and cancellation return no partial list.
    /// Reads are synchronous and individual filesystem calls cannot be interrupted. Enumeration
    /// does not lock out moves/repair/pruning and represents observations, not a transaction.
    pub fn worktrees(
        &self,
        max_entries: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<Worktree>, WorktreeError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "repository.worktrees", outcome = "incomplete", failure_class = tracing::field::Empty, effects = tracing::field::Empty);
        let operation = || {
            check(cancel)?;
            let directory = self.common_dir.join("worktrees");
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
                Err(error) => return Err(io_error(&directory, error).into()),
            };
            let mut paths = Vec::new();
            for entry in entries {
                check(cancel)?;
                let entry = entry.map_err(|error| io_error(&directory, error))?;
                if paths.len() == max_entries {
                    return Err(WorktreeError::Limit);
                }
                paths.push(entry.path());
            }
            paths.sort();
            let mut result = Vec::with_capacity(paths.len());
            for git_dir in paths {
                check(cancel)?;
                let mut entry = Worktree {
                    git_dir,
                    path: None,
                    state: WorktreeState::Available,
                };
                entry.state = match inspect(&mut entry, &self.common_dir) {
                    Ok(()) => WorktreeState::Available,
                    Err(error) => match &error {
                        OpenError::Io { source, .. } => match source.kind() {
                            io::ErrorKind::NotFound => WorktreeState::Missing,
                            io::ErrorKind::NotADirectory | io::ErrorKind::IsADirectory => {
                                WorktreeState::Invalid(error)
                            }
                            _ => WorktreeState::Inaccessible(error),
                        },
                        _ => WorktreeState::Invalid(error),
                    },
                };
                result.push(entry);
            }
            Ok(result)
        };
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, |error| match error {
            WorktreeError::Open(_) => "io",
            WorktreeError::Limit => "limit",
            WorktreeError::Cancelled => "cancelled",
        });
        result
    }

    /// Opens a checkout and verifies that it shares this repository's common metadata identity.
    ///
    /// # Errors
    ///
    /// Returns opening errors or [`OpenError::Unrelated`] for a different common directory.
    /// Canonical filesystem paths determine identity; no files are changed.
    pub fn open_worktree(&self, path: impl AsRef<Path>) -> Result<Self, OpenError> {
        let repository = Self::open(path)?;
        if repository.common_dir != self.common_dir {
            return Err(OpenError::Unrelated(repository.common_dir));
        }
        Ok(repository)
    }
}

fn inspect(entry: &mut Worktree, common: &Path) -> Result<(), OpenError> {
    let metadata =
        fs::symlink_metadata(&entry.git_dir).map_err(|error| io_error(&entry.git_dir, error))?;
    if !metadata.is_dir() {
        return Err(super::malformed(
            &entry.git_dir,
            "registration is not a directory",
        ));
    }
    let backlink = entry.git_dir.join("gitdir");
    let target = entry
        .git_dir
        .join(metadata_path(&backlink, &read(&backlink)?)?);
    entry.path = target.parent().map(Path::to_path_buf);
    let repository = Repository::open(&entry.git_dir)?;
    if repository.common_dir() != common {
        return Err(OpenError::Unrelated(repository.common_dir().into()));
    }
    let checkout = target
        .parent()
        .ok_or_else(|| super::malformed(&backlink, "backlink has no parent"))?;
    let checkout_metadata = fs::metadata(checkout).map_err(|error| io_error(checkout, error))?;
    if !checkout_metadata.is_dir() {
        return Err(super::malformed(checkout, "checkout is not a directory"));
    }
    fs::metadata(&target).map_err(|error| io_error(&target, error))?;
    if read_gitfile(&target)? != canonical(&entry.git_dir)? {
        return Err(super::malformed(
            &backlink,
            "backlink points to another repository",
        ));
    }
    entry.path = target.parent().map(canonical).transpose()?;
    Ok(())
}

fn check(cancel: &AtomicBool) -> Result<(), WorktreeError> {
    if cancel.load(Ordering::Relaxed) {
        Err(WorktreeError::Cancelled)
    } else {
        Ok(())
    }
}
