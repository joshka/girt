//! Git's default history simplification for path-limited logs.

use std::sync::atomic::AtomicBool;

use crate::pathspec::Pathspec;
use crate::{Commit, ObjectId, Objects, ReadCommitError, TreeCompareError, TreeCompareLimits};

/// How one commit appears in a history limited to a pathspec, as `git log -- <pathspec>` shows it.
///
/// Returned by [`Objects::simplify_commit`]. A walk shows the commit when [`Self::shown`] is true
/// and continues only through [`Self::follow`].
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Simplification {
    /// Whether the commit changes a selected path relative to the parents it is compared with.
    pub shown: bool,
    /// The parents a walk continues through, in parent order.
    pub follow: Vec<ObjectId>,
}

/// A path-limited history step failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SimplifyError {
    /// A parent commit could not be read.
    #[error("reading parent {id}")]
    Parent {
        /// The parent commit.
        id: ObjectId,
        /// Why it could not be read.
        #[source]
        source: ReadCommitError,
    },
    /// Comparing trees failed.
    #[error(transparent)]
    Trees(#[from] TreeCompareError),
}

impl Objects {
    /// Decides whether `commit` appears in `git log -- <paths>` and which parents the walk follows.
    ///
    /// A commit is *TREESAME* to a parent when no selected path differs between their trees.
    /// Following Git's default simplification:
    ///
    /// - a root commit is shown when its tree holds a selected path;
    /// - a commit with one parent is shown unless it is TREESAME to it, and the parent is followed;
    /// - a merge TREESAME to some parent is hidden, and only the first such parent is followed, so
    ///   side branches that did not change the paths are not walked;
    /// - any other merge is shown and every parent is followed.
    ///
    /// Pass `parents` as the IDs the walk should consider, normally [`Commit::parents`]; pass an
    /// empty slice at a shallow boundary, where the commit is compared with an empty tree like a
    /// root. Parent commits are read with [`TreeCompareLimits::read`] to find their trees.
    /// `--full-history`, `--simplify-merges`, parent rewriting and `--follow` are not provided.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::pathspec::{Context, Pathspec};
    /// use girt::tree::TreeEditor;
    /// use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let loose = repo.loose_objects();
    /// let person = Signature {
    ///     name: b"A. Writer".to_vec(),
    ///     email: b"writer@example.com".to_vec(),
    ///     seconds: 1_700_000_000,
    ///     offset_minutes: 0,
    /// };
    /// let mut editor = TreeEditor::new(&loose);
    /// let mut commit = |path: &[u8], parents: Vec<_>| -> Result<_, Box<dyn std::error::Error>> {
    ///     editor.set(path, loose.write_blob(path)?)?;
    ///     let fields = CommitFields {
    ///         tree: editor.write()?,
    ///         parents,
    ///         author: person.clone(),
    ///         committer: person.clone(),
    ///         extra_headers: vec![],
    ///         message: b"change\n".to_vec(),
    ///     };
    ///     Ok(loose.write_commit(&Commit::new(fields)?)?)
    /// };
    /// let first = commit(b"src/lib.rs", vec![])?;
    /// let second = commit(b"docs/guide.md", vec![first])?;
    ///
    /// let objects = repo.objects()?;
    /// let paths = Pathspec::parse(["src"], &Context::new())?;
    /// let cancel = AtomicBool::new(false);
    /// let step = |id| -> Result<_, Box<dyn std::error::Error>> {
    ///     let commit = objects.read_commit(id)?;
    ///     let parents = commit.parents().to_vec();
    ///     Ok(objects.simplify_commit(&commit, &parents, &paths, Default::default(), &cancel)?)
    /// };
    /// // `second` only touched docs, so `git log -- src` skips it and continues to `first`.
    /// assert!(!step(second)?.shown);
    /// assert_eq!(step(second)?.follow, [first]);
    /// assert!(step(first)?.shown);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    ///
    /// # Errors
    ///
    /// Returns [`SimplifyError::Parent`] when a parent cannot be read and
    /// [`SimplifyError::Trees`] when a tree comparison fails, including
    /// [`TreeCompareError::Unsupported`] for `attr:` magic.
    pub fn simplify_commit(
        &self,
        commit: &Commit,
        parents: &[ObjectId],
        paths: &Pathspec,
        limits: TreeCompareLimits,
        cancel: &AtomicBool,
    ) -> Result<Simplification, SimplifyError> {
        let differs = |old: Option<ObjectId>| -> Result<bool, SimplifyError> {
            let changes =
                self.compare_trees_matching(old, Some(commit.tree()), paths, limits, cancel)?;
            Ok(!changes.is_empty())
        };
        let mut changed = Vec::with_capacity(parents.len());
        for &parent in parents {
            let tree = self
                .read_commit_with(parent, limits.read)
                .map_err(|source| SimplifyError::Parent { id: parent, source })?
                .tree();
            let different = differs(Some(tree))?;
            if !different && parents.len() > 1 {
                return Ok(Simplification {
                    shown: false,
                    follow: vec![parent],
                });
            }
            changed.push(different);
        }
        let shown = match parents {
            [] => differs(None)?,
            [_] => changed[0],
            _ => true,
        };
        Ok(Simplification {
            shown,
            follow: parents.to_vec(),
        })
    }
}
