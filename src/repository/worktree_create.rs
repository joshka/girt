//! Exclusive registration of an unborn linked worktree.
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use super::{Repository, RepositoryLocation, RepositoryMetadata, WorktreeLinkStyle};
use crate::SharedPermissions;
use crate::refs::{Backend, RefName, References, Target};

/// Selected file synchronization during orphan-worktree creation.
///
/// Uses full file flush on macOS and `File::sync_all` elsewhere. Directory entries are not
/// synchronized; success is not a power-loss durability guarantee.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorktreeDurability {
    /// Synchronize the selected index lock before publication.
    pub index: bool,
    /// Synchronize private reftable files and reference placeholders. Files-backend unborn HEAD
    /// creation does not perform reference synchronization, matching the qualified operation.
    pub references: bool,
}

/// Explicit policy captured by the caller before creating an orphan worktree.
///
/// No environment or effective configuration is read for these fields. The caller owns source
/// selection and any transformation of private configuration. Defaults preserve the existing
/// creation API's private-index, umask and unsynchronized behavior.
#[derive(Clone, Debug, Default)]
pub struct OrphanWorktreeOptions {
    /// Link spelling for this operation; does not enable repository extensions.
    pub link_style: WorktreeLinkStyle,
    /// Selected index path, or the new registration's private `index` when absent.
    /// Relative overrides resolve against the process current directory.
    pub index_path: Option<PathBuf>,
    /// Limits for reading and replacing the selected index.
    pub index_limits: crate::index::Limits,
    /// Captured, transformed contents for the new `config.worktree`, if any.
    pub private_config: Option<Vec<u8>>,
    /// Mode adjustment for shared metadata; registration and link files retain ordinary umask.
    pub shared_permissions: SharedPermissions,
    /// Selected file synchronization before completion.
    pub durability: WorktreeDurability,
}

/// A refusal or partial creation failure. A registered directory in an error is retained.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CreateWorktreeError {
    /// The branch is not a full heads reference.
    #[error("invalid orphan branch name")]
    Branch,
    /// The requested orphan branch already has a stored value.
    #[error("branch already exists")]
    ExistingBranch,
    /// The destination exists or lacks a usable filename.
    #[error("worktree destination already exists or has no filename: {0}")]
    Destination(PathBuf),
    /// The path cannot be written unambiguously in Git's line-based metadata.
    #[error("unsupported worktree metadata path: {0}")]
    Path(PathBuf),
    /// No free registration name was found within the caller's bound.
    #[error("worktree registration name limit exceeded")]
    Limit,
    /// Reference inspection failed before creation.
    #[error(transparent)]
    References(#[from] crate::refs::ReferenceError),
    /// Existing repository configuration could not be interpreted.
    #[error(transparent)]
    Configuration(#[from] super::OpenError),
    /// Empty index encoding failed before creation.
    #[error(transparent)]
    Index(#[from] crate::index::Error),
    /// Selected index read, lock or publication failed. Partial registration is retained.
    #[error("cannot initialize worktree index: {source}")]
    IndexStorage {
        /// Reserved registration, if any.
        registration: Option<PathBuf>,
        /// Original guarded-storage error, including cleanup failures.
        #[source]
        source: crate::index::StorageError,
    },
    /// Creation failed and the selected index lock could not be cleaned up safely.
    #[error("{operation}; index lock cleanup also failed: {cleanup}")]
    Cleanup {
        /// Original creation failure.
        #[source]
        operation: Box<CreateWorktreeError>,
        /// Cleanup failure; a replacement lock is never removed.
        cleanup: crate::index::StorageError,
    },
    /// Private reftable encoding failed before creation.
    #[error(transparent)]
    Reftable(#[from] crate::refs::reftable::Error),
    /// Filesystem failure; registration identifies retained partial state, if any.
    #[error("cannot create {path}: {source}")]
    Io {
        /// Failed path.
        path: PathBuf,
        /// Reserved registration, if any.
        registration: Option<PathBuf>,
        /// OS cause.
        #[source]
        source: io::Error,
    },
    /// A created reference file could not be synchronized. The file and registration remain.
    #[error("created worktree file {path} could not be synchronized: {source}")]
    Synchronize {
        /// File whose contents were written but whose requested synchronization failed.
        path: PathBuf,
        /// Registration retained for inspection.
        registration: PathBuf,
        /// Synchronization cause.
        #[source]
        source: io::Error,
    },
    /// Written metadata could not be reopened.
    #[error("created worktree metadata failed validation: {source}")]
    Open {
        /// Registration retained for inspection.
        registration: PathBuf,
        /// Opening failure.
        #[source]
        source: Box<super::OpenError>,
    },
}

struct SelectedIndexAlias {
    path: PathBuf,
    target: PathBuf,
    spelling: PathBuf,
    link_metadata: fs::Metadata,
    target_metadata: Option<fs::Metadata>,
}

impl SelectedIndexAlias {
    fn capture(path: &Path) -> Result<Option<Self>, CreateWorktreeError> {
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()
                .map_err(|source| io_error(path, None, source))?
                .join(path)
        };
        let link_metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => metadata,
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(io_error(&path, None, error)),
        };
        #[cfg(not(unix))]
        return Err(CreateWorktreeError::IndexStorage {
            registration: None,
            source: crate::index::StorageError::NotRegular(path),
        });
        #[cfg(unix)]
        {
            let spelling = fs::read_link(&path).map_err(|source| io_error(&path, None, source))?;
            let linked = if spelling.is_absolute() {
                spelling.clone()
            } else {
                path.parent().expect("absolute path").join(&spelling)
            };
            let name = linked
                .file_name()
                .ok_or_else(|| CreateWorktreeError::Path(linked.clone()))?;
            let parent = fs::canonicalize(linked.parent().expect("path with filename"))
                .map_err(|source| io_error(&linked, None, source))?;
            let target = parent.join(name);
            let target_metadata = match fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.is_file() => Some(metadata),
                Ok(_) => {
                    return Err(CreateWorktreeError::IndexStorage {
                        registration: None,
                        source: crate::index::StorageError::NotRegular(target),
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(io_error(&target, None, error)),
            };
            Ok(Some(Self {
                path,
                target,
                spelling,
                link_metadata,
                target_metadata,
            }))
        }
    }

    fn check(&self, registration: Option<&Path>) -> Result<(), CreateWorktreeError> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let current_link = fs::symlink_metadata(&self.path).ok();
            let current_target = fs::symlink_metadata(&self.target).ok();
            let same_identity = |left: &fs::Metadata, right: &fs::Metadata| {
                left.file_type() == right.file_type()
                    && left.dev() == right.dev()
                    && left.ino() == right.ino()
            };
            let link_unchanged = current_link
                .as_ref()
                .is_some_and(|current| same_identity(&self.link_metadata, current))
                && fs::read_link(&self.path).ok().as_ref() == Some(&self.spelling);
            let target_unchanged = match (&self.target_metadata, current_target.as_ref()) {
                (None, None) => true,
                (Some(before), Some(after)) => same_identity(before, after),
                _ => false,
            };
            if link_unchanged && target_unchanged {
                return Ok(());
            }
        }
        Err(CreateWorktreeError::IndexStorage {
            registration: registration.map(Path::to_owned),
            source: crate::index::StorageError::Changed(self.path.clone()),
        })
    }
}

impl Repository {
    /// Registers an empty linked worktree with an unborn branch and private empty index.
    ///
    /// The branch must be a full heads name without a stored value. The destination must be
    /// absent and its parent must exist. A destination filename supplies the registration name;
    /// numeric suffixes resolve collisions within max_names attempts. Multiple worktrees may
    /// point at the same unborn branch, as Git permits. The configured relativeWorktrees
    /// extension selects relative forward and back links. No checkout files, initial commit,
    /// shared branch value, or jj workspace metadata are created.
    ///
    /// The caller must exclude competing branch/worktree administration and path replacement.
    /// Failures may retain a registration or checkout directory for inspection. There is no
    /// automatic rollback, fsync or crash-durability guarantee. Parent symlinks are followed;
    /// this is not a security boundary for untrusted paths.
    ///
    /// # Errors
    ///
    /// Branch and destination refusals precede creation. Exhausted names can leave a newly
    /// created registrations directory. Later errors retain the failed path and reservation.
    pub fn create_orphan_worktree(
        &self,
        destination: impl AsRef<Path>,
        branch: &RefName,
        max_names: usize,
    ) -> Result<Self, CreateWorktreeError> {
        self.create_orphan_worktree_with_link_style(
            destination,
            branch,
            max_names,
            WorktreeLinkStyle::Configured,
        )
    }

    /// Registers an unborn worktree with an explicit link spelling for this operation.
    ///
    /// The same creation, exclusion and partial-failure contract as
    /// [`Self::create_orphan_worktree`] applies. The direct repository configuration is still
    /// validated; selecting a style does not persist a configuration change.
    pub fn create_orphan_worktree_with_link_style(
        &self,
        destination: impl AsRef<Path>,
        branch: &RefName,
        max_names: usize,
        link_style: WorktreeLinkStyle,
    ) -> Result<Self, CreateWorktreeError> {
        self.create_orphan_worktree_with_options(
            destination,
            branch,
            max_names,
            OrphanWorktreeOptions {
                link_style,
                ..Default::default()
            },
        )
    }

    /// Creates an orphan worktree with caller-captured index, private-config and file policies.
    ///
    /// Uses the existing branch, path, link and exclusion rules. The selected index is read under
    /// its ordinary lock and replaced with an empty standalone draft; missing shared dependencies
    /// fail before registration or destination creation. A selected symlink to a regular or
    /// missing file retains its leaf and locks and publishes the referent; nested leaf symlinks
    /// are refused. The source default index is ignored.
    /// Private configuration bytes are copied, not reread or transformed. No hook is run.
    ///
    /// Shared permissions affect the worktrees root, refs/reftable directories, HEAD, selected
    /// index and private table files. Registration, link and private-config modes retain umask.
    /// Permission validation precedes creation; this is not Git's exact partial-effect timing.
    /// Non-Unix hosts currently support only the umask permission policy.
    ///
    /// # Errors
    ///
    /// Returns existing creation refusals, guarded index errors and explicit permission/sync
    /// failures. Later errors retain the registration and any completed files for inspection;
    /// there is no rollback or fallback. File sync does not make directory entries crash durable.
    ///
    /// ```no_run
    /// use girt::refs::RefName;
    /// use girt::{OrphanWorktreeOptions, Repository, SharedPermissions, WorktreeDurability};
    /// let repository = Repository::open("main")?;
    /// let options = OrphanWorktreeOptions {
    ///     shared_permissions: SharedPermissions::Group,
    ///     durability: WorktreeDurability {
    ///         index: true,
    ///         references: true,
    ///     },
    ///     ..Default::default()
    /// };
    /// let branch = RefName::new(b"refs/heads/new-work")?;
    /// let linked = repository.create_orphan_worktree_with_options("linked", &branch, 128, options)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn create_orphan_worktree_with_options(
        &self,
        destination: impl AsRef<Path>,
        branch: &RefName,
        max_names: usize,
        options: OrphanWorktreeOptions,
    ) -> Result<Self, CreateWorktreeError> {
        let location = create_orphan_worktree(
            self.references()?,
            destination.as_ref(),
            branch,
            max_names,
            options,
        )?;
        let registration = location.git_dir().to_path_buf();
        location
            .open_with_config(&crate::config::ConfigInputs::default())
            .map_err(|source| CreateWorktreeError::Open {
                registration,
                source: Box::new(source),
            })
    }
}

impl RepositoryMetadata {
    /// Registers an unborn worktree without opening objects or reading shallow history.
    ///
    /// Uses the policies and exclusion requirements of
    /// [`Repository::create_orphan_worktree_with_options`]. The source's default index is ignored.
    /// An explicitly selected index is locked and validated before any registration or destination
    /// is created. Relevant references and direct repository configuration are still validated.
    /// The returned location can be inspected as metadata or opened as a full repository.
    ///
    /// # Errors
    ///
    /// Returns creation and index errors, including explicit lock cleanup failures. Later failures
    /// retain completed files and registration; no rollback or shallow validation is performed.
    ///
    /// ```no_run
    /// # use girt::{config::ConfigInputs, RepositoryLocation, OrphanWorktreeOptions, refs::RefName};
    /// let location = RepositoryLocation::at_git_dir("main/.git")?;
    /// let metadata = location.read_metadata_with_config(&ConfigInputs::default())?;
    /// let branch = RefName::new(b"refs/heads/new-work")?;
    /// let linked = metadata.create_orphan_worktree_with_options(
    ///     "linked",
    ///     &branch,
    ///     128,
    ///     OrphanWorktreeOptions::default(),
    /// )?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn create_orphan_worktree_with_options(
        &self,
        destination: impl AsRef<Path>,
        branch: &RefName,
        max_names: usize,
        options: OrphanWorktreeOptions,
    ) -> Result<RepositoryLocation, CreateWorktreeError> {
        let references = References::from_layout(
            self.git_dir(),
            self.common_dir(),
            self.object_format(),
            self.reference_backend(),
        )?;
        create_orphan_worktree(references, destination.as_ref(), branch, max_names, options)
    }
}

fn create_orphan_worktree(
    references: References<'_>,
    destination: &Path,
    branch: &RefName,
    max_names: usize,
    options: OrphanWorktreeOptions,
) -> Result<RepositoryLocation, CreateWorktreeError> {
    options
        .shared_permissions
        .validate()
        .map_err(|source| io_error(destination, None, source))?;
    if max_names == 0 {
        return Err(CreateWorktreeError::Limit);
    }
    if !branch.as_bytes().starts_with(b"refs/heads/") {
        return Err(CreateWorktreeError::Branch);
    }
    if references.read(branch)?.is_some() {
        return Err(CreateWorktreeError::ExistingBranch);
    }

    let name = destination
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| CreateWorktreeError::Destination(destination.into()))?;
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|e| io_error(parent, None, e))?;
    let destination = parent.join(name);
    validate_path(&destination)?;
    validate_path(references.common_dir)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(CreateWorktreeError::Destination(destination)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => (),
        Err(e) => return Err(io_error(&destination, None, e)),
    }
    let table = if references.reference_backend == Backend::Reftable {
        Some(
            initial_head_table(references.object_format, branch)
                .encode(crate::refs::reftable::Limits::default())?,
        )
    } else {
        None
    };
    let config_path = references.common_dir.join("config");
    let bytes = super::read_config(&config_path, crate::config::ResolveLimits::default().bytes)?;
    let config = crate::Config::parse(&bytes).map_err(|source| super::OpenError::Config {
        path: config_path.clone(),
        source,
    })?;
    let configured_relative = super::extension_boolean(&config, &config_path, "relativeworktrees")?;
    let relative = options.link_style.uses_relative(configured_relative);
    let selected_alias = options
        .index_path
        .as_deref()
        .map(SelectedIndexAlias::capture)
        .transpose()?
        .flatten();
    let selected_index = selected_alias
        .as_ref()
        .map(|alias| &alias.target)
        .or(options.index_path.as_ref());
    let selected_missing = selected_index
        .map(|path| match fs::symlink_metadata(path) {
            Ok(_) => Ok(false),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
            Err(error) => Err(io_error(path, None, error)),
        })
        .transpose()?;
    let mut index_edit = selected_index
        .map(|path| {
            crate::index::IndexEdit::acquire(
                references.object_format,
                path.clone(),
                options.index_limits,
                crate::index::EditOptions {
                    resolve_split: true,
                    follow_symlink: false,
                },
            )
            .map_err(|source| CreateWorktreeError::IndexStorage {
                registration: None,
                source,
            })
        })
        .transpose()?;
    if let (Some(missing), Some(edit)) = (selected_missing, index_edit.as_ref())
        && missing != edit.original_missing()
    {
        let path = selected_index.expect("selected index path").clone();
        let error = CreateWorktreeError::IndexStorage {
            registration: None,
            source: crate::index::StorageError::Changed(path),
        };
        return finish_creation(Err(error), index_edit);
    }
    if let Some(alias) = &selected_alias
        && let Err(error) = alias.check(None)
    {
        return finish_creation(Err(error), index_edit);
    }
    let version = match index_edit.as_ref().map(|edit| edit.index().version()) {
        Some(crate::index::Version::V4) => crate::index::Version::V4,
        _ => crate::index::Version::V2,
    };
    let empty_index = crate::index::Index::empty_for_orphan(references.object_format, version);
    if let Err(error) = empty_index.encode(options.index_limits) {
        return finish_creation(Err(CreateWorktreeError::Index(error)), index_edit);
    }
    let result = (|| {
        let registrations = references.common_dir.join("worktrees");
        fs::create_dir_all(&registrations).map_err(|e| io_error(&registrations, None, e))?;
        options
            .shared_permissions
            .apply_directory(&registrations)
            .map_err(|source| io_error(&registrations, None, source))?;
        let registration = reserve_name(&registrations, name, max_names)?;
        let forward = if relative {
            relative_path(&destination, &registration)
        } else {
            registration.clone()
        };
        let backlink = if relative {
            relative_path(&registration, &destination.join(".git"))
        } else {
            destination.join(".git")
        };
        write_file(&registration.join("commondir"), b"../..\n", &registration)?;
        write_file(
            &registration.join("gitdir"),
            &path_line(&backlink),
            &registration,
        )?;
        let refs = registration.join("refs");
        create_dir(&refs, &registration)?;
        options
            .shared_permissions
            .apply_directory(&refs)
            .map_err(|source| io_error(&refs, Some(&registration), source))?;
        let reference_sync = table.is_some() && options.durability.references;
        if let Some(table) = table {
            let tables = registration.join("reftable");
            create_dir(&tables, &registration)?;
            options
                .shared_permissions
                .apply_directory(&tables)
                .map_err(|source| io_error(&tables, Some(&registration), source))?;
            write_shared_file(
                &registration.join("reftable/initial.ref"),
                &table,
                &registration,
                options.shared_permissions,
                reference_sync,
            )?;
            write_shared_file(
                &registration.join("reftable/tables.list"),
                b"initial.ref\n",
                &registration,
                options.shared_permissions,
                reference_sync,
            )?;
            write_shared_file(
                &registration.join("refs/heads"),
                b"this repository uses the reftable format\n",
                &registration,
                options.shared_permissions,
                reference_sync,
            )?;
            write_shared_file(
                &registration.join("HEAD"),
                b"ref: refs/heads/.invalid\n",
                &registration,
                options.shared_permissions,
                reference_sync,
            )?;
        } else {
            let mut head = b"ref: ".to_vec();
            head.extend_from_slice(branch.as_bytes());
            head.push(b'\n');
            write_shared_file(
                &registration.join("HEAD"),
                &head,
                &registration,
                options.shared_permissions,
                false,
            )?;
        }
        if let Some(bytes) = &options.private_config {
            write_file(&registration.join("config.worktree"), bytes, &registration)?;
        }
        if let Some(alias) = &selected_alias {
            alias.check(Some(&registration))?;
        }
        if index_edit.is_none() {
            index_edit = Some(
                crate::index::IndexEdit::acquire(
                    references.object_format,
                    registration.join("index"),
                    options.index_limits,
                    crate::index::EditOptions {
                        resolve_split: true,
                        follow_symlink: true,
                    },
                )
                .map_err(|source| CreateWorktreeError::IndexStorage {
                    registration: Some(registration.clone()),
                    source,
                })?,
            );
        }
        index_edit
            .as_mut()
            .expect("selected or private index acquired")
            .replace_index(empty_index)?;
        let edit = index_edit
            .take()
            .expect("selected or private index acquired");
        let commit_options = crate::index::IndexCommitOptions {
            shared_permissions: options.shared_permissions,
            sync: options.durability.index,
        };
        let publication = if selected_missing == Some(true) {
            edit.commit_new_with_options(commit_options)
        } else {
            edit.commit_with_options(commit_options)
        };
        publication.map_err(|source| CreateWorktreeError::IndexStorage {
            registration: Some(registration.clone()),
            source,
        })?;
        create_dir(&destination, &registration)?;
        write_file(
            &destination.join(".git"),
            &gitfile_line(&forward),
            &registration,
        )?;
        RepositoryLocation::at_git_dir(destination.join(".git")).map_err(|source| {
            CreateWorktreeError::Open {
                registration: registration.clone(),
                source: Box::new(source),
            }
        })
    })();
    finish_creation(result, index_edit)
}

fn finish_creation(
    result: Result<RepositoryLocation, CreateWorktreeError>,
    index_edit: Option<crate::index::IndexEdit>,
) -> Result<RepositoryLocation, CreateWorktreeError> {
    match (result, index_edit) {
        (Err(operation), Some(edit)) => match edit.abort() {
            Ok(()) => Err(operation),
            Err(cleanup) => Err(CreateWorktreeError::Cleanup {
                operation: Box::new(operation),
                cleanup,
            }),
        },
        (result, None) => result,
        (Ok(_), Some(_)) => unreachable!("successful creation publishes the selected index"),
    }
}

fn reserve_name(
    root: &Path,
    name: &OsStr,
    max_names: usize,
) -> Result<PathBuf, CreateWorktreeError> {
    for suffix in 0..max_names {
        let mut candidate = OsString::from(name);
        if suffix != 0 {
            candidate.push(suffix.to_string());
        }
        let path = root.join(candidate);
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(io_error(&path, None, e)),
        }
    }
    Err(CreateWorktreeError::Limit)
}

fn initial_head_table(
    format: crate::ObjectFormat,
    branch: &RefName,
) -> crate::refs::reftable::Table {
    use crate::refs::reftable::{RefRecord, Table};
    Table {
        format,
        min_update_index: 1,
        max_update_index: 1,
        references: vec![RefRecord {
            name: RefName::new(b"HEAD").expect("fixed name").into(),
            update_index: 1,
            target: Some(Target::Symbolic(branch.clone())),
            peeled: None,
        }],
        logs: Vec::new(),
    }
}

pub(super) fn relative_path(from: &Path, to: &Path) -> PathBuf {
    let absolute = to.to_path_buf();
    let from: Vec<Component<'_>> = from.components().collect();
    let to: Vec<Component<'_>> = to.components().collect();
    if from.first() != to.first() {
        return absolute;
    }
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut result = PathBuf::new();
    for _ in shared..from.len() {
        result.push("..");
    }
    for component in &to[shared..] {
        result.push(component.as_os_str());
    }
    result
}

pub(super) fn validate_path(path: &Path) -> Result<(), CreateWorktreeError> {
    #[cfg(unix)]
    let invalid = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().contains(&b'\n') || path.as_os_str().as_bytes().contains(&b'\r')
    };
    #[cfg(not(unix))]
    let invalid = path.to_str().is_none()
        || path.to_string_lossy().as_bytes().contains(&b'\n')
        || path.to_string_lossy().as_bytes().contains(&b'\r');
    if invalid {
        Err(CreateWorktreeError::Path(path.into()))
    } else {
        Ok(())
    }
}

pub(super) fn path_line(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    let mut bytes = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let mut bytes = {
        let spelling = path.to_str().expect("validated metadata path");
        // Git treats the verbatim prefix returned by Windows canonicalization as nonlocal.
        let spelling = if let Some(unc) = spelling.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else {
            spelling
                .strip_prefix(r"\\?\")
                .unwrap_or(spelling)
                .to_owned()
        };
        spelling.into_bytes()
    };
    #[cfg(not(any(unix, windows)))]
    let mut bytes = path
        .to_str()
        .expect("validated metadata path")
        .as_bytes()
        .to_vec();
    bytes.push(b'\n');
    bytes
}

pub(super) fn gitfile_line(path: &Path) -> Vec<u8> {
    let mut bytes = b"gitdir: ".to_vec();
    bytes.extend_from_slice(&path_line(path));
    bytes
}

fn create_dir(path: &Path, registration: &Path) -> Result<(), CreateWorktreeError> {
    fs::create_dir(path).map_err(|e| io_error(path, Some(registration), e))
}

fn write_file(path: &Path, bytes: &[u8], registration: &Path) -> Result<(), CreateWorktreeError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io_error(path, Some(registration), e))?;
    file.write_all(bytes)
        .map_err(|e| io_error(path, Some(registration), e))
}

fn io_error(path: &Path, registration: Option<&Path>, source: io::Error) -> CreateWorktreeError {
    CreateWorktreeError::Io {
        path: path.into(),
        registration: registration.map(Path::to_path_buf),
        source,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn gitfile_paths_drop_windows_verbatim_prefixes() {
        assert_eq!(
            path_line(Path::new(r"\\?\C:\repo\worktrees\one")),
            b"C:\\repo\\worktrees\\one\n"
        );
        assert_eq!(
            path_line(Path::new(r"\\?\UNC\server\share\repo")),
            b"\\\\server\\share\\repo\n"
        );
    }
}

fn write_shared_file(
    path: &Path,
    bytes: &[u8],
    registration: &Path,
    permissions: SharedPermissions,
    sync: bool,
) -> Result<(), CreateWorktreeError> {
    write_shared_file_with_sync(
        path,
        bytes,
        registration,
        permissions,
        sync,
        crate::file_policy::sync_file,
    )
}

fn write_shared_file_with_sync(
    path: &Path,
    bytes: &[u8],
    registration: &Path,
    permissions: SharedPermissions,
    sync: bool,
    synchronize: impl FnOnce(&std::fs::File) -> io::Result<()>,
) -> Result<(), CreateWorktreeError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|source| io_error(path, Some(registration), source))?;
    file.write_all(bytes)
        .map_err(|source| io_error(path, Some(registration), source))?;
    permissions
        .apply_file(&file)
        .map_err(|source| io_error(path, Some(registration), source))?;
    if sync {
        synchronize(&file).map_err(|source| CreateWorktreeError::Synchronize {
            path: path.into(),
            registration: registration.into(),
            source,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod policy_tests {
    use rstest::rstest;

    use super::*;

    #[cfg(unix)]
    #[test]
    fn selected_alias_rejects_retarget_and_replaced_referent() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected");
        let first = root.path().join("first");
        let second = root.path().join("second");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();
        std::os::unix::fs::symlink("first", &selected).unwrap();
        let alias = SelectedIndexAlias::capture(&selected).unwrap().unwrap();
        assert!(alias.check(None).is_ok());

        fs::remove_file(&selected).unwrap();
        std::os::unix::fs::symlink("second", &selected).unwrap();
        assert!(matches!(
            alias.check(None),
            Err(CreateWorktreeError::IndexStorage {
                registration: None,
                source: crate::index::StorageError::Changed(_),
            })
        ));

        fs::remove_file(&selected).unwrap();
        std::os::unix::fs::symlink("first", &selected).unwrap();
        let alias = SelectedIndexAlias::capture(&selected).unwrap().unwrap();
        fs::write(root.path().join("replacement"), b"replacement").unwrap();
        fs::rename(root.path().join("replacement"), &first).unwrap();
        assert!(matches!(
            alias.check(None),
            Err(CreateWorktreeError::IndexStorage {
                registration: None,
                source: crate::index::StorageError::Changed(_),
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn dangling_selected_alias_rejects_new_referent_before_creation() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected");
        std::os::unix::fs::symlink("referent", &selected).unwrap();
        let alias = SelectedIndexAlias::capture(&selected).unwrap().unwrap();
        fs::write(root.path().join("referent"), b"foreign").unwrap();
        assert!(matches!(
            alias.check(None),
            Err(CreateWorktreeError::IndexStorage {
                registration: None,
                source: crate::index::StorageError::Changed(_),
            })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn failed_creation_reports_index_cleanup_failure() {
        let root = tempfile::tempdir().unwrap();
        let edit = crate::index::IndexEdit::acquire(
            crate::ObjectFormat::Sha1,
            root.path().join("selected"),
            crate::index::Limits::default(),
            crate::index::EditOptions::default(),
        )
        .unwrap();
        // Abort must preserve a replacement lock and report both errors.
        {
            fs::rename(
                root.path().join("selected.lock"),
                root.path().join("owned.lock"),
            )
            .unwrap();
            fs::write(root.path().join("selected.lock"), b"replacement owner").unwrap();
            let error = finish_creation(Err(CreateWorktreeError::Limit), Some(edit)).unwrap_err();
            assert!(
                matches!(error, CreateWorktreeError::Cleanup { operation, .. } if matches!(*operation, CreateWorktreeError::Limit))
            );
            assert_eq!(
                fs::read(root.path().join("selected.lock")).unwrap(),
                b"replacement owner"
            );
        }
    }

    #[rstest]
    #[case::disabled(false)]
    #[case::requested(true)]
    fn reference_file_sync_failure_retains_registration(#[case] requested: bool) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("table.ref");
        let called = std::cell::Cell::new(false);
        let result = write_shared_file_with_sync(
            &path,
            b"retained table",
            root.path(),
            SharedPermissions::Umask,
            requested,
            |_| {
                called.set(true);
                Err(io::Error::other("injected synchronization failure"))
            },
        );
        assert_eq!(called.get(), requested);
        if requested {
            assert!(
                matches!(result, Err(CreateWorktreeError::Synchronize { registration, .. }) if registration == root.path())
            );
        } else {
            result.unwrap();
        }
        assert_eq!(fs::read(path).unwrap(), b"retained table");
    }
}
