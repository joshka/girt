use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::refs::{Backend, Expected, RefEdit, RefName, Reflog, ReflogEntry, ReflogRecord, Target};
use crate::{ObjectFormat, ObjectId, Repository, Signature};

/// Bounds for a complete stash-history observation; no truncated history is returned.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct LogLimits {
    /// Maximum raw reflog bytes (default 16 MiB).
    pub max_bytes: usize,
    /// Maximum saved entries (default 100,000).
    pub max_entries: usize,
}
impl Default for LogLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_entries: 100_000,
        }
    }
}

/// Exact files-backend stash history and tip observed for a conditional edit.
///
/// Entries are a reflog stack, not commit ancestry: every stash's first parent is its original
/// HEAD, not the preceding stash. Reads retain original record bytes and never mutate storage.
/// Reftable history editing is currently refused explicitly.
#[derive(Clone, Debug)]
pub struct History {
    directory: PathBuf,
    format: ObjectFormat,
    tip: Option<ObjectId>,
    bytes: Option<Vec<u8>>,
    records: Vec<ReflogRecord>,
    limits: LogLimits,
}

impl LogLimits {
    /// Selects the complete input/output log byte bound.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes;
        self
    }
}

impl History {
    /// Observes a bounded complete `refs/stash` log and its direct stored tip.
    ///
    /// Requires a trusted repository and caller isolation for a coherent cross-file observation.
    /// A missing ref with an existing log or a tip differing from the newest entry is refused.
    /// No commit/tree objects are read by listing.
    ///
    /// # Errors
    ///
    /// Returns limits, malformed records, inconsistent tips, unsafe paths or unsupported backend.
    pub fn read(repo: &Repository, limits: LogLimits) -> Result<Self, LogError> {
        if repo.reference_backend() != Backend::Files {
            return Err(LogError::Backend);
        }
        let name = stash_name();
        let tip = match repo.references()?.read(&name)? {
            Some(Target::Direct(id)) => Some(id),
            None => None,
            Some(Target::Symbolic(_)) => return Err(LogError::Inconsistent),
        };
        let bytes = read_log(repo.common_dir(), limits)?;
        let records =
            ReflogRecord::parse(repo.object_format(), bytes.as_deref().unwrap_or_default())?;
        if records.len() > limits.max_entries {
            return Err(LogError::Limit);
        }
        if records.last().map(|r| r.entry().new) != tip || (tip.is_none() && bytes.is_some()) {
            return Err(LogError::Inconsistent);
        }
        Ok(Self {
            directory: repo.common_dir().to_path_buf(),
            format: repo.object_format(),
            tip,
            bytes,
            records,
            limits,
        })
    }

    /// Lists entries newest first; ordinal zero is Git's `stash@{0}`.
    pub fn entries(&self) -> impl DoubleEndedIterator<Item = &ReflogEntry> + ExactSizeIterator {
        self.records.iter().rev().map(ReflogRecord::entry)
    }

    /// Plans an append with explicit reflog identity/message; installs no objects or refs.
    ///
    /// The supplied ID must be installed before [`LogEdit::publish`]. Like low-level reference
    /// transactions, this operation validates format and encoding, not object connectivity.
    ///
    /// # Errors
    ///
    /// Returns invalid identity/message/format or output bounds. An unchanged ID still appends.
    pub fn push(
        &self,
        id: ObjectId,
        committer: Signature,
        message: Vec<u8>,
    ) -> Result<LogEdit, LogError> {
        id.require_format(self.format)
            .map_err(crate::refs::ReferenceError::from)?;
        let entry = ReflogEntry {
            old: self.tip.unwrap_or_else(|| ObjectId::null(self.format)),
            new: id,
            committer,
            message,
        };
        let mut after = self.bytes.clone().unwrap_or_default();
        after.extend(entry.encode()?);
        if after.len() > self.limits.max_bytes || self.records.len() >= self.limits.max_entries {
            return Err(LogError::Limit);
        }
        Ok(LogEdit {
            before: self.clone(),
            after: Some(after),
            tip: Some(id),
        })
    }

    /// Plans removal of one newest-first entry, retaining every other record and object.
    ///
    /// Rewrites only predecessor identities after the removed record, preserving other lexical
    /// bytes. Dropping the last entry deletes the stash name and its log. Pop callers must finish
    /// successful apply/checkout/index publication before publishing this plan; conflicts keep it.
    /// Observe history before applying and reuse that observation for the drop. A concurrent
    /// push/drop makes publication refuse with [`LogError::Changed`]; restored working changes
    /// remain, so reconcile the fresh history rather than applying again.
    ///
    /// # Errors
    ///
    /// Returns [`LogError::Position`] for an absent ordinal. No mutation occurs.
    pub fn drop(&self, position: usize) -> Result<LogEdit, LogError> {
        let selected = self
            .records
            .len()
            .checked_sub(position.saturating_add(1))
            .ok_or(LogError::Position)?;
        let mut after = Vec::new();
        let mut previous = ObjectId::null(self.format);
        let mut tip = None;
        for (index, record) in self.records.iter().enumerate() {
            if index == selected {
                continue;
            }
            if index > selected {
                after.extend_from_slice(previous.to_string().as_bytes());
                after.extend_from_slice(
                    &record.as_bytes()[ObjectId::null(self.format).to_string().len()..],
                );
            } else {
                after.extend_from_slice(record.as_bytes());
            }
            previous = record.entry().new;
            tip = Some(previous);
        }
        Ok(LogEdit {
            before: self.clone(),
            after: (!after.is_empty()).then_some(after),
            tip,
        })
    }
}

/// Conditional log replacement, separate from save/apply and checkout publication.
#[derive(Clone, Debug)]
pub struct LogEdit {
    before: History,
    after: Option<Vec<u8>>,
    tip: Option<ObjectId>,
}
impl LogEdit {
    /// Publishes the planned stash tip and log, requiring caller-exclusive operation access.
    ///
    /// Uses the existing Git reflog lock and expected-old reference transaction. The reference is
    /// published first, then the log is atomically replaced (or removed). Live readers can observe
    /// the intermediate pair; no cross-file crash transaction or crash durability is promised.
    /// Objects, HEAD, branches, index and working files are untouched. Preserve additive saved
    /// objects after any failure. Never blindly retry after [`LogError::Partial`]; read fresh state
    /// under isolation and reconcile the reported reference effects with the still-readable log.
    ///
    /// # Errors
    ///
    /// Stale snapshots/locks fail before ref publication. A reference transaction failure retains
    /// its own partial receipt; later log failure returns [`LogError::Partial`].
    pub fn publish(&self, repo: &Repository) -> Result<(), LogError> {
        self.publish_with(repo, || Ok(()))
    }

    fn publish_with(
        &self,
        repo: &Repository,
        after_ref: impl FnOnce() -> Result<(), crate::refs::ReferenceError>,
    ) -> Result<(), LogError> {
        if repo.common_dir() != self.before.directory
            || repo.object_format() != self.before.format
            || repo.reference_backend() != Backend::Files
        {
            return Err(LogError::Changed);
        }
        let path = repo.common_dir().join("logs/refs/stash");
        let lock = crate::refs::FileLock::acquire(path.clone())?;
        if read_log(repo.common_dir(), self.before.limits)? != self.before.bytes {
            return Err(LogError::Changed);
        }
        let name = stash_name();
        let expected = self
            .before
            .tip
            .map_or(Expected::Absent, |id| Expected::Value(Target::Direct(id)));
        let edit = RefEdit::new(
            name.clone(),
            self.tip.map(Target::Direct),
            expected,
            Reflog::Preserve,
        );
        if repo.references()?.read(&name)? != self.before.tip.map(Target::Direct) {
            return Err(LogError::Changed);
        }
        lock.check_owned()?;
        repo.references()?.transaction(&[edit])?;
        let publish = || -> Result<(), crate::refs::ReferenceError> {
            after_ref()?;
            lock.check_owned()?;
            if let Some(bytes) = &self.after {
                lock.publish_retaining_lock(bytes)
            } else {
                fs::remove_file(&path).map_err(|source| crate::refs::ReferenceError::Io {
                    path: path.clone(),
                    source,
                })
            }
        };
        publish().map_err(|source| LogError::Partial {
            tip: self.tip,
            source: Box::new(source),
        })
    }
}

/// Stash history observation/planning/publication failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum LogError {
    /// Underlying reference/log framing or filesystem operation failed.
    #[error("stash reference history")]
    Reference(#[from] crate::refs::ReferenceError),
    /// Reference transaction may carry completed effects; inspect its receipt.
    #[error("publishing stash tip")]
    Transaction(#[from] crate::refs::TransactionError),
    /// Complete history exceeded caller limits.
    #[error("stash history limit exceeded")]
    Limit,
    /// Tip/log pair or repository identity changed since observation.
    #[error("stash history changed")]
    Changed,
    /// Ref/log history does not form a supported stash stack.
    #[error("stash tip and reflog are inconsistent")]
    Inconsistent,
    /// The selected newest-first ordinal does not exist.
    #[error("stash position is absent")]
    Position,
    /// Conditional reftable log replacement is not implemented.
    #[error("stash history editing requires files reference storage")]
    Backend,
    /// Ref transaction completed, then log publication failed. The old log remains readable.
    #[error("stash tip published but log publication failed")]
    Partial {
        /// Tip after the completed reference transaction (`None` means deleted).
        tip: Option<ObjectId>,
        /// Log publication failure; re-observe before recovery.
        #[source]
        source: Box<crate::refs::ReferenceError>,
    },
}

fn stash_name() -> RefName {
    RefName::new("refs/stash").expect("fixed valid stash reference")
}

fn read_log(directory: &Path, limits: LogLimits) -> Result<Option<Vec<u8>>, LogError> {
    let mut path = directory.to_path_buf();
    for part in ["logs", "refs", "stash"] {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LogError::Reference(crate::refs::ReferenceError::Io {
                    path,
                    source: io::Error::new(io::ErrorKind::InvalidInput, "stash log symlink"),
                }));
            }
            Ok(metadata) if part == "stash" && !metadata.is_file() => {
                return Err(LogError::Inconsistent);
            }
            Ok(_) => (),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(LogError::Reference(crate::refs::ReferenceError::Io {
                    path,
                    source,
                }));
            }
        }
    }
    let file = fs::File::open(&path).map_err(|source| crate::refs::ReferenceError::Io {
        path: path.clone(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| crate::refs::ReferenceError::Io {
            path: path.clone(),
            source,
        })?
        .is_file()
    {
        return Err(LogError::Inconsistent);
    }
    let mut bytes = Vec::new();
    file.take(limits.max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| crate::refs::ReferenceError::Io { path, source })?;
    if bytes.len() > limits.max_bytes {
        return Err(LogError::Limit);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Commit, InitKind, Tree};

    #[test]
    fn log_failure_after_tip_publication_retains_original_history_and_receipt() {
        let directory = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            ObjectFormat::Sha1,
            directory.path().join("repo"),
            InitKind::Bare,
        )
        .unwrap();
        let writer = Signature {
            name: b"Writer".to_vec(),
            email: b"w@example.com".to_vec(),
            seconds: 1700000000,
            offset_minutes: 0,
        };
        let loose = repo.loose_objects();
        let tree = loose
            .write_tree(&Tree::new(ObjectFormat::Sha1, vec![]).unwrap())
            .unwrap();
        let first = loose
            .write_commit(
                &Commit::builder(tree, writer.clone(), b"first")
                    .build()
                    .unwrap(),
            )
            .unwrap();
        let second = loose
            .write_commit(
                &Commit::builder(tree, writer.clone(), b"second")
                    .build()
                    .unwrap(),
            )
            .unwrap();
        History::read(&repo, LogLimits::default())
            .unwrap()
            .push(first, writer.clone(), b"first".to_vec())
            .unwrap()
            .publish(&repo)
            .unwrap();
        let observed = History::read(&repo, LogLimits::default()).unwrap();
        let plan = observed.push(second, writer, b"second".to_vec()).unwrap();
        let result = plan.publish_with(&repo, || {
            Err(crate::refs::ReferenceError::Io {
                path: repo.common_dir().join("logs/refs/stash"),
                source: io::Error::other("injected log failure"),
            })
        });
        assert!(matches!(result, Err(LogError::Partial { tip: Some(id), .. }) if id == second));
        assert_eq!(
            repo.references().unwrap().read(&stash_name()).unwrap(),
            Some(Target::Direct(second))
        );
        assert_eq!(
            read_log(repo.common_dir(), LogLimits::default()).unwrap(),
            observed.bytes
        );
        assert!(matches!(
            History::read(&repo, LogLimits::default()),
            Err(LogError::Inconsistent)
        ));
        repo.objects().unwrap().read_commit(first).unwrap();
        repo.objects().unwrap().read_commit(second).unwrap();
    }
}
