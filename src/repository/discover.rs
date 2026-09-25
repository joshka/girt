//! Parent traversal with an optional inclusive ceiling.
use std::path::Path;

use super::{OpenError, Repository, canonical, io_error, malformed};

impl Repository {
    /// Finds the nearest repository at or above an existing directory.
    ///
    /// Searches the canonical start and its parents through the filesystem root, crossing mount
    /// points. Symlinks therefore follow physical ancestry. An explicit `.git` entry, or `HEAD`
    /// together with both `objects` and `refs` (or linked `commondir`), marks a candidate.
    /// Lone ordinary names and partial bare layouts allow ancestor search; recognized malformed
    /// or unsupported candidates stop the search. Ordinary,
    /// bare, separate-Git-directory and linked-worktree layouts use [`Self::open`]'s rules.
    /// No environment variables, ownership checks, or Git discovery configuration are consulted.
    /// Use [`Self::discover_with_ceiling`] to restrict the ancestor search.
    ///
    /// # Errors
    ///
    /// Returns [`OpenError::NotFound`] for an absent start or no candidate, and
    /// [`OpenError::Malformed`] for a non-directory start. Candidate errors are propagated without
    /// falling back to an outer repository. No files are changed. Reads are synchronous and have
    /// the same unbounded metadata allocation and trusted-path assumptions as [`Self::open`].
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, OpenError> {
        discover(start.as_ref(), None)
    }

    /// Finds a repository between an existing directory and an inclusive ancestor ceiling.
    ///
    /// Both paths are canonicalized before testing ancestry. The ceiling itself is searched, but
    /// its parents are not. The ceiling limits candidate locations; gitfiles and linked-worktree
    /// metadata may point outside it. All other rules are those of [`Self::discover`].
    ///
    /// # Errors
    ///
    /// In addition to [`Self::discover`]'s failures, returns [`OpenError::Malformed`] if the
    /// ceiling is not an ancestor of the start (or the same directory), and I/O errors for
    /// inaccessible paths. An absent ceiling returns [`OpenError::NotFound`]. No files are
    /// changed.
    pub fn discover_with_ceiling(
        start: impl AsRef<Path>,
        ceiling: impl AsRef<Path>,
    ) -> Result<Self, OpenError> {
        discover(start.as_ref(), Some(ceiling.as_ref()))
    }
}

fn discover(start: &Path, ceiling: Option<&Path>) -> Result<Repository, OpenError> {
    let start = directory(start)?;
    let ceiling = ceiling.map(directory).transpose()?;
    if ceiling
        .as_ref()
        .is_some_and(|ceiling| !start.starts_with(ceiling))
    {
        return Err(malformed(
            ceiling.as_deref().unwrap(),
            "discovery ceiling is not an ancestor",
        ));
    }
    for candidate in start.ancestors() {
        if is_candidate(candidate)? {
            return Repository::open(candidate);
        }
        if ceiling.as_deref() == Some(candidate) {
            break;
        }
    }
    Err(OpenError::NotFound(start))
}

fn directory(path: &Path) -> Result<std::path::PathBuf, OpenError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => canonical(path),
        Ok(_) => Err(malformed(path, "discovery requires a directory")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(OpenError::NotFound(path.into()))
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn is_candidate(path: &Path) -> Result<bool, OpenError> {
    Ok(marker_exists(path, ".git")?
        || (marker_exists(path, "HEAD")?
            && ((marker_exists(path, "objects")? && marker_exists(path, "refs")?)
                || marker_exists(path, "commondir")?)))
}

fn marker_exists(path: &Path, name: &str) -> Result<bool, OpenError> {
    let marker = path.join(name);
    match std::fs::symlink_metadata(&marker) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(&marker, error)),
    }
}

// Initialization deliberately refuses even ambiguous markers before writing anything.
pub(super) fn has_marker(path: &Path) -> Result<bool, OpenError> {
    for name in [".git", "HEAD", "objects"] {
        if marker_exists(path, name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::InitKind;

    #[test]
    fn ceiling_is_inclusive_and_stops_parent_search() {
        let root = tempfile::tempdir().unwrap();
        let outer =
            Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        assert!(matches!(
            Repository::discover_with_ceiling(&child, &child),
            Err(OpenError::NotFound(_))
        ));
        assert_eq!(
            Repository::discover_with_ceiling(&child, root.path())
                .unwrap()
                .git_dir(),
            outer.git_dir()
        );
    }

    #[test]
    fn nearest_repository_wins() {
        let root = tempfile::tempdir().unwrap();
        Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
        let inner = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("inner"),
            InitKind::Worktree,
        )
        .unwrap();
        assert_eq!(
            Repository::discover(inner.worktree().unwrap())
                .unwrap()
                .git_dir(),
            inner.git_dir()
        );
    }

    #[test]
    fn rejects_non_ancestor_ceiling() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        assert!(matches!(
            Repository::discover_with_ceiling(root.path(), &child),
            Err(OpenError::Malformed { .. })
        ));
    }

    #[test]
    fn malformed_inner_marker_prevents_outer_fallback() {
        let root = tempfile::tempdir().unwrap();
        Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join(".git"), b"invalid\n").unwrap();
        assert!(matches!(
            Repository::discover(&child),
            Err(OpenError::Malformed { .. })
        ));
        assert_eq!(std::fs::read(child.join(".git")).unwrap(), b"invalid\n");
    }

    #[test]
    fn start_must_be_an_existing_directory() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("file");
        std::fs::write(&file, b"keep").unwrap();
        assert!(matches!(
            Repository::discover(&file),
            Err(OpenError::Malformed { .. })
        ));
        assert!(matches!(
            Repository::discover(root.path().join("missing")),
            Err(OpenError::NotFound(_))
        ));
    }

    #[test]
    fn empty_search_is_read_only() {
        let root = tempfile::tempdir().unwrap();
        assert!(matches!(
            Repository::discover_with_ceiling(root.path(), root.path()),
            Err(OpenError::NotFound(_))
        ));
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_start_uses_physical_ancestry() {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("physical"),
            InitKind::Worktree,
        )
        .unwrap();
        let nested = root.path().join("physical/nested");
        std::fs::create_dir(&nested).unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(&nested, &alias).unwrap();
        assert_eq!(
            Repository::discover(&alias).unwrap().git_dir(),
            repo.git_dir()
        );
        assert_eq!(
            Repository::discover_with_ceiling(&alias, repo.worktree().unwrap())
                .unwrap()
                .git_dir(),
            repo.git_dir()
        );
    }

    #[cfg(unix)]
    #[test]
    fn dangling_gitfile_symlink_stops_search() {
        let root = tempfile::tempdir().unwrap();
        Repository::init(crate::ObjectFormat::Sha1, root.path(), InitKind::Worktree).unwrap();
        let child = root.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::os::unix::fs::symlink("missing", child.join(".git")).unwrap();
        assert!(Repository::discover(&child).is_err());
    }
}
