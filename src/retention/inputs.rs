//! Bounded input observations used instead of decoding the retention graph again.
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use super::{RetentionError, RetentionPlan, RetentionPolicy, check};
use crate::object::Hasher;
use crate::{ObjectId, Repository};

#[derive(Clone, Debug, Eq, PartialEq)]
struct FileInput {
    len: u64,
    modified: SystemTime,
    digest: Option<ObjectId>,
    link: Option<(PathBuf, SystemTime)>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Inputs(BTreeMap<PathBuf, FileInput>);

impl Inputs {
    pub(super) fn observe(
        repository: &Repository,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<Self, RetentionError> {
        let current = Repository::open(repository.git_dir()).map_err(RetentionError::read)?;
        if current.object_dir() != repository.object_dir()
            || current.object_format() != repository.object_format()
            || current.reference_backend() != repository.reference_backend()
        {
            return Err(RetentionError::RepositoryLayoutChanged);
        }
        let mut observer = Observer {
            inputs: Self(BTreeMap::new()),
            repository: &current,
            policy,
            cancel,
            entries: 0,
        };
        let mut roots = vec![
            current.common_dir().to_path_buf(),
            current.git_dir().to_path_buf(),
        ];
        roots.sort();
        roots.dedup();
        for root in roots {
            for name in [
                "config",
                "config.worktree",
                "commondir",
                "gitdir",
                "HEAD",
                "index",
                "shallow",
                "packed-refs",
                "refs",
                "logs",
                "reftable",
                "worktrees",
                "ORIG_HEAD",
                "MERGE_HEAD",
                "CHERRY_PICK_HEAD",
                "REVERT_HEAD",
                "BISECT_HEAD",
                "REBASE_HEAD",
                "AUTO_MERGE",
                "FETCH_HEAD",
            ] {
                observer.collect(root.join(name), true)?;
            }
        }
        let directories = crate::Objects::retention_directories(current.object_dir())
            .map_err(RetentionError::read)?;
        for directory in directories {
            observer.collect(directory.clone(), false)?;
            observer.collect(directory.join("info/alternates"), true)?;
        }
        observer.config_sources(&current)?;
        for entry in current
            .worktrees(policy.max_entries, cancel)
            .map_err(RetentionError::read)?
        {
            let worktree = current
                .open_retained_worktree(&entry.git_dir)
                .map_err(RetentionError::read)?;
            observer.config_sources(&worktree)?;
        }
        Ok(observer.inputs)
    }

    #[cfg(unix)]
    pub(super) fn accept(
        &mut self,
        observed: Self,
        changed: &[(PathBuf, Option<ObjectId>)],
    ) -> Result<(), RetentionError> {
        let paths: Vec<_> = changed.iter().map(|(path, _)| path.clone()).collect();
        if !self.same_except(&observed, &paths) {
            return Err(RetentionError::RetentionInputsChanged);
        }
        for (path, expected) in changed {
            match expected {
                Some(digest)
                    if observed.0.get(path).and_then(|file| file.digest) == Some(*digest) => {}
                None if !observed.0.keys().any(|input| input.starts_with(path)) => {}
                _ => return Err(RetentionError::RetentionOutputChanged),
            }
        }
        *self = observed;
        Ok(())
    }

    /// Accept only the exact files this phase changes; all independent inputs still compare.
    #[cfg(unix)]
    pub(super) fn same_except(&self, other: &Self, paths: &[PathBuf]) -> bool {
        let untouched = |path: &&PathBuf| !paths.iter().any(|allowed| path.starts_with(allowed));
        self.0
            .iter()
            .filter(|(path, _)| untouched(path))
            .eq(other.0.iter().filter(|(path, _)| untouched(path)))
    }
}

/// One bounded filesystem inventory; traversal is iterative even for deeply nested ref names.
struct Observer<'a> {
    inputs: Inputs,
    repository: &'a Repository,
    policy: &'a RetentionPolicy,
    cancel: &'a AtomicBool,
    entries: usize,
}

impl Observer<'_> {
    fn collect(&mut self, path: PathBuf, content: bool) -> Result<(), RetentionError> {
        let mut pending = vec![path];
        while let Some(path) = pending.pop() {
            check(self.cancel)?;
            // Duplicate paths have already been observed with this strength. Alternates metadata
            // is deliberately revisited with content hashing after the metadata-only inventory.
            if self
                .inputs
                .0
                .get(&path)
                .is_some_and(|file| !content || file.digest.is_some())
            {
                continue;
            }
            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(RetentionError::read(e)),
            };
            let (metadata, link) = if metadata.is_symlink() {
                let target = fs::read_link(&path).map_err(RetentionError::read)?;
                let modified = metadata.modified().map_err(RetentionError::read)?;
                let followed = fs::metadata(&path).map_err(RetentionError::read)?;
                if !followed.is_file() {
                    return Err(RetentionError::NonFileRetentionInputTarget);
                }
                (followed, Some((target, modified)))
            } else {
                (metadata, None)
            };
            self.entries = self
                .entries
                .checked_add(1)
                .ok_or(RetentionError::InputEntryOverflow)?;
            if self.entries > self.policy.max_entries {
                return Err(RetentionError::InputEntryLimit);
            }
            #[cfg(unix)]
            use std::os::unix::fs::MetadataExt;
            if metadata.is_dir() {
                self.inputs.0.insert(
                    path.clone(),
                    FileInput {
                        len: 0,
                        modified: std::time::UNIX_EPOCH,
                        digest: None,
                        link: None,
                        #[cfg(unix)]
                        identity: (metadata.dev(), metadata.ino(), 0, 0),
                    },
                );
                for entry in fs::read_dir(&path).map_err(RetentionError::read)? {
                    check(self.cancel)?;
                    if pending.len() >= self.policy.max_entries.saturating_sub(self.entries) {
                        return Err(RetentionError::InputEntryLimit);
                    }
                    pending.push(entry.map_err(RetentionError::read)?.path());
                }
                continue;
            }
            if !metadata.is_file() {
                return Err(RetentionError::NonFileRetentionInput);
            }
            let digest = if content {
                Some(self.hash_file(&path, metadata.len())?)
            } else {
                None
            };
            self.inputs.0.insert(
                path,
                FileInput {
                    len: metadata.len(),
                    modified: metadata.modified().map_err(RetentionError::read)?,
                    digest,
                    link,
                    #[cfg(unix)]
                    identity: (
                        metadata.dev(),
                        metadata.ino(),
                        metadata.ctime(),
                        metadata.ctime_nsec(),
                    ),
                },
            );
        }
        Ok(())
    }

    fn config_sources(&mut self, repository: &Repository) -> Result<(), RetentionError> {
        for entry in repository.config().entries() {
            if let Some(origin) = &entry.origin {
                for source in std::iter::once(&origin.location).chain(origin.included_from.iter()) {
                    if let Some(path) = &source.path {
                        self.collect(path.clone(), true)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn hash_file(&self, path: &Path, len: u64) -> Result<ObjectId, RetentionError> {
        let bound = self
            .policy
            .max_reference_bytes
            .max(self.policy.reflog.bytes)
            .max(self.policy.index.max_bytes) as u64;
        if len > bound {
            return Err(RetentionError::InputByteLimit);
        }
        let mut file = fs::File::open(path).map_err(RetentionError::read)?;
        let mut hash = Hasher::new(self.repository.object_format());
        let mut bytes = 0u64;
        let mut buffer = [0; 8192];
        loop {
            check(self.cancel)?;
            let count = file.read(&mut buffer).map_err(RetentionError::read)?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(count as u64)
                .ok_or(RetentionError::InputByteOverflow)?;
            if bytes > bound {
                return Err(RetentionError::InputByteLimit);
            }
            hash.update(&buffer[..count]);
        }
        Ok(hash.finalize())
    }
}

pub(super) struct Session {
    pub plan: RetentionPlan,
    pub inputs: Inputs,
}

impl Session {
    pub(super) fn new(
        repository: &Repository,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<Self, RetentionError> {
        let inputs = Inputs::observe(repository, policy, cancel)?;
        let current = Repository::open(repository.git_dir()).map_err(RetentionError::read)?;
        let plan = current.plan_retention(policy, cancel);
        if let super::RetentionOutcome::Incomplete(reason) = plan.outcome {
            return Err(reason);
        }
        let session = Self { plan, inputs };
        session.validate(repository, policy, cancel)?;
        Ok(session)
    }

    pub(super) fn validate(
        &self,
        repository: &Repository,
        policy: &RetentionPolicy,
        cancel: &AtomicBool,
    ) -> Result<(), RetentionError> {
        let observed = Inputs::observe(repository, policy, cancel)?;
        if observed != self.inputs {
            return Err(RetentionError::RetentionInputsChanged);
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(super) fn digest(repository: &Repository, bytes: &[u8]) -> ObjectId {
    let mut hash = Hasher::new(repository.object_format());
    hash.update(bytes);
    hash.finalize()
}
