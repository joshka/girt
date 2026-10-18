use std::sync::atomic::AtomicBool;

use super::{Error, Limits, WorktreeChange};
use crate::Repository;

/// What [`Repository::refresh_index_with`] found and recorded.
///
/// ```
/// use girt::{InitKind, ObjectFormat, Repository};
/// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r"),
///     InitKind::Worktree,
/// )?;
///
/// let refresh = repo.refresh_index()?;
/// assert_eq!(refresh.recorded, 0);
/// assert!(!refresh.written);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Refresh {
    /// Entries whose stat words were replaced with those of their unchanged working files.
    pub recorded: usize,
    /// Of [`Self::recorded`], entries whose file changed in or after the second the refresh
    /// began. Their size is recorded as zero, so the next status still reads them, as Git
    /// marks racily clean entries.
    pub racy: usize,
    /// Tracked paths whose working files differ from the index, as configured status reports
    /// them in [`Report::unstaged`](super::Report::unstaged). Their stat words are unchanged.
    pub changes: Vec<WorktreeChange>,
    /// Whether a new index was published; `false` when there was nothing to record.
    pub written: bool,
}

impl Repository {
    /// Records fresh stat words for index entries whose working files are unchanged, as
    /// `git update-index --refresh` does.
    ///
    /// Equivalent to `self.refresh_index_with(None, Limits::default(), &never)` with a flag that
    /// is never set, so file contents are compared without conversion.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::refresh_index_with`] except cancellation.
    pub fn refresh_index(&self) -> Result<Refresh, Error> {
        self.refresh_index_with(None, Limits::default(), &AtomicBool::new(false))
    }

    /// Records fresh stat words for index entries whose working files are unchanged, so later
    /// status calls trust them without reading their contents.
    ///
    /// Status is read-only and never writes back what it learns, so a file whose stat words went
    /// stale (after `touch`, a build tool, or a checkout, which records none) is read again by
    /// every status call. This is the explicit write that fixes that, as
    /// `git update-index --refresh` does.
    ///
    /// The index lock is taken first and held until publication, so other writers see a locked
    /// index for the whole scan. The working tree is then compared with the locked snapshot by
    /// the walk of [`Self::status_with`] (or [`Self::status_with_filters`] when `filters` is
    /// given), honouring the same index flags and configuration: assume-unchanged and
    /// skip-worktree entries are not inspected, intent-to-add entries are never refreshed, and
    /// `core.fileMode`, `core.symlinks`, `core.checkStat` and `core.trustCtime` apply. Entries
    /// that the walk already trusts are left as they are.
    ///
    /// Stat words are recorded only for a stage-zero entry whose file was read and whose
    /// (converted) content and mode equal the entry's. The `lstat` taken before the read is
    /// confirmed after it, so the recorded words describe the hashed bytes. As in Git, a file
    /// whose size differs from the nonzero cached size is reported changed even when its
    /// converted content matches, and is not refreshed. Changed files keep their stat words and
    /// are listed in [`Refresh::changes`].
    ///
    /// Racy entries: a file modified in or after the whole second the refresh began could change
    /// again within that second without changing any stat word, so such an entry is recorded
    /// with a zero size, which every reader rereads. Entries the refresh did not verify keep the
    /// rule of [`IndexEdit::commit`](crate::index::IndexEdit::commit).
    ///
    /// When nothing is recorded the lock is released and the index is not written. Otherwise the
    /// index is published through [`IndexEdit::commit`](crate::index::IndexEdit::commit), which
    /// rechecks that the index bytes are unchanged and replaces the file with one rename. Only
    /// stat words change; no objects or working files are written.
    ///
    /// `limits` bound the walk and the index read as they bound status. Cancellation is observed
    /// as in [`Self::status_with`]; a cancelled refresh writes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Index`] with [`StorageError::Locked`](crate::index::StorageError::Locked)
    /// when another writer holds `index.lock`, leaving the index untouched, and
    /// [`StorageError::Changed`](crate::index::StorageError::Changed) when the index changed
    /// despite the lock. A split index is refused with [`Error::Unsupported`]. Otherwise returns
    /// the walk errors of [`Self::status_with_filters`]. Nothing is written on error.
    ///
    /// ```
    /// use std::time::{Duration, SystemTime};
    ///
    /// use girt::index::{Entry, Mode};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Worktree,
    /// )?;
    /// let path = repo.worktree().unwrap().join("hello.txt");
    /// std::fs::write(&path, b"hello\n")?;
    /// // A file modified in the refresh's own second would be recorded as racy.
    /// let earlier = SystemTime::now() - Duration::from_secs(5);
    /// std::fs::File::options()
    ///     .write(true)
    ///     .open(&path)?
    ///     .set_modified(earlier)?;
    ///
    /// // Staged without stat words, as a checkout leaves it.
    /// let id = repo.loose_objects().write_blob(b"hello\n")?;
    /// let mut edit = repo.edit_index(Default::default())?;
    /// edit.replace_entries(vec![Entry::new(b"hello.txt".to_vec(), Mode::Regular, id)])?;
    /// edit.commit()?;
    ///
    /// let refresh = repo.refresh_index()?;
    /// assert_eq!((refresh.recorded, refresh.written), (1, true));
    /// let index = repo.read_index(Default::default())?.unwrap();
    /// assert_eq!(index.entries()[0].stat.size, 6);
    ///
    /// // Nothing is stale now, so nothing is written.
    /// assert!(!repo.refresh_index()?.written);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn refresh_index_with(
        &self,
        filters: Option<&mut crate::filter::Pipeline>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Refresh, Error> {
        super::types::check(cancel)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return supported::refresh(self, filters, limits, cancel);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (filters, limits);
            Err(Error::Unsupported {
                path: vec![],
                reason: "descriptor-relative worktree traversal requires macOS/Linux",
            })
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod supported {
    use std::os::unix::fs::MetadataExt as _;
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

    use super::{Error, Limits, Refresh};
    use crate::ignore::{Case, Ignore};
    use crate::pathspec::Pathspec;
    use crate::status::UntrackedFiles;
    use crate::status::types::{charge, check};
    use crate::status::worktree::{self, Listing, Shared};
    use crate::{Repository, index};

    pub(super) fn refresh(
        repo: &Repository,
        filters: Option<&mut crate::filter::Pipeline>,
        mut limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Refresh, Error> {
        let root = repo.worktree().ok_or_else(|| Error::Unsupported {
            path: vec![],
            reason: if repo.is_bare() {
                "bare repository has no working tree"
            } else {
                "worktree location is unknown"
            },
        })?;
        let names = crate::status::workflow::supported::git_names(repo)?;
        let edit = repo.edit_index(limits.index)?;
        // The edit releases its lock when dropped, including on every early return below.
        if edit.original_missing() {
            edit.abort()?;
            return Ok(Refresh::default());
        }
        if edit
            .index()
            .extensions()
            .iter()
            .any(|extension| extension.signature() == *b"link")
        {
            edit.abort()?;
            return Err(Error::Unsupported {
                path: vec![],
                reason: "refreshing a split index",
            });
        }
        // The lock excludes cooperating writers, so the file now holds the snapshot's bytes;
        // publication rechecks them. Its mtime is the racy cutoff for trusting cached words.
        let index_path = repo.git_dir().join("index");
        let index_written = std::fs::symlink_metadata(&index_path)
            .map_err(|source| Error::Io {
                path: b"index".to_vec(),
                source,
            })?
            .mtime() as u32;
        let started = edit
            .locked_at()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                u32::try_from(since.as_secs()).unwrap_or(u32::MAX)
            });
        let entries = edit.index().entries();
        for entry in entries {
            check(cancel)?;
            worktree::validate_path(&entry.path)?;
            charge(&mut limits.max_path_bytes, entry.path.len(), "path bytes")?;
        }
        let objects = repo.objects_with(limits.packs).map_err(Error::Objects)?;
        let environment = filters
            .as_ref()
            .and_then(|filters| filters.environment())
            .cloned()
            .unwrap_or_default();
        let shared = filters.map(|pipeline| Shared {
            pipeline: Mutex::new(pipeline),
            objects: &objects,
        });
        // Untracked paths are not listed, so ignore rules are never consulted.
        let listing = Listing::Ignoring {
            rules: Box::new(Ignore::new(Case::Sensitive, Default::default())),
            files: UntrackedFiles::Omit,
            names,
            environment: Box::new(environment),
        };
        let observed = worktree::scan(
            repo,
            root,
            entries,
            Some(index_written),
            listing,
            &Pathspec::all(),
            shared.as_ref().map(|shared| shared as _),
            limits,
            cancel,
        )?;
        check(cancel)?;
        let racy = |stat: &index::Stat| stat.mtime.seconds >= started;
        // A file reread only because its index was racy can already hold these exact words.
        let verified: Vec<_> = observed
            .verified
            .into_iter()
            .filter(|(position, stat)| {
                let recorded = index::Stat {
                    size: if racy(stat) { 0 } else { stat.size },
                    ..*stat
                };
                entries
                    .get(*position)
                    .is_some_and(|entry| entry.stat != recorded)
            })
            .collect();
        let mut refresh = Refresh {
            recorded: verified.len(),
            racy: verified.iter().filter(|(_, stat)| racy(stat)).count(),
            changes: observed.changes,
            written: false,
        };
        if verified.is_empty() {
            edit.abort()?;
            return Ok(refresh);
        }
        edit.commit_verified_stat(&verified, started)?;
        refresh.written = true;
        Ok(refresh)
    }
}
