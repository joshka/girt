//! Tree identity derived from validated index entries, without consulting stored objects.
use super::{Index, Mode, Stage};
use crate::{EntryMode, ObjectId, Tree, TreeEntry};

impl Index {
    /// Stores the staged snapshot as nested trees with default pack-opening limits.
    ///
    /// Wraps object lookup, `Tree::new` and loose tree writes. Like `git write-tree`, rejects
    /// missing objects (except gitlinks), omits intent-to-add entries and retains collapsed sparse
    /// directory IDs. Object presence is checked, not recursive connectivity or object kind.
    ///
    /// # Errors
    ///
    /// Returns the completeness, lookup and storage errors described by [`Self::write_tree_with`].
    pub fn write_tree(&self, repo: &crate::Repository) -> Result<ObjectId, WriteTreeError> {
        self.write_tree_with(
            repo,
            crate::PackLimits::default(),
            &std::sync::atomic::AtomicBool::new(false),
        )
    }

    /// Stores the staged snapshot with explicit pack-opening limits and cancellation.
    ///
    /// # Errors
    ///
    /// Rejects unmerged entries, non-placeholder null IDs, foreign formats, absent objects or
    /// cancellation. Lookup errors preserve their causes. All entries are checked before writing
    /// any trees. A storage failure may leave immutable subtrees; retrying is safe under loose
    /// storage rules. The index and references remain unchanged. Callers needing a stable object
    /// set must exclude concurrent object retirement.
    pub fn write_tree_with(
        &self,
        repo: &crate::Repository,
        limits: crate::PackLimits,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Result<ObjectId, WriteTreeError> {
        let check = || {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                Err(WriteTreeError::Cancelled)
            } else {
                Ok(())
            }
        };
        check()?;
        ObjectId::null(self.format).require_format(repo.object_format())?;
        let objects = repo.objects(limits)?;
        for entry in &self.entries {
            check()?;
            if entry.stage != Stage::Normal {
                return Err(WriteTreeError::Unmerged {
                    path: entry.path.clone(),
                });
            }
            if entry.intent_to_add {
                continue;
            }
            if entry.id.is_null() {
                return Err(WriteTreeError::NullId {
                    path: entry.path.clone(),
                });
            }
            if entry.mode != Mode::Gitlink && !objects.contains(entry.id)? {
                return Err(WriteTreeError::MissingObject {
                    path: entry.path.clone(),
                    id: entry.id,
                });
            }
        }
        let loose = repo.loose_objects();
        let mut editor = crate::tree::TreeEditor::new(&loose);
        for entry in &self.entries {
            check()?;
            if entry.intent_to_add {
                continue;
            }
            let mode = match entry.mode {
                Mode::Regular => EntryMode::Blob,
                Mode::Executable => EntryMode::Executable,
                Mode::Symlink => EntryMode::Symlink,
                Mode::Gitlink => EntryMode::Gitlink,
                Mode::SparseDirectory => EntryMode::Tree,
            };
            let path = entry.path.strip_suffix(b"/").unwrap_or(&entry.path);
            editor.set_entry(path, crate::TreeValue { mode, id: entry.id })?;
        }
        check()?;
        Ok(editor.write()?)
    }

    /// Computes the canonical Git tree identity without reading or writing objects.
    ///
    /// Like `git write-tree`, omits intent-to-add entries and uses collapsed sparse directory
    /// identities directly. Stat data, assume-valid, skip-worktree and optional extensions do not
    /// affect the result. Returns `None` for unmerged entries or non-placeholder null IDs.
    /// Object existence and type are not checked; this is an identity calculation, not proof of
    /// repository connectivity. Memory is proportional to the admitted index, and directory
    /// nesting uses an explicit stack rather than recursion.
    ///
    /// ```
    /// use girt::index::Index;
    /// use girt::{ObjectFormat, Tree};
    /// let index = Index::empty(ObjectFormat::Sha1);
    /// assert_eq!(
    ///     index.tree_id(),
    ///     Some(Tree::new(ObjectFormat::Sha1, vec![])?.id())
    /// );
    /// # Ok::<(), girt::TreeError>(())
    /// ```
    pub fn tree_id(&self) -> Option<ObjectId> {
        let mut directories = vec![Directory {
            name: &[],
            entries: Vec::new(),
        }];
        for entry in &self.entries {
            if entry.stage != Stage::Normal {
                return None;
            }
            if entry.intent_to_add {
                continue;
            }
            if entry.id.is_null() {
                return None;
            }
            let path = entry.path.strip_suffix(b"/").unwrap_or(&entry.path);
            let mut components: Vec<_> = path.split(|&b| b == b'/').collect();
            let name = components.pop()?;
            let common = directories
                .iter()
                .skip(1)
                .zip(&components)
                .take_while(|(directory, component)| directory.name == **component)
                .count();
            while directories.len() > common + 1 {
                let tree = directories.pop()?.finish(self.format)?;
                directories.last_mut()?.entries.push(tree);
            }
            for &name in &components[common..] {
                directories.push(Directory {
                    name,
                    entries: Vec::new(),
                });
            }
            let mode = match entry.mode {
                Mode::Regular => EntryMode::Blob,
                Mode::Executable => EntryMode::Executable,
                Mode::Symlink => EntryMode::Symlink,
                Mode::Gitlink => EntryMode::Gitlink,
                Mode::SparseDirectory => EntryMode::Tree,
            };
            directories.last_mut()?.entries.push(TreeEntry {
                name: name.to_vec(),
                mode,
                id: entry.id,
            });
        }
        while directories.len() > 1 {
            let tree = directories.pop()?.finish(self.format)?;
            directories.last_mut()?.entries.push(tree);
        }
        Some(directories.pop()?.finish(self.format)?.id)
    }
}

/// An index snapshot is incomplete, its objects are unavailable, or tree publication failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WriteTreeError {
    /// An index path has conflict stages.
    #[error("index contains an unmerged path")]
    Unmerged {
        /// Conflicted byte path.
        path: Vec<u8>,
    },
    /// A staged entry has no usable object ID.
    #[error("index contains a null object ID")]
    NullId {
        /// Incomplete byte path.
        path: Vec<u8>,
    },
    /// A referenced object is absent from the repository and alternates.
    #[error("index object {id} is missing")]
    MissingObject {
        /// Staged byte path.
        path: Vec<u8>,
        /// Absent object identity.
        id: ObjectId,
    },
    /// Object lookup or pack opening failed.
    #[error(transparent)]
    Read(#[from] crate::ObjectReadError),
    /// Canonical tree construction or immutable publication failed.
    #[error(transparent)]
    Write(#[from] crate::tree::TreeWriteError),
    /// Index and destination object formats differ.
    #[error(transparent)]
    ObjectFormat(#[from] crate::ObjectFormatError),
    /// Cancellation was requested. No index or reference changed.
    #[error("writing index tree cancelled")]
    Cancelled,
}

struct Directory<'a> {
    name: &'a [u8],
    entries: Vec<TreeEntry>,
}

impl Directory<'_> {
    fn finish(self, format: crate::ObjectFormat) -> Option<TreeEntry> {
        let tree = Tree::new(format, self.entries).ok()?;
        Some(TreeEntry {
            name: self.name.to_vec(),
            mode: EntryMode::Tree,
            id: tree.id(),
        })
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::ObjectFormat;
    use crate::index::{Entry, Limits};

    #[rstest]
    #[case::unmerged(Stage::Ours, ObjectId::for_blob(ObjectFormat::Sha1, b"x"))]
    #[case::null(Stage::Normal, ObjectId::null(ObjectFormat::Sha1))]
    fn incomplete_index_has_no_tree_identity(#[case] stage: Stage, #[case] id: ObjectId) {
        let index = Index::new(
            ObjectFormat::Sha1,
            vec![Entry::new(b"file".to_vec(), Mode::Regular, id).with_stage(stage)],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(index.tree_id(), None);
    }

    #[test]
    fn null_intent_to_add_is_omitted() {
        let format = ObjectFormat::Sha1;
        let index = Index::new(
            format,
            vec![
                Entry::new(
                    b"future/file".to_vec(),
                    Mode::Regular,
                    ObjectId::null(format),
                )
                .with_intent_to_add(true),
            ],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(index.tree_id(), Index::empty(format).tree_id());
    }
}
