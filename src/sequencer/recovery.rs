use std::path::{Path, PathBuf};
use std::{fs, io};

use super::state::snapshot;
use super::{Error, Kind, Limits, Recovery, State};
use crate::ObjectId;
use crate::replay::todo::Todo;

fn io_error(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

impl Recovery {
    /// Completes a continuation reflog prefix with the proposed commit's subject.
    ///
    /// Abort/skip/finish messages are returned unchanged. This computation does not append a log.
    pub fn reflog_for(&self, commit: &crate::Commit) -> Vec<u8> {
        let mut message = self.reflog_message.clone();
        if matches!(
            message.as_slice(),
            b"commit (cherry-pick)" | b"commit" | b"rebase (continue)"
        ) {
            message.extend_from_slice(b": ");
            message.extend_from_slice(&commit.subject());
        }
        message
    }

    /// Verifies the original byte snapshot without changing files.
    ///
    /// Callers still need exclusive operation ownership: Git does not honor a girt-wide sequencer
    /// lock, and comparison alone cannot exclude a concurrent writer after this method returns.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Changed`] when observed metadata differs, or the underlying read failure.
    pub fn verify(&self) -> Result<(), Error> {
        for directory in ["sequencer", "rebase-merge"] {
            let expected = self.state.directory.as_deref() == Some(Path::new(directory));
            if self.state.git_dir.join(directory).exists() != expected {
                return Err(Error::Changed);
            }
        }
        if snapshot(&self.state.git_dir, self.state.limits)? != self.state.files {
            return Err(Error::Changed);
        }
        Ok(())
    }

    /// Borrows the proposed bytes for a state path; `None` means absent after publication.
    pub fn file_after(&self, path: impl AsRef<Path>) -> Option<&[u8]> {
        self.after.get(path.as_ref()).map(Vec::as_slice)
    }

    /// Publishes operation metadata after the caller's checkout and HEAD transaction completes.
    ///
    /// Requires exclusive ownership of this repository operation through the entire caller-owned
    /// checkout/ref/metadata sequence. Verifies repository location, destination HEAD and original
    /// byte snapshot, then stages each changed file under a create-new `.lock`. Files are replaced
    /// individually; completed operation directories are renamed out of Git's active namespace
    /// before cleanup. There is no cross-file crash transaction. A failure after replacement
    /// reports changed paths in [`Error::Partial`]. Re-read disk state before recovery; do not
    /// blindly retry. Operation pseudo-refs (including `ORIG_HEAD` when starting rebase) are
    /// explicit metadata outputs. Object, working-file, index, HEAD and branch publication
    /// remain caller-owned.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for stale state, unexpected HEAD/location, lock conflicts, I/O or partial
    /// publication. Earlier caller-owned checkout/reference effects remain on any metadata failure.
    pub fn publish_metadata(&self, repository: &crate::Repository) -> Result<(), Error> {
        use std::io::Write;
        if repository.git_dir() != self.state.git_dir
            || repository.object_format() != self.state.format
            || repository.head()?.id != Some(self.destination)
        {
            return Err(Error::Changed);
        }
        if let Some(branch) = &self.restore_branch
            && repository.references()?.read(&crate::refs::RefName::HEAD)?
                != Some(crate::refs::Target::Symbolic(branch.clone()))
        {
            return Err(Error::Changed);
        }
        for update in &self.reference_updates {
            if repository.references()?.resolve(&update.name)?.id != update.new {
                return Err(Error::Changed);
            }
        }
        self.verify()?;
        let total = self
            .after
            .values()
            .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
            .ok_or(Error::Limit)?;
        if self.after.len() > self.state.limits.max_files || total > self.state.limits.max_bytes {
            return Err(Error::Limit);
        }
        let retiring: Vec<_> = ["sequencer", "rebase-merge"]
            .into_iter()
            .filter(|directory| {
                self.state
                    .files
                    .keys()
                    .any(|name| name.starts_with(directory))
                    && !self.after.keys().any(|name| name.starts_with(directory))
            })
            .map(PathBuf::from)
            .collect();
        let names: std::collections::BTreeSet<_> = self
            .state
            .files
            .keys()
            .chain(self.after.keys())
            .cloned()
            .collect();
        let mut directories = CreatedDirectories(Vec::new());
        let mut staged = Vec::new();
        for name in names {
            if retiring.iter().any(|directory| name.starts_with(directory))
                || self.state.files.get(&name) == self.after.get(&name)
            {
                continue;
            }
            let path = self.state.git_dir.join(&name);
            if let Some(parent) = path.parent()
                && !parent.exists()
            {
                fs::create_dir(parent).map_err(|error| io_error(parent, error))?;
                directories.0.push(parent.to_path_buf());
            }
            let mut lock_name = path.as_os_str().to_os_string();
            lock_name.push(".lock");
            let lock = PathBuf::from(lock_name);
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
                .map_err(|error| io_error(&lock, error))?;
            let guard = MetadataLock(lock);
            if let Some(bytes) = self.after.get(&name) {
                file.write_all(bytes)
                    .map_err(|error| io_error(&path, error))?;
                file.sync_all().map_err(|error| io_error(&path, error))?;
            }
            staged.push((name, guard));
        }
        // Ignore our staged .lock files while checking that no source bytes changed.
        for (name, _) in &staged {
            let path = self.state.git_dir.join(name);
            let current = match fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(io_error(path, error)),
            };
            if current.as_ref() != self.state.files.get(name) {
                return Err(Error::Changed);
            }
        }
        let mut changed = Vec::new();
        for (name, lock) in &staged {
            let path = self.state.git_dir.join(name);
            let result = if self.after.contains_key(name) {
                fs::rename(&lock.0, &path)
            } else {
                fs::remove_file(&path)
            };
            if let Err(source) = result {
                return Err(Error::Partial {
                    changed,
                    source: Box::new(io_error(path, source)),
                });
            }
            changed.push(name.clone());
        }
        drop(staged);
        for directory in retiring {
            self.retire_directory(&directory, &mut changed, |path| fs::remove_file(path))?;
        }
        // Newly created directories now contain their published metadata.
        drop(directories);
        Ok(())
    }

    // Whole-directory retirement keeps active Git state complete until its final rename. Cleanup
    // may fail afterwards, but it cannot leave a half-deleted active sequencer directory.
    fn retire_directory(
        &self,
        directory: &Path,
        changed: &mut Vec<PathBuf>,
        mut remove: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<(), Error> {
        let fail = |path: PathBuf, source| Error::Partial {
            changed: changed.clone(),
            source: Box::new(io_error(path, source)),
        };
        let temporary = tempfile::Builder::new()
            .prefix(".girt-sequencer-retired-")
            .tempdir_in(&self.state.git_dir)
            .map_err(|source| fail(self.state.git_dir.clone(), source))?;
        let container = temporary.keep();
        let retired = container.join(directory);
        if let Err(source) = fs::rename(self.state.git_dir.join(directory), &retired) {
            let _ = fs::remove_dir(&container);
            return Err(fail(self.state.git_dir.join(directory), source));
        }
        changed.push(directory.to_path_buf());
        changed.push(
            retired
                .strip_prefix(&self.state.git_dir)
                .expect("retired metadata is a child")
                .to_path_buf(),
        );
        let fail = |path: PathBuf, source| Error::Partial {
            changed: changed.clone(),
            source: Box::new(io_error(path, source)),
        };
        for name in self
            .state
            .files
            .keys()
            .filter(|name| name.starts_with(directory))
        {
            let path = retired.join(name.strip_prefix(directory).expect("filtered directory"));
            remove(&path).map_err(|source| fail(path, source))?;
        }
        fs::remove_dir(&retired).map_err(|source| fail(retired, source))?;
        fs::remove_dir(&container).map_err(|source| fail(container, source))?;
        Ok(())
    }

    /// Prepares Git-readable single cherry-pick/revert stop metadata without publishing it.
    ///
    /// First publish the merge's conflict index/working files explicitly. HEAD remains at the
    /// supplied destination. After metadata publication, Git can resolve and `--continue`, `--skip`
    /// or `--abort` the operation. The message is stored verbatim; callers can supply their own
    /// conflict comments. Rebase uses [`Self::start_rebase`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for an existing operation, unsupported kind or unreadable state.
    pub fn start_single(
        repository: &crate::Repository,
        kind: Kind,
        source: ObjectId,
        destination: ObjectId,
        message: &[u8],
        limits: Limits,
    ) -> Result<Self, Error> {
        if source.format() != repository.object_format()
            || destination.format() != repository.object_format()
        {
            return Err(Error::Unsupported(PathBuf::from("CHERRY_PICK_HEAD")));
        }
        if kind == Kind::Rebase {
            return Err(Error::Unsupported(PathBuf::from("rebase-merge")));
        }
        let state = empty_state(repository, kind, limits)?;
        let mut after = state.files.clone();
        let name = if kind == Kind::CherryPick {
            "CHERRY_PICK_HEAD"
        } else {
            "REVERT_HEAD"
        };
        after.insert(PathBuf::from(name), format!("{source}\n").into_bytes());
        after.insert(PathBuf::from("MERGE_MSG"), message.to_vec());
        Ok(Self {
            state,
            after,
            destination,
            restore_branch: None,
            expected_branch_tip: None,
            reference_updates: Vec::new(),
            reflog_message: vec![],
        })
    }

    /// Prepares a Git-readable linear rebase todo for execution by Git or a caller executor.
    ///
    /// Caller first checks out and detaches HEAD at `onto`; the original branch remains unchanged
    /// until rebase finishes. Original branch `None` represents a detached original HEAD. Todo
    /// commands are parsed before any publication; merge/reset topology is refused. Git can then
    /// run `rebase --continue` to execute the plan. No commit has been replayed by this method.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for existing/unsupported state, invalid todo or bounds.
    pub fn start_rebase(
        repository: &crate::Repository,
        original: ObjectId,
        onto: ObjectId,
        branch: Option<crate::refs::RefName>,
        todo: &[u8],
        limits: Limits,
    ) -> Result<Self, Error> {
        if original.format() != repository.object_format()
            || onto.format() != repository.object_format()
            || branch
                .as_ref()
                .is_some_and(|name| !name.as_bytes().starts_with(b"refs/heads/"))
        {
            return Err(Error::Unsupported(PathBuf::from("rebase-merge/head-name")));
        }
        let parsed = Todo::parse(todo, limits.todo)?;
        if parsed.lines().iter().any(|line| {
            matches!(
                line.instruction,
                crate::replay::todo::Instruction::Merge { .. }
                    | crate::replay::todo::Instruction::Reset(_)
            )
        }) {
            return Err(Error::Unsupported(PathBuf::from(
                "rebase-merge/git-rebase-todo",
            )));
        }
        let state = empty_state(repository, Kind::Rebase, limits)?;
        let mut after = state.files.clone();
        let branch = branch
            .map(|name| name.into_bytes())
            .unwrap_or_else(|| b"detached HEAD".to_vec());
        let mut branch_line = branch;
        branch_line.push(b'\n');
        for (name, bytes) in [
            ("head-name", branch_line),
            ("onto", format!("{onto}\n").into_bytes()),
            ("orig-head", format!("{original}\n").into_bytes()),
            ("git-rebase-todo", todo.to_vec()),
            ("done", vec![]),
            ("msgnum", b"0\n".to_vec()),
            ("end", format!("{}\n", parsed.lines().len()).into_bytes()),
            ("interactive", vec![]),
        ] {
            after.insert(PathBuf::from("rebase-merge").join(name), bytes);
        }
        after.insert(
            PathBuf::from("ORIG_HEAD"),
            format!("{original}\n").into_bytes(),
        );
        Ok(Self {
            state,
            after,
            destination: onto,
            restore_branch: None,
            expected_branch_tip: None,
            reference_updates: Vec::new(),
            reflog_message: vec![],
        })
    }
}

fn empty_state(repository: &crate::Repository, kind: Kind, limits: Limits) -> Result<State, Error> {
    if State::read(repository.git_dir(), repository.object_format(), limits)?.is_some() {
        return Err(Error::Changed);
    }
    Ok(State {
        git_dir: repository.git_dir().to_path_buf(),
        format: repository.object_format(),
        limits,
        files: snapshot(repository.git_dir(), limits)?,
        kind,
        directory: None,
        pending: None,
        original_head: None,
        onto: None,
        branch: None,
        todo: None,
    })
}
struct MetadataLock(PathBuf);
impl Drop for MetadataLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

struct CreatedDirectories(Vec<PathBuf>);
impl Drop for CreatedDirectories {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            let _ = fs::remove_dir(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ObjectFormat;

    fn rebase_state() -> (tempfile::TempDir, State) {
        let directory = tempfile::tempdir().unwrap();
        let state_dir = directory.path().join("rebase-merge");
        fs::create_dir(&state_dir).unwrap();
        for (name, bytes) in [
            ("head-name", b"detached HEAD\n".as_slice()),
            ("onto", b"1111111111111111111111111111111111111111\n"),
            ("orig-head", b"2222222222222222222222222222222222222222\n"),
            ("git-rebase-todo", b""),
        ] {
            fs::write(state_dir.join(name), bytes).unwrap();
        }
        let state = State::read(directory.path(), ObjectFormat::Sha1, Limits::default())
            .unwrap()
            .unwrap();
        (directory, state)
    }

    #[test]
    fn failed_retired_cleanup_keeps_active_state_absent_and_reports_preserved_path() {
        let (directory, state) = rebase_state();
        let recovery = state
            .prepare(crate::sequencer::Action::Abort, state.onto().unwrap())
            .unwrap();
        let mut changed = Vec::new();
        let error = recovery
            .retire_directory(Path::new("rebase-merge"), &mut changed, |_| {
                Err(io::Error::other("injected cleanup failure"))
            })
            .unwrap_err();
        assert!(matches!(error, Error::Partial { .. }));
        assert_eq!(changed.len(), 2);
        assert!(!directory.path().join("rebase-merge").exists());
        let retired = directory.path().join(&changed[1]);
        assert_eq!(
            fs::read(retired.join("orig-head")).unwrap(),
            b"2222222222222222222222222222222222222222\n"
        );
        assert!(
            State::read(directory.path(), ObjectFormat::Sha1, Limits::default())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn successful_retirement_removes_only_the_observed_directory() {
        let (directory, state) = rebase_state();
        fs::write(directory.path().join("unrelated"), b"retain\n").unwrap();
        let recovery = state
            .prepare(crate::sequencer::Action::Abort, state.onto().unwrap())
            .unwrap();
        recovery
            .retire_directory(Path::new("rebase-merge"), &mut Vec::new(), |path| {
                fs::remove_file(path)
            })
            .unwrap();
        assert_eq!(
            fs::read(directory.path().join("unrelated")).unwrap(),
            b"retain\n"
        );
        assert!(!directory.path().join("rebase-merge").exists());
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
