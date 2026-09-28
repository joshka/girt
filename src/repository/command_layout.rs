//! Explicit storage selection and command worktree setup, independent of ambient environment.
use super::*;

impl RepositoryLocation {
    /// Selects an exact private Git directory with optional common and object storage overrides.
    ///
    /// Every supplied path must be nonempty and absolute. The caller anchors command-environment
    /// paths against its captured working directory, rejecting empty values before anchoring.
    /// An explicit common directory bypasses `commondir` entirely, even if that file is malformed.
    /// Its explicit presence remains meaningful when it names the private directory itself.
    /// Private and common directories are canonicalized; the private logical alias remains in
    /// conditional-include context. Object storage is checked later by metadata or full opening.
    ///
    /// This does not select an index, consult environment variables, establish trust, or freeze
    /// filesystem identity. [`Self::read_metadata_for_command`] supplies command worktree setup;
    /// ordinary metadata/full opening retains its existing linked-backlink validation.
    ///
    /// # Errors
    ///
    /// Rejects relative/empty supplied paths and reports private/common selection errors as in
    /// [`Self::at_git_dir`]. The object path need not exist until metadata is read.
    pub fn at_git_dir_with_storage(
        git_dir: impl AsRef<Path>,
        common_dir: Option<&Path>,
        object_dir: Option<&Path>,
    ) -> Result<Self, OpenError> {
        let git_dir = git_dir.as_ref();
        require_absolute(git_dir)?;
        if let Some(path) = common_dir {
            require_absolute(path)?;
        }
        if let Some(path) = object_dir {
            require_absolute(path)?;
        }
        Self::resolve_storage(git_dir, true, common_dir, object_dir)
    }

    /// Reads selected metadata with explicit-command worktree setup instead of backlink inference.
    ///
    /// `cwd` and a supplied `worktree_override` must be nonempty absolute paths. An override wins
    /// over configured bare/worktree settings, even when the checkout does not exist. Otherwise
    /// the direct layout configuration selects bare or a worktree path (relative to the private
    /// Git directory), falling back to `cwd`. A source checkout and its backlink are not required.
    ///
    /// An explicitly supplied common directory suppresses its `core.bare` and `core.worktree`,
    /// including when private/common paths are equal. Enabled private `config.worktree` settings
    /// still apply. An on-disk `commondir` relationship also suppresses common layout settings.
    /// The effective configuration retains these values and their origins for policy consumers.
    ///
    /// Conflicting configured bare/worktree settings are represented by
    /// [`RepositoryMetadata::has_conflicting_worktree_config`], so the caller can report a warning.
    /// Normal metadata opening still rejects that conflict and validates linked backlinks.
    /// No environment or process working directory is read. Relative caller config-file paths
    /// must also be anchored before supplying `inputs`.
    ///
    /// # Errors
    ///
    /// Reports metadata/configuration errors as in [`Self::read_metadata_with_config`]. Command
    /// setup currently requires an explicit direct `core.repositoryFormatVersion`; repositories
    /// omitting it have different Git bootstrap rules and return [`OpenError::Unsupported`].
    /// Configured worktree interpolation remains unsupported. Supplied storage is not mutated.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use girt::RepositoryLocation;
    /// use girt::config::ConfigInputs;
    /// let cwd = std::env::current_dir()?;
    /// let location = RepositoryLocation::at_git_dir_with_storage(cwd.join(".git"), None, None)?;
    /// let metadata = location.read_metadata_for_command(&ConfigInputs::default(), &cwd, None)?;
    /// if metadata.has_conflicting_worktree_config() {
    ///     eprintln!("core.bare and core.worktree conflict");
    /// }
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn read_metadata_for_command(
        &self,
        inputs: &ConfigInputs,
        cwd: &Path,
        worktree_override: Option<&Path>,
    ) -> Result<RepositoryMetadata, OpenError> {
        require_absolute(cwd)?;
        for file in &inputs.files {
            require_absolute(&file.path)?;
        }
        for path in inputs
            .context
            .home
            .iter()
            .chain(inputs.context.prefix.iter())
        {
            require_absolute(path)?;
        }
        for (_, path) in &inputs.context.user_homes {
            require_absolute(path)?;
        }
        if let Some(path) = worktree_override {
            require_absolute(path)?;
        }
        RepositoryMetadata::read_location(
            self,
            inputs,
            crate::refs::reftable::StackLimits::default(),
            Some(CommandWorktree {
                cwd,
                worktree_override,
            }),
        )
    }
}

impl RepositoryMetadata {
    /// Whether command setup observed both `core.bare=true` and a configured worktree path.
    ///
    /// Bare takes precedence unless an explicit worktree override is supplied. The caller owns
    /// reporting a warning; no diagnostic is printed by metadata reading. Ordinary metadata
    /// opening rejects the conflict instead of returning it.
    pub fn has_conflicting_worktree_config(&self) -> bool {
        self.worktree_config_conflict
    }
}

pub(super) struct CommandWorktree<'a> {
    cwd: &'a Path,
    worktree_override: Option<&'a Path>,
}

impl CommandWorktree<'_> {
    pub(super) fn resolve(
        self,
        git_dir: &Path,
        config: &Config,
        source: &Path,
    ) -> Result<(Option<PathBuf>, bool, bool), OpenError> {
        if let Some(path) = self.worktree_override {
            return Ok((Some(available_path(path)?), false, false));
        }
        let bare = boolean(config, source, "bare")? == Some(true);
        let configured = config.value("core", None, "worktree");
        if bare {
            return Ok((None, true, configured.is_some()));
        }
        let worktree = if let Some(value) = configured {
            let value = value
                .filter(|v| !v.is_empty())
                .ok_or_else(|| malformed(source, "empty core.worktree"))?;
            if value.starts_with(b"~") || value.starts_with(b"%(") {
                return Err(unsupported(source, "core.worktree path interpolation"));
            }
            available_path(&git_dir.join(path_bytes(source, value)?))?
        } else {
            available_path(self.cwd)?
        };
        Ok((Some(worktree), false, false))
    }
}

fn require_absolute(path: &Path) -> Result<(), OpenError> {
    if path.as_os_str().is_empty() || !path.is_absolute() {
        return Err(malformed(
            path,
            "explicit selection requires a nonempty absolute path",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
