//! Path-based construction of nested trees.
use std::collections::BTreeMap;

use crate::{EntryMode, LooseObjects, ObjectId, Tree, TreeEntry, TreeError, TreeValue};

/// Builds nested trees from byte paths, composing [`Tree::new`] and
/// [`LooseObjects::write_tree`]. Objects are not resolved; callers supply existing IDs.
/// Setting a path replaces its descendants and any leaf ancestors. Removal prunes empty
/// directories. Edits below opaque stored subtrees are refused; expand those paths first.
/// Memory grows with supplied paths. No index or reference is changed.
///
/// ```
/// use girt::tree::TreeEditor;
/// use girt::{InitKind, ObjectFormat, Repository};
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Bare,
/// )?;
/// let objects = repo.loose_objects();
/// let mut editor = TreeEditor::new(&objects);
/// editor.set(b"src/lib.rs", objects.write_blob(b"pub fn run() {}\n")?)?;
/// editor.set(b"temporary", objects.write_blob(b"discard")?)?;
/// editor.remove(b"temporary")?;
/// let root = objects.read_tree(editor.write()?, 1024)?;
/// assert_eq!(root.entries()[0].name, b"src");
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Debug)]
pub struct TreeEditor<'a> {
    objects: &'a LooseObjects,
    entries: BTreeMap<Vec<u8>, TreeValue>,
}
impl<'a> TreeEditor<'a> {
    /// Starts an empty snapshot in the destination's format.
    pub fn new(objects: &'a LooseObjects) -> Self {
        Self {
            objects,
            entries: BTreeMap::new(),
        }
    }
    /// Sets a regular blob ID. Use [`Self::set_entry`] for other modes.
    ///
    /// # Errors
    ///
    /// Rejects empty, absolute, trailing-slash, NUL, dot or dot-dot components and foreign IDs.
    /// Edits beneath a stored subtree are refused. Failure leaves the editor unchanged.
    pub fn set(&mut self, path: impl AsRef<[u8]>, blob: ObjectId) -> Result<(), TreeWriteError> {
        self.set_entry(
            path,
            TreeValue {
                mode: EntryMode::Blob,
                id: blob,
            },
        )
    }
    /// Sets a mode and ID, including an already stored subtree.
    ///
    /// # Errors
    ///
    /// Returns the validation errors described by [`Self::set`].
    pub fn set_entry(
        &mut self,
        path: impl AsRef<[u8]>,
        value: TreeValue,
    ) -> Result<(), TreeWriteError> {
        let path = path.as_ref();
        validate_path(path)?;
        value
            .id
            .require_format(self.objects.object_format())
            .map_err(TreeError::from)?;
        self.check_ancestors(path)?;
        self.remove_subtree(path);
        let mut ancestor = split_path(path).0;
        while !ancestor.is_empty() {
            self.entries.remove(ancestor);
            ancestor = split_path(ancestor).0;
        }
        self.entries.insert(path.to_vec(), value);
        Ok(())
    }
    /// Removes a path and its descendants. Absent paths are a no-op.
    ///
    /// # Errors
    ///
    /// Rejects invalid paths as described by [`Self::set`], without changing the editor.
    pub fn remove(&mut self, path: impl AsRef<[u8]>) -> Result<(), TreeWriteError> {
        let path = path.as_ref();
        validate_path(path)?;
        self.check_ancestors(path)?;
        self.remove_subtree(path);
        Ok(())
    }
    fn check_ancestors(&self, path: &[u8]) -> Result<(), TreeWriteError> {
        let mut ancestor = split_path(path).0;
        while !ancestor.is_empty() {
            if self
                .entries
                .get(ancestor)
                .is_some_and(|value| value.mode == EntryMode::Tree)
            {
                return Err(TreeWriteError::StoredSubtree {
                    path: ancestor.to_vec(),
                });
            }
            ancestor = split_path(ancestor).0;
        }
        Ok(())
    }

    fn remove_subtree(&mut self, path: &[u8]) {
        self.entries.remove(path);
        let prefix = [path, b"/"].concat();
        let descendants: Vec<_> = self
            .entries
            .range(prefix.clone()..)
            .take_while(|(key, _)| key.starts_with(&prefix))
            .map(|(key, _)| key.clone())
            .collect();
        for key in descendants {
            self.entries.remove(&key);
        }
    }

    /// Stores canonical trees bottom-up and returns the root ID, retaining the editor.
    ///
    /// # Errors
    ///
    /// Returns validation or storage errors. Earlier immutable writes may remain; retrying is
    /// safe under loose-storage rules. No references or index are changed.
    pub fn write(&self) -> Result<ObjectId, TreeWriteError> {
        let mut directories: BTreeMap<Vec<u8>, Vec<TreeEntry>> = BTreeMap::new();
        directories.insert(Vec::new(), Vec::new());
        for (path, value) in &self.entries {
            let (parent, name) = split_path(path);
            directories
                .entry(parent.to_vec())
                .or_default()
                .push(TreeEntry {
                    name: name.to_vec(),
                    mode: value.mode,
                    id: value.id,
                });
            let mut ancestor = parent;
            while !ancestor.is_empty() {
                ancestor = split_path(ancestor).0;
                directories.entry(ancestor.to_vec()).or_default();
            }
        }
        while directories.len() > 1 {
            let Some((path, entries)) = directories.pop_last() else {
                break;
            };
            let tree = Tree::new(self.objects.object_format(), entries)?;
            let id = self.objects.write_tree(&tree)?;
            let (parent, name) = split_path(&path);
            directories
                .entry(parent.to_vec())
                .or_default()
                .push(TreeEntry {
                    name: name.to_vec(),
                    mode: EntryMode::Tree,
                    id,
                });
        }
        let tree = Tree::new(
            self.objects.object_format(),
            directories.remove(b"".as_slice()).unwrap_or_default(),
        )?;
        Ok(self.objects.write_tree(&tree)?)
    }
}
fn split_path(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().rposition(|&byte| byte == b'/') {
        Some(index) => {
            let (parent, suffix) = path.split_at(index);
            (parent, suffix.strip_prefix(b"/").unwrap_or_default())
        }
        None => (&[], path),
    }
}
fn validate_path(path: &[u8]) -> Result<(), TreeWriteError> {
    for name in path.split(|&byte| byte == b'/') {
        if name.is_empty() || name.contains(&0) || name == b"." || name == b".." {
            return Err(TreeError::InvalidName.into());
        }
    }
    Ok(())
}
/// A snapshot cannot be constructed or stored. References and index remain unchanged.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TreeWriteError {
    /// Invalid tree fields.
    #[error(transparent)]
    Tree(#[from] TreeError),
    /// Immutable object publication failed.
    #[error(transparent)]
    Storage(#[from] crate::loose::Error),
    /// An edit traverses an opaque stored tree. Expand it into individual paths first.
    #[error("path traverses a stored subtree")]
    StoredSubtree {
        /// The opaque ancestor path.
        path: Vec<u8>,
    },
}
