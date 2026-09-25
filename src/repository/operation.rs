//! Bounded snapshots of worktree-private operation metadata.
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::Repository;

/// Recognized metadata roots, in cleanup order. Auxiliary files can survive interrupted commands.
///
/// `BISECT_LOG` is inspected for compatibility with colocation consumers; deleting it does not
/// implement `git bisect reset`. ORIG_HEAD, refs, logs, index and working files are never selected.
const ROOTS: &[&str] = &[
    "MERGE_HEAD",
    "MERGE_MODE",
    "MERGE_MSG",
    "AUTO_MERGE",
    "REVERT_HEAD",
    "CHERRY_PICK_HEAD",
    "REBASE_HEAD",
    "BISECT_LOG",
    "rebase-merge",
    "rebase-apply",
    "sequencer",
];

/// Resource bounds for operation metadata, including directory contents.
#[derive(Clone, Copy, Debug)]
pub struct OperationLimits {
    /// Total regular-file bytes retained in a snapshot.
    pub max_bytes: usize,
    /// Total files and directories retained.
    pub max_entries: usize,
    /// Maximum directory nesting beneath a recognized root (root depth is zero), capped at 128.
    pub max_depth: usize,
}

impl Default for OperationLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_entries: 4096,
            max_depth: 32,
        }
    }
}

/// Exact recognized metadata snapshot, tied to its worktree Git directory.
///
/// Presence is reported without interpreting payloads or choosing precedence among simultaneous
/// states. Files remain bytes; even malformed command metadata can be inspected and discarded.
/// Symlinks and special nodes are refused, including inside recognized directories.
#[derive(Debug)]
pub struct OperationState {
    directory: PathBuf,
    limits: OperationLimits,
    nodes: BTreeMap<PathBuf, Node>,
}

#[derive(Debug, Eq, PartialEq)]
enum Node {
    File(Vec<u8>),
    Directory,
}

/// Inspection or cleanup failure with a concrete recovery boundary.
#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    /// Filesystem access failed; no inspection operation writes anything.
    #[error("cannot access operation metadata {path}: {source}")]
    Io {
        /// Affected path.
        path: PathBuf,
        /// Operating-system cause.
        #[source]
        source: io::Error,
    },
    /// A symlink, special node or unexpected root type was encountered.
    #[error("unsupported operation metadata node: {0}")]
    Unsupported(PathBuf),
    /// A snapshot exceeded a caller-supplied bound.
    #[error("operation metadata limit exceeded at {0}")]
    Limit(PathBuf),
    /// Contents or presence changed after inspection; nothing was deleted.
    #[error("operation metadata changed since inspection")]
    Changed,
}

/// Cleanup stopped after these exact removals; retry requires inspecting current state again.
#[derive(Debug, thiserror::Error)]
#[error("operation cleanup failed after {} removals: {source}", removed.len())]
pub struct OperationCleanupError {
    /// Successfully removed paths relative to this worktree's Git directory, in deletion order.
    /// Empty directories count as removals too. Earlier removals are never rolled back.
    pub removed: Vec<PathBuf>,
    /// Failure preventing further cleanup.
    #[source]
    pub source: OperationError,
}

impl Repository {
    /// Snapshots recognized operation metadata from this worktree's Git directory.
    ///
    /// Includes merge, cherry-pick/revert, rebase-merge/rebase-apply and sequencer metadata,
    /// plus auxiliary AUTO_MERGE, REBASE_HEAD and BISECT_LOG. Unknown files outside these roots
    /// remain untouched; all regular descendants inside recognized directories belong to them.
    /// No environment overrides, payload parsing, state precedence or working-file reset occurs.
    /// Linked worktrees use private metadata, never the common directory.
    ///
    /// # Errors
    ///
    /// Returns [`OperationError`] on I/O, unsupported node types or exhausted bounds. Callers
    /// must exclude concurrent operation writers for a coherent snapshot; Git has no common
    /// operation-state lock. Repository directories must be trusted and stable.
    pub fn operation_state(
        &self,
        limits: OperationLimits,
    ) -> Result<OperationState, OperationError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "operation.inspect", outcome = "incomplete", failure_class = tracing::field::Empty);
        let operation = || OperationState::read(self.git_dir().to_path_buf(), limits);
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        crate::trace::finish(&span, &result, classify);
        result
    }
}

impl OperationState {
    fn read(directory: PathBuf, limits: OperationLimits) -> Result<Self, OperationError> {
        let mut state = Self {
            directory,
            limits,
            nodes: BTreeMap::new(),
        };
        let mut remaining = limits.max_bytes;
        for name in ROOTS {
            let path = Path::new(name);
            match fs::symlink_metadata(state.directory.join(path)) {
                Ok(metadata) => {
                    let directory_root =
                        matches!(*name, "rebase-merge" | "rebase-apply" | "sequencer");
                    if metadata.is_dir() != directory_root {
                        return Err(OperationError::Unsupported(state.directory.join(path)));
                    }
                    state.visit(path, 0, &mut remaining)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(io_error(state.directory.join(path), source)),
            }
        }
        Ok(state)
    }

    fn visit(
        &mut self,
        relative: &Path,
        depth: usize,
        remaining: &mut usize,
    ) -> Result<(), OperationError> {
        let path = self.directory.join(relative);
        if depth > self.limits.max_depth.min(128) || self.nodes.len() >= self.limits.max_entries {
            return Err(OperationError::Limit(path));
        }
        let metadata = fs::symlink_metadata(&path).map_err(|e| io_error(path.clone(), e))?;
        if metadata.is_dir() {
            self.nodes.insert(relative.to_path_buf(), Node::Directory);
            let entries = fs::read_dir(&path).map_err(|e| io_error(path.clone(), e))?;
            for entry in entries {
                let entry = entry.map_err(|e| io_error(path.clone(), e))?;
                self.visit(&relative.join(entry.file_name()), depth + 1, remaining)?;
            }
        } else if metadata.is_file() {
            let file = fs::File::open(&path).map_err(|e| io_error(path.clone(), e))?;
            let mut bytes = Vec::new();
            file.take((*remaining as u64).saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|e| io_error(path.clone(), e))?;
            if bytes.len() > *remaining {
                return Err(OperationError::Limit(path));
            }
            *remaining -= bytes.len();
            self.nodes.insert(relative.to_path_buf(), Node::File(bytes));
        } else {
            return Err(OperationError::Unsupported(path));
        }
        Ok(())
    }

    /// Whether none of the recognized metadata roots were present.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Lists present recognized root names without choosing a single operation classification.
    pub fn roots(&self) -> impl Iterator<Item = &'static str> + '_ {
        ROOTS
            .iter()
            .copied()
            .filter(|name| self.nodes.contains_key(Path::new(name)))
    }

    /// Returns exact regular-file bytes at a relative snapshot path, including nested metadata.
    /// Directories, absent paths and paths outside the snapshot return `None`.
    pub fn file(&self, path: &Path) -> Option<&[u8]> {
        match self.nodes.get(path) {
            Some(Node::File(bytes)) => Some(bytes),
            _ => None,
        }
    }

    /// Rechecks the complete snapshot, then removes inspected nodes, children before parents.
    ///
    /// The caller must exclude Git/jj operation writers for the entire inspection and cleanup
    /// lifecycle. There is no shared lock protocol: the byte/presence recheck detects observed
    /// changes but cannot prevent a write after comparison or detect an ABA replacement. Trusted,
    /// stable metadata directories are required. No recursive deletion follows links. Unknown
    /// newly created children prevent directory removal, and successful deletions are not undone.
    /// This discards command metadata only; it does not abort a merge/rebase or reset any files,
    /// HEAD, refs or index. No crash durability is promised.
    ///
    /// # Errors
    ///
    /// Preflight failure removes nothing. Later filesystem failure returns exact completed
    /// removals through [`OperationCleanupError`]. Inspect again before deciding how to recover.
    pub fn cleanup(self) -> Result<Vec<PathBuf>, OperationCleanupError> {
        #[cfg(feature = "tracing")]
        let span = tracing::debug_span!(target: "girt", "operation.cleanup", outcome = "incomplete", failure_class = tracing::field::Empty, removed = tracing::field::Empty);
        let operation = || self.cleanup_with(|_| Ok(()));
        #[cfg(feature = "tracing")]
        let result = span.in_scope(operation);
        #[cfg(not(feature = "tracing"))]
        let result = operation();
        #[cfg(feature = "tracing")]
        {
            let removed = match &result {
                Ok(paths) => paths.len(),
                Err(error) => error.removed.len(),
            };
            span.record("removed", removed);
            crate::trace::finish(&span, &result, |error| classify(&error.source));
        }
        result
    }

    fn cleanup_with(
        self,
        mut before_remove: impl FnMut(&Path) -> io::Result<()>,
    ) -> Result<Vec<PathBuf>, OperationCleanupError> {
        let current = Self::read(self.directory.clone(), self.limits).map_err(|source| {
            OperationCleanupError {
                removed: Vec::new(),
                source,
            }
        })?;
        if current.nodes != self.nodes {
            return Err(OperationCleanupError {
                removed: Vec::new(),
                source: OperationError::Changed,
            });
        }
        let mut removed = Vec::new();
        for (relative, node) in self.nodes.iter().rev() {
            let path = self.directory.join(relative);
            let result = before_remove(relative).and_then(|()| match node {
                Node::File(_) => fs::remove_file(&path),
                Node::Directory => fs::remove_dir(&path),
            });
            if let Err(source) = result {
                return Err(OperationCleanupError {
                    removed,
                    source: io_error(path, source),
                });
            }
            removed.push(relative.clone());
        }
        Ok(removed)
    }
}

fn io_error(path: PathBuf, source: io::Error) -> OperationError {
    OperationError::Io { path, source }
}

#[cfg(feature = "tracing")]
fn classify(error: &OperationError) -> &'static str {
    match error {
        OperationError::Io { .. } => "io",
        OperationError::Unsupported(_) => "unsupported",
        OperationError::Limit(_) => "limit",
        OperationError::Changed => "conflict",
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn fixture() -> (tempfile::TempDir, OperationState) {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("MERGE_HEAD"), b"opaque\n").unwrap();
        fs::create_dir(root.path().join("sequencer")).unwrap();
        fs::write(root.path().join("sequencer/todo"), b"pick\n").unwrap();
        let state =
            OperationState::read(root.path().to_path_buf(), OperationLimits::default()).unwrap();
        (root, state)
    }

    #[test]
    fn removes_only_snapshot_roots_and_descendants() {
        let (root, state) = fixture();
        fs::write(root.path().join("ORIG_HEAD"), b"keep").unwrap();
        assert_eq!(
            state.roots().collect::<Vec<_>>(),
            ["MERGE_HEAD", "sequencer"]
        );
        assert_eq!(
            state.file(Path::new("sequencer/todo")),
            Some(b"pick\n".as_slice())
        );
        let removed = state.cleanup().unwrap();
        assert_eq!(
            removed,
            [
                PathBuf::from("sequencer/todo"),
                PathBuf::from("sequencer"),
                PathBuf::from("MERGE_HEAD")
            ]
        );
        assert_eq!(fs::read(root.path().join("ORIG_HEAD")).unwrap(), b"keep");
    }

    #[rstest]
    #[case::existing("MERGE_HEAD")]
    #[case::new_root("CHERRY_PICK_HEAD")]
    #[case::new_child("sequencer/new")]
    fn rejects_changed_snapshot_without_deletion(#[case] path: &str) {
        let (root, state) = fixture();
        fs::write(root.path().join(path), b"concurrent").unwrap();
        let error = state.cleanup().unwrap_err();
        assert!(matches!(error.source, OperationError::Changed));
        assert!(error.removed.is_empty());
        assert_eq!(fs::read(root.path().join(path)).unwrap(), b"concurrent");
        assert!(root.path().join("sequencer/todo").exists());
    }

    #[test]
    fn reports_exact_partial_removals() {
        let (root, state) = fixture();
        let error = state
            .cleanup_with(|path| {
                if path == Path::new("sequencer") {
                    Err(io::Error::other("injected removal failure"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error.removed, [PathBuf::from("sequencer/todo")]);
        assert!(root.path().join("sequencer").exists());
        assert!(root.path().join("MERGE_HEAD").exists());
    }

    #[rstest]
    #[case::bytes(OperationLimits { max_bytes: 0, ..OperationLimits::default() })]
    #[case::entries(OperationLimits { max_entries: 0, ..OperationLimits::default() })]
    #[case::depth(OperationLimits { max_depth: 0, ..OperationLimits::default() })]
    fn bounded_inspection_preserves_files(#[case] limits: OperationLimits) {
        let (root, _) = fixture();
        let error = OperationState::read(root.path().to_path_buf(), limits).unwrap_err();
        assert!(matches!(error, OperationError::Limit(_)));
        assert!(root.path().join("sequencer/todo").exists());
    }

    #[test]
    fn unexpected_root_type_is_refused() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("MERGE_HEAD")).unwrap();
        let error = OperationState::read(root.path().to_path_buf(), OperationLimits::default())
            .unwrap_err();
        assert!(matches!(error, OperationError::Unsupported(_)));
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_nested_symlinks() {
        let (root, _) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("keep"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("sequencer/link")).unwrap();
        let error = OperationState::read(root.path().to_path_buf(), OperationLimits::default())
            .unwrap_err();
        assert!(matches!(error, OperationError::Unsupported(_)));
        assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"keep");
    }
}
