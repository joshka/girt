use std::path::PathBuf;

use super::{UpdateOptions, UpdatePlan, UpdateReport};
use crate::{Repository, workflow};

/// Completed population phases, also retained on failure. No phase is rolled back.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct PopulateReport {
    /// Clone completed in the parent's modules storage. Its repository snapshot predates layout
    /// edits.
    pub cloned: Option<Box<workflow::CloneOutcome>>,
    /// Fetch completed in a previously populated child.
    pub fetched: Option<workflow::FetchOutcome>,
    /// Worktree location was published in the new child's config.
    pub worktree_configured: bool,
    /// The child's exclusive `.git` file was written.
    pub gitfile_written: bool,
    /// Completed filtered checkout/HEAD publication, including no-op updates.
    pub update: Option<UpdateReport>,
}

/// A failed population with all completed phases retained for caller-directed recovery.
#[derive(Debug, thiserror::Error)]
#[error("submodule population failed: {source}")]
pub struct PopulateError {
    /// Completed phases; nested failures can additionally retain partial effects.
    pub report: Box<PopulateReport>,
    /// The failed phase or refusal.
    #[source]
    pub source: PopulateFailure,
}

/// Population refusal or failed phase. Existing paths are never deleted or replaced.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PopulateFailure {
    /// Platform or update method is unsupported, or the plan became stale.
    #[error(transparent)]
    Update(#[from] super::UpdateError),
    /// A reserved path exists, has nonempty content, or traverses a symlink/alias.
    #[error("submodule destination is occupied or unsafe: {}", .0.display())]
    Occupied(PathBuf),
    /// Layout creation or metadata inspection failed; partial owned directories can remain.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Repository/config/index/object preparation failed without hiding its original type.
    #[error(transparent)]
    Prepare(Box<crate::Error>),
    /// A new clone requests shallow history, which the clone workflow cannot yet produce.
    #[error("shallow submodule cloning is not supported")]
    Shallow,
    /// The URL cannot be represented by the current string-based workflow transport API.
    #[error("submodule URL is not UTF-8")]
    UrlEncoding,
    /// Clone failed; inspect its nested partial report before recovery.
    #[error(transparent)]
    Clone(Box<workflow::CloneError>),
    /// Fetch failed; inspect any retained installation/reference effects.
    #[error(transparent)]
    Fetch(Box<workflow::TransferError>),
    /// The recorded commit is still absent after fetching advertised heads/tags.
    #[error("recorded submodule commit {0} was not fetched from advertised heads or tags")]
    MissingCommit(crate::ObjectId),
}

impl UpdatePlan {
    /// Explicitly clones/fetches a child if necessary, then applies its recorded checkout.
    ///
    /// Unlike [`Self::apply`], this operation authorizes transport. New child metadata is cloned
    /// into `parent.common_dir()/modules/<name>`, then configured with `core.worktree` and linked
    /// by an exclusively created child `.git` file. It writes absolute paths; Git reads both
    /// absolute and relative gitfiles/worktree settings. No existing metadata is moved or removed.
    /// The checkout starts from an empty baseline for a newly created child, uses its own target
    /// attributes and explicitly permitted filters, and always detaches its HEAD.
    ///
    /// Existing populated children fetch origin's advertised heads/tags only when the recorded
    /// commit is absent, preserving their local branches. Unadvertised/unreachable recorded commits
    /// are explicitly refused; direct-ID fetch is not implemented. None performs no transport;
    /// rebase, merge and custom methods are refused. A shallow recommendation refuses a new clone;
    /// branch selection does not change a recorded-commit checkout.
    ///
    /// Configuration inherits the parent's explicitly selected non-local layers plus the child's
    /// local config. Transport receives the captured environment and automatic protocol policy
    /// (`GIT_PROTOCOL_FROM_USER=0`); file URLs therefore need explicitly authorized file transport.
    /// Clone uses bounded workflow defaults; existing-child fetch inherits [`workflow::fetch`]'s
    /// current resource contract. These synchronous workflows do not offer cancellation/deadlines.
    ///
    /// The caller must exclusively own the parent index/config and child metadata/worktree paths
    /// and ancestors throughout. No-follow/exact-spelling checks refuse symlinks and path aliases,
    /// but concurrent replacement and hostile child metadata are outside this contract. Reopen
    /// handles after changes. Publication is not atomic or crash-durable across phases.
    ///
    /// # Errors
    ///
    /// Returns completed clone/fetch/layout phases in [`PopulateError::report`]; the nested source
    /// can retain additional partial effects. A failed new child can leave modules storage,
    /// directories, config, a gitfile or checkout effects. Inspect those under exclusive ownership
    /// before recovery; never delete a collided path based on this error. Existing absorbed
    /// metadata with no worktree is refused rather than adopted. Checkout requires Linux/macOS.
    ///
    /// ```no_run
    /// use girt::{Repository, Signature, submodule, workflow};
    /// # fn populate(parent: &Repository, plan: &submodule::UpdatePlan, person: Signature)
    /// # -> Result<(), Box<dyn std::error::Error>> {
    /// let options = submodule::UpdateOptions::new(person)
    ///     .with_environment(workflow::Environment::from_process());
    /// let report = plan.populate(parent, &options, &mut workflow::NoCallbacks)?;
    /// assert!(report.update.is_some());
    /// # Ok(())
    /// # }
    /// ```
    pub fn populate(
        &self,
        parent: &Repository,
        options: &UpdateOptions,
        callbacks: &mut dyn workflow::TransferCallbacks,
    ) -> Result<PopulateReport, PopulateError> {
        let mut report = PopulateReport::default();
        if matches!(
            self.method(),
            super::UpdateMethod::BuiltIn(super::Update::None)
        ) {
            report.update = Some(UpdateReport::default());
            return Ok(report);
        }
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let result = self.populate_inner(parent, options, callbacks, &mut report);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let result: Result<(), PopulateFailure> = {
            let _ = (parent, options, callbacks);
            Err(super::UpdateError::UnsupportedPlatform.into())
        };
        match result {
            Ok(()) => Ok(report),
            Err(source) => Err(PopulateError {
                report: Box::new(report),
                source,
            }),
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn populate_inner(
        &self,
        parent: &Repository,
        options: &UpdateOptions,
        callbacks: &mut dyn workflow::TransferCallbacks,
        report: &mut PopulateReport,
    ) -> Result<(), PopulateFailure> {
        use std::io::Write;
        use std::os::unix::ffi::OsStrExt;
        use std::sync::atomic::AtomicBool;

        use super::{Update, UpdateMethod};

        match self.method() {
            UpdateMethod::BuiltIn(Update::None) => {
                report.update = Some(UpdateReport::default());
                return Ok(());
            }
            UpdateMethod::BuiltIn(Update::Checkout) => {}
            _ => return Err(super::UpdateError::UnsupportedMethod.into()),
        }
        let fresh = parent.reopen().map_err(prepare)?;
        if !super::execute::same_layout(parent, &fresh)
            || self.expected != super::plan::update_values(fresh.config(), self.name())
        {
            return Err(super::UpdateError::Changed.into());
        }
        let parent = &fresh;
        let index = parent.read_index(options.limits.index).map_err(prepare)?;
        let mut entries = index
            .as_ref()
            .into_iter()
            .flat_map(|i| i.entries())
            .filter(|e| e.path == self.path());
        let entry = entries.next().ok_or(super::UpdateError::Changed)?;
        if entry.id != self.recorded()
            || entry.mode != crate::index::Mode::Gitlink
            || entry.stage != crate::index::Stage::Normal
            || entries.next().is_some()
        {
            return Err(super::UpdateError::Changed.into());
        }
        let worktree = parent
            .worktree()
            .ok_or(super::UpdateError::MissingCheckout)?;
        let child_path = worktree.join(std::ffi::OsStr::from_bytes(self.path()));
        if child_path
            .as_os_str()
            .as_bytes()
            .iter()
            .any(u8::is_ascii_control)
            || parent
                .common_dir()
                .as_os_str()
                .as_bytes()
                .iter()
                .any(u8::is_ascii_control)
            || (cfg!(target_os = "macos") && (!self.path().is_ascii() || !self.name().is_ascii()))
        {
            return Err(PopulateFailure::Occupied(child_path));
        }
        let populated = super::status::checkout_path(parent, self.path())
            .map_err(prepare)?
            .is_some();
        let environment = options
            .environment
            .clone()
            .with_current_dir(worktree)
            .with("GIT_PROTOCOL_FROM_USER", "0");
        if populated {
            let child = Repository::open_with_config(&child_path, parent.inherited_config())
                .map_err(prepare)?;
            if child.is_bare() || child.worktree() != Some(child_path.as_path()) {
                return Err(PopulateFailure::Occupied(child_path));
            }
            self.recorded()
                .require_format(child.object_format())
                .map_err(prepare)?;
            match child
                .objects_with(options.limits.packs)
                .map_err(prepare)?
                .read_commit_with(self.recorded(), options.limits.read)
            {
                Ok(_) => {}
                Err(crate::ReadCommitError::Missing(_)) => {
                    let environment = super::child_environment(&child, &environment)
                        .with_current_dir(&child_path);
                    let fetch = workflow::FetchOptions::new([
                        "+refs/heads/*:refs/remotes/origin/*",
                        "refs/tags/*:refs/tags/*",
                    ]);
                    report.fetched = Some(
                        workflow::fetch(&child, "origin", &fetch, &environment, callbacks)
                            .map_err(|e| PopulateFailure::Fetch(Box::new(e)))?,
                    );
                    let child = child.reopen().map_err(prepare)?;
                    match child
                        .objects_with(options.limits.packs)
                        .map_err(prepare)?
                        .read_commit_with(self.recorded(), options.limits.read)
                    {
                        Ok(_) => {}
                        Err(crate::ReadCommitError::Missing(_)) => {
                            return Err(PopulateFailure::MissingCommit(self.recorded()));
                        }
                        Err(e) => return Err(prepare(e)),
                    }
                }
                Err(e) => return Err(prepare(e)),
            }
            report.update = Some(self.apply(parent, options)?);
            return Ok(());
        }
        if self.shallow() {
            return Err(PopulateFailure::Shallow);
        }
        // Check both namespaces before transport or creating any repository data.
        check_empty_path(worktree, self.path())?;
        let modules = parent.common_dir().join("modules");
        check_storage(&modules, self.name())?;
        create_directories(worktree, self.path())?;
        let storage_parent = self.name().rsplitn(2, |b| *b == b'/').nth(1);
        ensure_directory(&modules)?;
        if let Some(prefix) = storage_parent {
            create_directories(&modules, prefix)?;
        }
        let destination = modules.join(std::ffi::OsStr::from_bytes(self.name()));
        let url = std::str::from_utf8(self.url()).map_err(|_| PopulateFailure::UrlEncoding)?;
        let cloned = workflow::clone_with(
            url,
            &destination,
            &workflow::CloneOptions::default()
                .with_kind(crate::InitKind::Bare)
                .with_checkout(false),
            parent.inherited_config(),
            &environment,
            callbacks,
        )
        .map_err(|e| PopulateFailure::Clone(Box::new(e)))?;
        report.cloned = Some(Box::new(cloned));
        let child = Repository::open_with_config(&destination, parent.inherited_config())
            .map_err(prepare)?;
        self.recorded()
            .require_format(child.object_format())
            .map_err(prepare)?;
        let mut edit = child.edit_config(1024 * 1024).map_err(prepare)?;
        edit.document_mut()
            .append("core", None, "bare", b"false")
            .map_err(prepare)?;
        edit.document_mut()
            .append("core", None, "worktree", child_path.as_os_str().as_bytes())
            .map_err(prepare)?;
        edit.commit().map_err(prepare)?;
        report.worktree_configured = true;
        let mut gitfile = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(child_path.join(".git"))?;
        // An opened gitfile can already be visible if writing later fails.
        report.gitfile_written = true;
        gitfile.write_all(b"gitdir: ")?;
        gitfile.write_all(destination.as_os_str().as_bytes())?;
        gitfile.write_all(b"\n")?;
        report.update =
            Some(self.checkout_initial(parent, options, &AtomicBool::new(false), true, || {})?);
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn prepare(error: impl Into<crate::Error>) -> PopulateFailure {
    PopulateFailure::Prepare(Box::new(error.into()))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ensure_directory(path: &std::path::Path) -> Result<(), PopulateFailure> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(PopulateFailure::Occupied(path.to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(std::fs::create_dir(path)?)
        }
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_empty_path(root: &std::path::Path, path: &[u8]) -> Result<(), PopulateFailure> {
    use std::os::unix::ffi::OsStrExt;
    let mut current = root.to_owned();
    for component in path.split(|b| *b == b'/') {
        let native = std::ffi::OsStr::from_bytes(component);
        if current.exists() {
            for entry in std::fs::read_dir(&current)? {
                let name = entry?.file_name();
                if cfg!(target_os = "macos")
                    && name != native
                    && name.as_bytes().eq_ignore_ascii_case(component)
                {
                    return Err(PopulateFailure::Occupied(current.join(native)));
                }
            }
        }
        current.push(native);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(PopulateFailure::Occupied(current)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    if std::fs::read_dir(&current)?.next().is_some() {
        return Err(PopulateFailure::Occupied(current));
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn check_storage(root: &std::path::Path, name: &[u8]) -> Result<(), PopulateFailure> {
    use std::os::unix::ffi::OsStrExt;
    // Existing storage is never adopted; prefixes must not themselves contain a repository.
    if cfg!(target_os = "macos") {
        let parent = root
            .parent()
            .ok_or_else(|| PopulateFailure::Occupied(root.to_owned()))?;
        for entry in std::fs::read_dir(parent)? {
            let name = entry?.file_name();
            if name.as_bytes() != b"modules" && name.as_bytes().eq_ignore_ascii_case(b"modules") {
                return Err(PopulateFailure::Occupied(root.to_owned()));
            }
        }
    }
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(PopulateFailure::Occupied(root.to_owned())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    }
    let mut current = root.to_owned();
    for component in name.split(|b| *b == b'/') {
        if current.join("HEAD").exists() || current.join("config").exists() {
            return Err(PopulateFailure::Occupied(current));
        }
        let native = std::ffi::OsStr::from_bytes(component);
        for entry in std::fs::read_dir(&current)? {
            let name = entry?.file_name();
            if cfg!(target_os = "macos")
                && name != native
                && name.as_bytes().eq_ignore_ascii_case(component)
            {
                return Err(PopulateFailure::Occupied(current.join(native)));
            }
        }
        current.push(native);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(PopulateFailure::Occupied(current)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Err(PopulateFailure::Occupied(current))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn create_directories(root: &std::path::Path, path: &[u8]) -> Result<(), PopulateFailure> {
    use std::os::unix::ffi::OsStrExt;
    let mut current = root.to_owned();
    for component in path.split(|b| *b == b'/') {
        current.push(std::ffi::OsStr::from_bytes(component));
        ensure_directory(&current)?;
    }
    Ok(())
}
