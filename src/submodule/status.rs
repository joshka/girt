use std::sync::atomic::{AtomicBool, Ordering};

use super::Ignore;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::{Modules, Source};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::index;
use crate::{ObjectId, Repository, filter, status};

/// A gitlink and its nested checkout, observed without modifying either repository.
///
/// Registration and checkout presence are independent. A populated checkout without an active
/// configuration can still make parent status dirty, while `git submodule status` prints `-`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Checkout {
    /// Parent-relative gitlink path bytes.
    pub path: Vec<u8>,
    /// Logical name from `.gitmodules`, if a definition maps this path.
    pub name: Option<Vec<u8>>,
    /// Stage-zero recorded commit, or a conflict stage's ID when `conflicted` is true.
    pub recorded: ObjectId,
    /// Nested HEAD identity; absent for an unpopulated checkout. Unborn populated HEAD is refused.
    pub head: Option<ObjectId>,
    /// Whether configuration marks this submodule active (URL presence is the fallback).
    pub registered: bool,
    /// The parent index has unresolved gitlink stages at this path.
    pub conflicted: bool,
    /// Effective dirtiness policy after local/non-local configuration overrides `.gitmodules`.
    pub ignore: Ignore,
    /// Visible tracked changes, including staged/unmerged content and nested submodules.
    pub tracked_dirty: bool,
    /// Visible untracked content after the nested repository's ignore rules.
    pub untracked_dirty: bool,
}

impl Checkout {
    /// Git's submodule-status prefix: `U`, `-`, `+`, or a space, in that precedence order.
    ///
    /// Dirtiness does not affect this prefix; use [`Self::is_modified`] for parent status.
    pub fn prefix(&self) -> char {
        if self.conflicted {
            'U'
        } else if !self.registered || self.head.is_none() {
            '-'
        } else if self.head != Some(self.recorded) {
            '+'
        } else {
            ' '
        }
    }

    /// Identity printed after [`Self::prefix`], including Git's zero sentinel for conflicts.
    ///
    /// Unregistered checkouts display the parent's recorded ID even if their HEAD differs.
    pub fn display_id(&self) -> ObjectId {
        match self.prefix() {
            'U' => ObjectId::null(self.recorded.format()),
            '-' => self.recorded,
            _ => self.head.unwrap_or(self.recorded),
        }
    }

    /// Whether configured parent status reports an unstaged modification at this gitlink.
    ///
    /// Unpopulated submodules are not deletions. Conflicts are reported separately by the parent.
    pub fn is_modified(&self) -> bool {
        !self.conflicted
            && self.ignore != Ignore::All
            && self.head.is_some()
            && (self.head != Some(self.recorded) || self.tracked_dirty || self.untracked_dirty)
    }
}

/// Lists gitlinks and inspects nested status with default bounds and filter commands refused.
///
/// Uses [`status_with`] with [`status::Limits::default`] and a flag that never cancels. This
/// reads the index and `.gitmodules` but never refreshes or writes them. See [`status_with`] for
/// configuration inheritance, platform support and filesystem trust requirements.
///
/// # Errors
///
/// Returns the errors of [`status_with`] except cancellation.
///
/// ```
/// use girt::index::{Entry, Mode};
/// use girt::{InitKind, ObjectFormat, ObjectId, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("parent"),
///     InitKind::Worktree,
/// )?;
/// let recorded: ObjectId = "1111111111111111111111111111111111111111".parse()?;
/// let mut index = repo.edit_index(Default::default())?;
/// index.replace_entries(vec![Entry::new(
///     b"vendor/math".to_vec(),
///     Mode::Gitlink,
///     recorded,
/// )])?;
/// index.commit()?;
/// std::fs::write(
///     repo.worktree().unwrap().join(".gitmodules"),
///     b"[submodule \"math\"]\npath = vendor/math\nurl = ../math.git\n",
/// )?;
/// let checkouts = girt::submodule::status(&repo)?;
/// assert_eq!(checkouts[0].prefix(), '-'); // Not initialized or cloned.
/// assert_eq!(checkouts[0].display_id(), recorded);
/// assert!(!checkouts[0].is_modified()); // An unpopulated submodule is not a deletion.
///
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn status(repository: &Repository) -> Result<Vec<Checkout>, StatusError> {
    status_with(
        repository,
        filter::Commands::Refuse,
        status::Limits::default(),
        &AtomicBool::new(false),
    )
}

/// Lists gitlinks and inspects their checkouts, with explicit filter trust and finite budgets.
///
/// `.gitmodules` uses [`Source::WorktreeOrIndex`]. Effective `submodule.<name>.ignore` overrides
/// that file; absent per-submodule policy falls back to `diff.ignoreSubmodules`, then `none`.
/// `all` suppresses parent changes, `dirty` skips content inspection, and `untracked` skips only
/// untracked content. HEAD is still inspected for listing, even under `all`.
///
/// Child configuration rereads exactly the parent's supplied non-local source layers and adds
/// the child's own local/worktree files. Conditional includes are reevaluated for the child;
/// parent-local settings and ambient process configuration are not inherited. Each child uses
/// its own attributes/filters and ignore rules. `commands` authorizes filter programs throughout
/// the nested scan. No update method, hook, fetch or other network operation runs.
///
/// The depth and aggregate gitlink count use `limits.max_submodule_depth` (capped at 64) and
/// `limits.max_submodules`; other status budgets apply separately to each repository. Cancellation
/// is checked between paths and by nested status. macOS/Linux support no-follow directory opens;
/// other platforms return an explicit unsupported error. Symlink ancestors and non-ASCII macOS
/// paths are refused. Repository metadata and directory replacement must be excluded during
/// inspection, as opening nested repositories is not a filesystem sandbox. Reports are not
/// atomic snapshots or overwrite authorization. Branch/tag description suffixes are not computed;
/// [`Checkout::prefix`] and [`Checkout::display_id`] expose the status line's structural values.
///
/// # Errors
///
/// Reports invalid metadata/configuration, ambiguous mappings, blocked paths, nested storage or
/// filter errors, cancellation and exhausted bounds. No partial list is returned.
pub fn status_with(
    repository: &Repository,
    commands: filter::Commands,
    limits: status::Limits,
    cancel: &AtomicBool,
) -> Result<Vec<Checkout>, StatusError> {
    check(cancel)?;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let index = repository
            .read_index(limits.index)
            .map_err(|e| at(b"index", e))?;
        let entries = index.as_ref().map_or(&[][..], |index| index.entries());
        let mut inspection = Inspection::new(
            commands,
            filter::Limits::default(),
            &crate::workflow::Environment::default(),
            limits,
            cancel,
        );
        let result = inspection.inspect(
            repository,
            entries
                .iter()
                .filter(|e| e.mode == index::Mode::Gitlink)
                .cloned()
                .collect(),
            0,
            true,
        )?;
        if !repository
            .index_unchanged(index.as_ref(), limits.index)
            .map_err(|e| at(b"index", e))?
        {
            return Err(at(b"index", status::Error::Changed(b"index".to_vec())));
        }
        Ok(result)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (repository, commands, limits);
        Err(StatusError::Unsupported)
    }
}

// Read policy before worktree scanning so ignore=all never reads replacement file content.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn ignored_gitlinks(
    repository: &Repository,
    entries: &[index::Entry],
    limits: status::Limits,
    paths: &crate::pathspec::Pathspec,
) -> Result<std::collections::HashSet<Vec<u8>>, StatusError> {
    let mut ignored = std::collections::HashSet::new();
    if !entries
        .iter()
        .any(|entry| entry.mode == index::Mode::Gitlink && paths.matches_unchecked(&entry.path))
    {
        return Ok(ignored);
    }
    let objects = repository
        .objects_with(limits.packs)
        .map_err(|e| at(b".gitmodules", e))?;
    let modules = Modules::read(repository, &objects, Source::WorktreeOrIndex)
        .map_err(|e| at(b".gitmodules", e))?;
    for entry in entries
        .iter()
        .filter(|entry| entry.mode == index::Mode::Gitlink && paths.matches_unchecked(&entry.path))
    {
        let mut definitions = modules
            .as_ref()
            .map_or(&[][..], |m| m.definitions())
            .iter()
            .filter(|definition| definition.path() == Some(entry.path.as_slice()));
        let definition = definitions.next();
        if definitions.next().is_some() {
            return Err(StatusError::Configuration {
                path: entry.path.clone(),
                reason: "multiple submodule definitions select one path",
            });
        }
        if effective_ignore(
            repository,
            definition.map(|d| d.name()),
            definition.map_or(Ignore::Unspecified, |d| d.ignore()),
            &entry.path,
        )? == Ignore::All
        {
            ignored.insert(entry.path.clone());
        }
    }
    Ok(ignored)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn complete_status(
    repository: &Repository,
    report: &mut status::Report,
    commands: filter::Commands,
    driver_limits: filter::Limits,
    environment: &crate::workflow::Environment,
    limits: status::Limits,
    cancel: &AtomicBool,
) -> Result<(), StatusError> {
    let mut inspection = Inspection::new(commands, driver_limits, environment, limits, cancel);
    inspection.complete(repository, report, 0)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Inspection<'a> {
    environment: crate::workflow::Environment,
    commands: filter::Commands,
    driver_limits: filter::Limits,
    limits: status::Limits,
    cancel: &'a AtomicBool,
    remaining: usize,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl<'a> Inspection<'a> {
    fn new(
        commands: filter::Commands,
        driver_limits: filter::Limits,
        environment: &crate::workflow::Environment,
        limits: status::Limits,
        cancel: &'a AtomicBool,
    ) -> Self {
        Self {
            environment: environment.clone(),
            commands,
            driver_limits,
            limits,
            cancel,
            remaining: limits.max_submodules,
        }
    }

    fn complete(
        &mut self,
        repository: &Repository,
        report: &mut status::Report,
        depth: usize,
    ) -> Result<(), StatusError> {
        let mut entries = report.unchecked_gitlinks.clone();
        entries.extend(
            report
                .unmerged
                .iter()
                .filter(|entry| entry.mode == index::Mode::Gitlink)
                .cloned(),
        );
        if entries.is_empty() {
            return Ok(());
        }
        report.submodules = self.inspect(repository, entries, depth, false)?;
        for checkout in &report.submodules {
            if checkout.is_modified() {
                let old = crate::TreeValue {
                    mode: crate::EntryMode::Gitlink,
                    id: checkout.recorded,
                };
                let new = crate::TreeValue {
                    mode: crate::EntryMode::Gitlink,
                    id: checkout.head.unwrap_or(checkout.recorded),
                };
                report.unstaged.push(status::WorktreeChange {
                    path: checkout.path.clone(),
                    index: old,
                    change: status::Change::Modified(new),
                });
            }
        }
        report.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
        report.unchecked_gitlinks.retain(|entry| {
            !report
                .submodules
                .iter()
                .any(|checkout| checkout.path == entry.path)
        });
        Ok(())
    }

    fn inspect(
        &mut self,
        repository: &Repository,
        mut entries: Vec<index::Entry>,
        depth: usize,
        include_ignored: bool,
    ) -> Result<Vec<Checkout>, StatusError> {
        check(self.cancel)?;
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let objects = repository
            .objects_with(self.limits.packs)
            .map_err(|e| at(b".gitmodules", e))?;
        let modules = Modules::read(repository, &objects, Source::WorktreeOrIndex)
            .map_err(|e| at(b".gitmodules", e))?;
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        let mut result = Vec::new();
        let mut previous = None;
        for entry in entries {
            check(self.cancel)?;
            if previous.as_deref() == Some(entry.path.as_slice()) {
                continue;
            }
            previous = Some(entry.path.clone());
            let definitions: Vec<_> = modules
                .as_ref()
                .map_or(&[][..], |modules| modules.definitions())
                .iter()
                .filter(|definition| definition.path() == Some(entry.path.as_slice()))
                .collect();
            if definitions.len() > 1 {
                return Err(StatusError::Configuration {
                    path: entry.path,
                    reason: "multiple submodule definitions select one path",
                });
            }
            let definition = definitions.first().copied();
            if definition.is_some_and(|d| !super::safe_relative_path(d.name())) {
                return Err(StatusError::Configuration {
                    path: entry.path,
                    reason: "unsafe logical submodule name",
                });
            }
            let name = definition.map(|d| d.name());
            let ignore = effective_ignore(
                repository,
                name,
                definition.map_or(Ignore::Unspecified, |d| d.ignore()),
                &entry.path,
            )?;
            if !include_ignored
                && (ignore == Ignore::All || entry.assume_valid || entry.skip_worktree)
            {
                continue;
            }
            self.remaining = self
                .remaining
                .checked_sub(1)
                .ok_or(StatusError::Limit("submodule paths"))?;
            let mut checkout = Checkout {
                path: entry.path.clone(),
                name: name.map(<[u8]>::to_vec),
                recorded: entry.id,
                head: None,
                registered: registered(repository, name, &entry.path)?,
                conflicted: entry.stage != index::Stage::Normal,
                ignore,
                tracked_dirty: false,
                untracked_dirty: false,
            };
            if !checkout.conflicted {
                self.inspect_checkout(repository, &mut checkout, depth)?;
            }
            result.push(checkout);
        }
        Ok(result)
    }

    fn inspect_checkout(
        &mut self,
        parent: &Repository,
        checkout: &mut Checkout,
        depth: usize,
    ) -> Result<(), StatusError> {
        let Some(path) = checkout_path(parent, &checkout.path)? else {
            return Ok(());
        };
        if depth >= self.limits.max_submodule_depth.min(64) {
            return Err(StatusError::Limit("submodule depth"));
        }
        let repository = Repository::open_with_config(&path, parent.inherited_config())
            .map_err(|error| at(&checkout.path, error))?;
        if repository.is_bare() {
            return Err(StatusError::Configuration {
                path: checkout.path.clone(),
                reason: "populated submodule is bare",
            });
        }
        checkout.head = repository.head().map_err(|e| at(&checkout.path, e))?.id;
        if checkout.head.is_none() {
            return Err(StatusError::Configuration {
                path: checkout.path.clone(),
                reason: "populated submodule HEAD is unborn",
            });
        }
        if matches!(checkout.ignore, Ignore::Dirty | Ignore::All) {
            return Ok(());
        }
        let mut environment = self.environment.clone();
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        ] {
            environment = environment.without(name);
        }
        if environment.get("HOME").is_none()
            && let Some(home) = &repository.inherited_config().context.home
        {
            environment = environment.with("HOME", home.as_os_str());
        }
        let mut filters = repository
            .filters(&environment, self.commands)
            .map_err(|e| at(&checkout.path, e))?;
        filters.set_driver_limits(self.driver_limits);
        let untracked = if checkout.ignore == Ignore::Untracked {
            status::UntrackedFiles::Omit
        } else {
            status::UntrackedFiles::All
        };
        let nested = crate::status::nested_status(
            &repository,
            untracked,
            &environment,
            &mut filters,
            self.limits,
            self.cancel,
            |nested| self.complete(&repository, nested, depth + 1),
        )
        .map_err(|e| at(&checkout.path, e))?;
        checkout.tracked_dirty = !nested.staged.is_empty()
            || !nested.unmerged.is_empty()
            || nested.unstaged.iter().any(|change| {
                nested
                    .submodules
                    .iter()
                    .find(|submodule| submodule.path == change.path)
                    .is_none_or(|submodule| {
                        submodule.head != Some(submodule.recorded) || submodule.tracked_dirty
                    })
            });
        checkout.untracked_dirty = checkout.ignore != Ignore::Untracked
            && (!nested.untracked.is_empty()
                || nested
                    .submodules
                    .iter()
                    .any(|submodule| submodule.untracked_dirty));
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn effective_ignore(
    repo: &Repository,
    name: Option<&[u8]>,
    module: Ignore,
    path: &[u8],
) -> Result<Ignore, StatusError> {
    if let Some(value) =
        name.and_then(|name| repo.config().value("submodule", Some(name), "ignore"))
    {
        return decode_ignore(value, path);
    }
    if module != Ignore::Unspecified {
        return Ok(module);
    }
    match repo.config().value("diff", None, "ignoreSubmodules") {
        Some(value) => decode_ignore(value, path),
        None => Ok(Ignore::None),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn decode_ignore(value: Option<&[u8]>, path: &[u8]) -> Result<Ignore, StatusError> {
    match value {
        Some(b"all") => Ok(Ignore::All),
        Some(b"dirty") => Ok(Ignore::Dirty),
        Some(b"untracked") => Ok(Ignore::Untracked),
        Some(b"none") => Ok(Ignore::None),
        _ => Err(StatusError::Configuration {
            path: path.to_vec(),
            reason: "invalid ignoreSubmodules/submodule ignore value",
        }),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn registered(repo: &Repository, name: Option<&[u8]>, path: &[u8]) -> Result<bool, StatusError> {
    let Some(name) = name else {
        return Ok(false);
    };
    if let Some(active) = repo
        .config()
        .boolean("submodule", Some(name), "active")
        .map_err(|e| at(path, e))?
    {
        return Ok(active);
    }
    let active: Vec<_> = repo.config().values("submodule", None, "active").collect();
    if !active.is_empty() {
        let values: Option<Vec<_>> = active.into_iter().collect();
        let values = values.ok_or_else(|| StatusError::Configuration {
            path: path.to_vec(),
            reason: "implicit submodule.active pathspec",
        })?;
        let paths = crate::pathspec::Pathspec::parse(values, &crate::pathspec::Context::new())
            .map_err(|e| at(path, e))?;
        return paths.matches(path).map_err(|e| at(path, e));
    }
    Ok(repo
        .config()
        .string("submodule", Some(name), "url")
        .is_some())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn checkout_path(
    repo: &Repository,
    path: &[u8],
) -> Result<Option<std::path::PathBuf>, StatusError> {
    use std::os::unix::ffi::OsStrExt;
    if !super::safe_relative_path(path) || (cfg!(target_os = "macos") && !path.is_ascii()) {
        return Err(StatusError::Configuration {
            path: path.to_vec(),
            reason: "unsafe or unsupported submodule path",
        });
    }
    let root = repo.worktree().ok_or(StatusError::NoWorktree)?;
    let mut fd = crate::status::worktree::open_root(root).map_err(|e| at(path, e))?;
    let mut native = root.to_owned();
    let mut components = path.split(|byte| *byte == b'/').peekable();
    while let Some(component) = components.next() {
        // Check exact spelling as well as no-follow opens on case-folding filesystems.
        let directory = std::fs::read_dir(&native).map_err(|source| {
            at(
                path,
                status::Error::Io {
                    path: path.to_vec(),
                    source,
                },
            )
        })?;
        let mut present = false;
        for entry in directory {
            let entry = entry.map_err(|source| {
                at(
                    path,
                    status::Error::Io {
                        path: path.to_vec(),
                        source,
                    },
                )
            })?;
            if entry.file_name().as_bytes() == component {
                present = true;
                break;
            }
        }
        if !present {
            return Ok(None);
        }
        if components.peek().is_none() {
            let stat = rustix::fs::statat(&fd, component, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
                .map_err(|source| {
                    at(
                        path,
                        status::Error::Io {
                            path: path.to_vec(),
                            source: source.into(),
                        },
                    )
                })?;
            if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                == rustix::fs::FileType::RegularFile
            {
                return Ok(None);
            }
        }
        fd = match rustix::fs::openat(
            &fd,
            component,
            crate::status::worktree::directory_flags(),
            rustix::fs::Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(error) => {
                return Err(at(
                    path,
                    crate::status::Error::Io {
                        path: path.to_vec(),
                        source: error.into(),
                    },
                ));
            }
        };
        native.push(std::ffi::OsStr::from_bytes(component));
    }
    // Require the nested marker; never discover upward into the parent repository.
    match std::fs::symlink_metadata(native.join(".git")) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StatusError::Configuration {
            path: path.to_vec(),
            reason: "symlinked submodule .git marker",
        }),
        Ok(_) => Ok(Some(native)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(at(
            path,
            crate::status::Error::Io {
                path: path.to_vec(),
                source,
            },
        )),
    }
}

fn check(cancel: &AtomicBool) -> Result<(), StatusError> {
    if cancel.load(Ordering::Relaxed) {
        Err(StatusError::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn at(path: &[u8], source: impl Into<crate::Error>) -> StatusError {
    StatusError::AtPath {
        path: path.to_vec(),
        source: source.into(),
    }
}

/// Submodule inspection failed without returning a partial list or modifying repository state.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StatusError {
    /// The caller requested cancellation.
    #[error("submodule status cancelled")]
    Cancelled,
    /// A recursive scan exhausted its aggregate path count or depth bound.
    #[error("submodule status limit exceeded: {0}")]
    Limit(&'static str),
    /// A worktree is required to inspect nested checkouts.
    #[error("submodule status requires a worktree")]
    NoWorktree,
    /// Native nested worktree inspection requires macOS/Linux.
    #[error("submodule checkout inspection requires macOS/Linux")]
    Unsupported,
    /// Configuration or path mapping cannot be interpreted safely.
    #[error("submodule configuration at {path:?}: {reason}")]
    Configuration {
        /// Parent-relative path bytes.
        path: Vec<u8>,
        /// Rejected condition.
        reason: &'static str,
    },
    /// A nested operation failed; the domain error remains available through `source`.
    #[error("submodule status at {path:?}")]
    AtPath {
        /// Relative path in the repository containing the failing operation.
        path: Vec<u8>,
        /// Original typed error, recoverable by downcasting.
        #[source]
        source: crate::Error,
    },
}
