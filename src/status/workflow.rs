use std::sync::atomic::AtomicBool;

use super::{Baseline, Error, Limits, Report, Untracked, UntrackedFiles};
use crate::Repository;
use crate::ignore::WorktreeRules;
use crate::pathspec::Pathspec;

impl Repository {
    /// Compares a baseline tree with the index, then verifies literal working-tree content.
    ///
    /// This is a **raw-content** API: attributes, filters, EOL conversion, ignores and config
    /// overrides of executable/symlink behavior are not applied. Every result records this policy.
    /// Use [`Untracked::Omit`] for tracked-only inspection, or explicitly request raw untracked
    /// leaves. No filesystem writes or index refresh occur; ordinary reads may update access times
    /// according to the filesystem.
    ///
    /// A working file is not read when the `lstat` taken during traversal matches every cached
    /// stat word of its stage-zero entry (size, mtime and ctime with nanoseconds, device, inode,
    /// uid and gid) and its mode. As in Git, a racily clean entry is read instead: one whose cached
    /// mtime or ctime falls in or after the second the index file was last modified, when a later
    /// change in that second could leave every word equal. A zero cached size and a split index
    /// always force reads. A trusted entry therefore compares equal even when its raw bytes differ
    /// from the index blob because Git stored it through a filter or EOL conversion, as
    /// `git status` reports it. Assume-valid is ignored. Like Git, status compares identities
    /// without reading index blobs, and skips baseline subtrees whose identity equals the
    /// index's.
    ///
    /// Intent-to-add and skip-worktree entries are refused pending explicit consumer policy.
    ///
    /// `paths` limits the report to paths the [`Pathspec`] selects, as `git status -- <pathspec>`
    /// does; pass [`Pathspec::all`] for the whole worktree. Directories the pathspec cannot reach
    /// are not entered and unselected files are not read. A pathspec with `attr:` magic is
    /// refused with [`Error::Unsupported`].
    ///
    /// macOS/Linux traversal opens components relative to directory descriptors with no-follow
    /// flags. It never follows symlink ancestors, enters `.git` components or descends into
    /// gitlinks/nested repositories. Exact directory names must match index bytes, including case.
    /// Names are compared as bytes, so a name the volume merges with an index path, such as
    /// `readme` for `README` on a case-insensitive volume, reports a deletion and an untracked
    /// path. Linux retains non-UTF-8 bytes. Colons and backslashes are ordinary name bytes. Both
    /// reject NUL, empty/dot/parent components and case-insensitive `.git` components.
    /// Windows and other
    /// platforms return unsupported before accessing storage. Worktree files must live on a
    /// filesystem supporting native POSIX modes and symlinks; emulation/normalization is excluded.
    ///
    /// File identity, size, mode, mtime and ctime are compared around reads; a trusted file is
    /// observed through its one `lstat`, as in Git. Directory metadata is
    /// checked around traversal; HEAD (when requested) and index are reread before returning.
    /// Observed changes fail. These checks cannot detect all changes, ABA replacements, or changes
    /// after inspection: the report is not an atomic snapshot and must never authorize checkout
    /// overwrites. Open directory descriptors pin inspected directories even if concurrently moved.
    /// Mount changes, hostile hardlinks and repository-metadata replacement are outside the trust
    /// boundary. Metadata/object/reference reads inherit the existing storage APIs' assumptions;
    /// in particular reference resolution has a depth bound but no byte budget.
    ///
    /// Work is synchronous: the call returns once up to [`Limits::max_threads`] scoped threads,
    /// the caller's included, have walked the working tree. Cancellation is checked between
    /// entries, objects, directories and 64-KiB file-read chunks. One syscall, object read, index
    /// parse, tree parse or sort cannot be interrupted. The caller owns blocking-worker scheduling;
    /// there is no wall-clock guarantee. Memory is bounded by index/tree/pack limits plus visited
    /// path records, the index's own trees and one file buffer per thread. Directory budgets are
    /// shared by every thread. Each thread holds about `min(max_depth, 128) + 1` directory
    /// descriptors, plus the parents of directories waiting in the queue.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for bare/unsupported layouts or names, observed concurrent changes,
    /// missing/corrupt/wrong-kind HEAD commits or baseline trees, malformed index/HEAD, I/O,
    /// cancellation or exceeded limits. Index blobs, gitlink commits and baseline leaf targets are
    /// not resolved. When multiple threads fail, one error is returned. No partial report is
    /// returned.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::Repository;
    /// use girt::pathspec::Pathspec;
    /// use girt::status::{Baseline, Limits, Untracked};
    ///
    /// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
    /// let directory = tempfile::tempdir()?;
    /// let repository = Repository::init(
    ///     girt::ObjectFormat::Sha1,
    ///     directory.path().join("repo"),
    ///     girt::InitKind::Worktree,
    /// )?;
    ///
    /// let report = repository.raw_status_with(
    ///     Baseline::Head,
    ///     Untracked::Omit,
    ///     &Pathspec::all(),
    ///     Limits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    ///
    /// println!(
    ///     "{} staged, {} raw unstaged",
    ///     report.staged.len(),
    ///     report.unstaged.len()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn raw_status_with(
        &self,
        baseline: Baseline,
        untracked: Untracked,
        paths: &Pathspec,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Error> {
        super::types::check(cancel)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return supported::run(
            self,
            baseline,
            super::worktree::Listing::Raw(untracked),
            paths,
            limits,
            cancel,
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (baseline, untracked, paths, limits);
            Err(Error::Unsupported {
                path: vec![],
                reason: "descriptor-relative worktree traversal requires macOS/Linux",
            })
        }
    }

    /// Reports raw status with [`Limits::default`] and no cancellation.
    ///
    /// Equivalent to `self.raw_status_with(baseline, untracked, &Pathspec::all(),
    /// Limits::default(), &never)` with a flag that is never set; see [`Self::raw_status_with`] for
    /// the raw-content policy, platform support and concurrency checks. `baseline` and
    /// `untracked` remain the caller's choice.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::raw_status_with`] except cancellation.
    pub fn raw_status(&self, baseline: Baseline, untracked: Untracked) -> Result<Report, Error> {
        self.raw_status_with(
            baseline,
            untracked,
            &Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false),
        )
    }

    /// Reports configured index and working-tree status, listing untracked paths through `ignore`.
    ///
    /// Intent-to-add entries have no staged content; their working files are unstaged additions.
    /// Assume-unchanged and skip-worktree entries retain staged differences but suppress working
    /// differences. `core.fileMode=false` ignores executable-bit differences on regular files.
    /// With `core.symlinks=false`, a regular file containing a symlink target represents the
    /// indexed symlink; native symlinks still compare by their target bytes.
    /// `core.checkStat=minimal` trusts a file whose size and whole-second mtime (and ctime) match
    /// its cached stat words, ignoring nanoseconds, inode, device, uid and gid, and
    /// `core.trustCtime=false` never compares the ctime, as in Git. Other `core.checkStat`
    /// values are refused with [`Error::Unsupported`].
    /// Content bytes remain literal; use [`Self::raw_status_with`] to inspect modes and content
    /// without these policies. Configuration comes from this repository handle, including layers
    /// supplied to [`Self::open_with_config`].
    /// [`Report::untracked`] lists the untracked paths Git would show, chosen by `untracked`, and
    /// [`Report::raw_untracked`] stays empty.
    ///
    /// Rules come from [`WorktreeRules::base`] (the global excludes file and `info/exclude`) plus
    /// the `.gitignore` of every directory traversal enters, read through the same no-follow
    /// directory descriptors as tracked content. `.gitignore` files that `ignore` loaded for
    /// [`WorktreeRules::check`] are not reused, so an edited file is always reread. A deeper
    /// `.gitignore` overrides a shallower one, and `.gitignore` overrides `info/exclude`, which
    /// overrides the global file; the last matching line within a file wins.
    ///
    /// Under a pathspec, an untracked directory is listed as one
    /// [`UntrackedKind::Directory`](super::UntrackedKind::Directory) entry only when the
    /// pathspec matches the directory itself (its path with a trailing `/`), as Git does:
    /// `new`, `new/` and `n*` list `new`, while `*.txt` lists the matching files inside it.
    ///
    /// Names follow Git's `core.precomposeUnicode` and `core.ignoreCase`. An index path missing
    /// from its directory listing is looked up by its own name, as Git does, so a volume that
    /// merges names still finds the file: `README` renamed to `readme` on a case-insensitive
    /// volume is clean, and of two index paths the volume stores as one file, the shadowed one
    /// is reported modified when its content differs. With `core.precomposeUnicode`, listed
    /// names are composed (NFC) before comparison and reporting on macOS, the only platform where
    /// Git reads that option. With `core.ignoreCase`, an
    /// untracked name is hidden when an index path in its directory matches it ignoring ASCII
    /// case, even on a volume that stores the two as different files, as Git hides it. Without it
    /// the name is listed, as Git lists `readme` beside a clean `README` on a case-insensitive
    /// volume.
    ///
    /// A tracked path is never ignored. An ignored directory is not entered unless it holds tracked
    /// paths, and then only to inspect them: its untracked contents stay ignored, as in Git.
    /// Ignored directories therefore cost nothing beyond one directory entry. A `.gitignore`
    /// symlink is skipped rather than followed, as Git skips it. `.gitignore` bytes count
    /// toward [`Limits::max_file_bytes`] and [`Limits::max_worktree_bytes`]; rule counts and
    /// matching work are bounded by the limits `ignore` was loaded with.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::raw_status_with`], plus [`Error::Ignore`] when a `.gitignore`
    /// or a matched path exceeds the ignore limits.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::Repository;
    /// use girt::config::ConfigInputs;
    /// use girt::pathspec::Pathspec;
    /// use girt::status::{Baseline, Limits, UntrackedFiles, UntrackedKind};
    /// use girt::workflow::Environment;
    ///
    /// # if cfg!(not(any(target_os = "linux", target_os = "macos"))) { return Ok(()); }
    /// let directory = tempfile::tempdir()?;
    /// Repository::init(
    ///     girt::ObjectFormat::Sha1,
    ///     directory.path().join("w"),
    ///     girt::InitKind::Worktree,
    /// )?;
    ///
    /// // Read configuration as `git` does, so `core.excludesFile` is honoured.
    /// let environment = Environment::default().with("GIT_CONFIG_NOSYSTEM", "1");
    /// let repo = Repository::open_with_config(
    ///     directory.path().join("w"),
    ///     &ConfigInputs::git_default(&environment)?,
    /// )?;
    ///
    /// let worktree = repo.worktree().unwrap();
    /// std::fs::create_dir(worktree.join("target"))?;
    /// std::fs::write(worktree.join("target/app"), b"binary")?;
    /// std::fs::write(worktree.join(".gitignore"), b"/target/\n")?;
    /// std::fs::write(worktree.join("main.rs"), b"fn main() {}")?;
    ///
    /// let rules = repo.ignore_rules(&environment)?;
    /// let report = repo.status_with(
    ///     Baseline::Head,
    ///     UntrackedFiles::All,
    ///     &rules,
    ///     &Pathspec::all(),
    ///     Limits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    ///
    /// let untracked: Vec<_> = report
    ///     .untracked
    ///     .iter()
    ///     .map(|entry| &entry.path[..])
    ///     .collect();
    /// assert_eq!(untracked, [&b".gitignore"[..], b"main.rs"]);
    /// assert!(
    ///     report
    ///         .untracked
    ///         .iter()
    ///         .all(|entry| entry.kind == UntrackedKind::File)
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn status_with(
        &self,
        baseline: Baseline,
        untracked: UntrackedFiles,
        ignore: &WorktreeRules,
        paths: &Pathspec,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Error> {
        super::types::check(cancel)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return supported::run(
            self,
            baseline,
            super::worktree::Listing::Ignoring {
                rules: Box::new(ignore.base().clone()),
                files: untracked,
                names: supported::git_names(self)?,
                environment: Box::new(ignore.environment().clone()),
                monitor: None,
                capture: None,
                excludes: Some(ignore.untracked_excludes(self.object_format())),
            },
            paths,
            limits,
            cancel,
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (baseline, untracked, ignore, paths, limits);
            Err(Error::Unsupported {
                path: vec![],
                reason: "descriptor-relative worktree traversal requires macOS/Linux",
            })
        }
    }

    /// Reports ignore-aware status with working files converted as Git converts them.
    ///
    /// Behaves as [`Self::status_with`], except that each tracked regular file that is read is
    /// first converted by `filters` (from [`Self::filters`]) as [`Pipeline::to_repository`] would
    /// store it, and the result is hashed. Native symlinks bypass conversion; regular-file
    /// placeholders under `core.symlinks=false` pass through clean/EOL conversion as Git reads
    /// them. Index flags and mode policy still apply.
    /// `core.safecrlf` is not applied, as `git status` does
    /// not apply it. A file whose stat words match its index entry is still trusted without
    /// reading. As in Git, a file whose size differs from the nonzero size cached in the index is
    /// reported as [`Change::Modified`](super::Change::Modified) even when its converted content
    /// matches, for example after only its line endings changed; the reported identity is then
    /// the index's own. [`Report::comparison`] is
    /// [`Comparison::ConvertedContent`](super::Comparison::ConvertedContent).
    ///
    /// Files are read in parallel and converted one at a time. Filter commands run only if
    /// `filters` allows them, and a long-running filter process stays running in `filters` after
    /// the call.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::status_with`], plus [`Error::Filter`] when a conversion
    /// fails: a required driver failed, a command would run under
    /// [`Commands::Refuse`](crate::filter::Commands::Refuse), or attributes could not be read.
    ///
    /// [`Pipeline::to_repository`]: crate::filter::Pipeline::to_repository
    #[expect(
        clippy::too_many_arguments,
        reason = "status_with's inputs plus the conversion pipeline"
    )]
    pub fn status_with_filters(
        &self,
        baseline: Baseline,
        untracked: UntrackedFiles,
        ignore: &WorktreeRules,
        paths: &Pathspec,
        filters: &mut crate::filter::Pipeline,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Error> {
        super::types::check(cancel)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return supported::run_with_checkpoint(
            self,
            baseline,
            super::worktree::Listing::Ignoring {
                rules: Box::new(ignore.base().clone()),
                files: untracked,
                names: supported::git_names(self)?,
                environment: Box::new(ignore.environment().clone()),
                monitor: None,
                capture: None,
                excludes: Some(ignore.untracked_excludes(self.object_format())),
            },
            paths,
            Some(filters),
            limits,
            cancel,
            || {},
        );
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (baseline, untracked, ignore, paths, filters, limits);
            Err(Error::Unsupported {
                path: vec![],
                reason: "descriptor-relative worktree traversal requires macOS/Linux",
            })
        }
    }

    /// Reports ignore-aware status with [`Limits::default`] and no cancellation.
    ///
    /// Equivalent to `self.status_with(baseline, untracked, ignore, &Pathspec::all(),
    /// Limits::default(), &never)`
    /// with a flag that is never set; see [`Self::status_with`] for how ignore rules apply.
    /// Load `ignore` with [`Self::ignore_rules`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::status_with`] except cancellation.
    pub fn status(
        &self,
        baseline: Baseline,
        untracked: UntrackedFiles,
        ignore: &WorktreeRules,
    ) -> Result<Report, Error> {
        self.status_with(
            baseline,
            untracked,
            ignore,
            &Pathspec::all(),
            Limits::default(),
            &AtomicBool::new(false),
        )
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod supported {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::status::types::{charge, check, value};
    use crate::status::worktree::Listing;
    use crate::{Commit, ObjectId, ObjectKind, Objects, TreeChange, TreeValue, index, refs};

    /// Git's `core.ignoreCase` and `core.precomposeUnicode` for name comparison; unset is false.
    pub(in crate::status) fn git_names(
        repo: &Repository,
    ) -> Result<super::super::worktree::GitNames, Error> {
        let config = repo.config();
        let setting = |name: &str, reason: &'static str| {
            config
                .boolean("core", None, name)
                .map(|value| value == Some(true))
                .map_err(|_| Error::Unsupported {
                    path: vec![],
                    reason,
                })
        };
        Ok(super::super::worktree::GitNames {
            ignore_case: setting("ignorecase", "core.ignoreCase is not a boolean")?,
            // Git reads this option only on macOS.
            precompose: cfg!(target_os = "macos")
                && setting(
                    "precomposeunicode",
                    "core.precomposeUnicode is not a boolean",
                )?,
        })
    }

    pub(super) fn run(
        repo: &Repository,
        baseline: Baseline,
        listing: Listing,
        paths: &Pathspec,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Error> {
        run_with_checkpoint(repo, baseline, listing, paths, None, limits, cancel, || {})
    }

    #[expect(clippy::too_many_arguments, reason = "one status run's inputs")]
    pub(in crate::status) fn run_with_checkpoint(
        repo: &Repository,
        baseline: Baseline,
        listing: Listing,
        paths: &Pathspec,
        filters: Option<&mut crate::filter::Pipeline>,
        limits: Limits,
        cancel: &AtomicBool,
        before_recheck: impl FnOnce(),
    ) -> Result<Report, Error> {
        let configured = matches!(listing, Listing::Ignoring { .. });
        let environment = filters
            .as_ref()
            .and_then(|filters| filters.environment())
            .cloned()
            .unwrap_or_else(|| match &listing {
                Listing::Ignoring { environment, .. } => (**environment).clone(),
                Listing::Raw(_) => crate::workflow::Environment::default(),
            });
        let (commands, driver_limits) = filters.as_ref().map_or(
            (
                crate::filter::Commands::Refuse,
                crate::filter::Limits::default(),
            ),
            |filters| filters.driver_policy(),
        );
        run_finishing(
            repo,
            baseline,
            listing,
            paths,
            filters,
            limits,
            cancel,
            |report| {
                if configured {
                    crate::submodule::complete_status(
                        repo,
                        report,
                        commands,
                        driver_limits,
                        &environment,
                        limits,
                        cancel,
                    )
                    .map_err(|error| Error::Submodules(Box::new(error)))?;
                }
                Ok(())
            },
            before_recheck,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one status run plus completion before recheck"
    )]
    pub(super) fn run_finishing(
        repo: &Repository,
        baseline: Baseline,
        listing: Listing,
        paths: &Pathspec,
        filters: Option<&mut crate::filter::Pipeline>,
        mut limits: Limits,
        cancel: &AtomicBool,
        finish: impl FnOnce(&mut Report) -> Result<(), Error>,
        before_recheck: impl FnOnce(),
    ) -> Result<Report, Error> {
        if paths.uses_attributes() {
            return Err(Error::Unsupported {
                path: vec![],
                reason: "attr: pathspec magic in status",
            });
        }
        let root = repo.worktree().ok_or_else(|| Error::Unsupported {
            path: vec![],
            reason: if repo.is_bare() {
                "bare repository has no working tree"
            } else {
                "worktree location is unknown"
            },
        })?;
        // The racy cutoff is the modification time of the very file whose entries are trusted.
        let (index, index_written) = repo.read_index_timed(limits.index)?;
        // Index words hold the low 32 bits of the seconds, as Git writes them.
        let index_written = index_written.map(|seconds| seconds as u32);
        let split = index.as_ref().is_some_and(|index| {
            index
                .extensions()
                .iter()
                .any(|extension| extension.signature() == *b"link")
        });
        let index_written = index_written.filter(|_| !split);
        check(cancel)?;
        let entries = index.as_ref().map_or(&[][..], index::Index::entries);
        for entry in entries {
            check(cancel)?;
            if matches!(listing, Listing::Raw(_)) && (entry.intent_to_add || entry.skip_worktree) {
                return Err(Error::Unsupported {
                    path: entry.path.clone(),
                    reason: "extended index flags require caller policy",
                });
            }
            super::super::worktree::validate_path(&entry.path)?;
            charge(&mut limits.max_path_bytes, entry.path.len(), "path bytes")?;
        }
        let objects = repo.objects_with(limits.packs).map_err(Error::Objects)?;
        check(cancel)?;
        let head = match baseline {
            Baseline::Head => Some(resolve_head(repo)?),
            Baseline::Tree(_) => None,
        };
        let tree = match baseline {
            Baseline::Tree(tree) => tree,
            Baseline::Head => head
                .as_ref()
                .unwrap()
                .id
                .map(|id| {
                    let object =
                        read_object(&objects, id, b"HEAD", ObjectKind::Commit, &mut limits)?;
                    Ok::<_, Error>(Commit::parse(repo.object_format(), object.data())?.tree())
                })
                .transpose()?,
        };
        let mut unmerged = Vec::new();
        let mut gitlinks = Vec::new();
        for entry in entries {
            check(cancel)?;
            if !paths.matches_unchecked(&entry.path) {
                continue;
            }
            if entry.stage != index::Stage::Normal {
                unmerged.push(entry.clone());
            } else if entry.mode == index::Mode::Gitlink {
                gitlinks.push(entry.clone());
            }
        }
        // Without conflicts the index has trees of its own, so the comparison skips every
        // baseline subtree whose identity equals the index's and reads only the rest.
        let conflicted = entries
            .iter()
            .any(|entry| entry.stage != index::Stage::Normal);
        let trees = index
            .as_ref()
            .filter(|_| !conflicted && !entries.iter().any(|entry| entry.intent_to_add))
            .and_then(|index| index.trees(cancel));
        check(cancel)?;
        let staged = match trees {
            Some((root, trees)) => objects.compare_trees_among(
                tree,
                Some(root),
                &trees,
                paths,
                limits.trees,
                cancel,
            )?,
            None => flattened_staged(&objects, tree, entries, limits, cancel)?,
        };
        let mut staged = staged;
        staged.retain(|change| paths.matches_unchecked(&change.path));
        let (untracked_policy, untracked_files) = match &listing {
            Listing::Raw(policy) => (*policy, None),
            Listing::Ignoring { files, .. } => (Untracked::Omit, Some(*files)),
        };
        let configured = matches!(listing, Listing::Ignoring { .. });
        let comparison = if filters.is_some() {
            super::super::Comparison::ConvertedContent
        } else if configured {
            super::super::Comparison::Configured
        } else {
            super::super::Comparison::RawBytesAndPosixModes
        };
        let shared = filters.map(|pipeline| super::super::worktree::Shared {
            pipeline: std::sync::Mutex::new(pipeline),
            objects: &objects,
        });
        let monitor_clean = match &listing {
            Listing::Ignoring {
                monitor: Some(monitor),
                ..
            } => super::super::scan::monitor_clean(
                repo,
                index.as_ref(),
                monitor,
                limits.index,
                cancel,
            )?,
            _ => Vec::new(),
        };
        let (untracked_cache, mut cache_use) =
            super::super::untracked::load(repo, index.as_ref(), &listing, paths, limits, cancel);
        let observed = super::super::worktree::scan(
            repo,
            root,
            entries,
            index_written,
            &monitor_clean,
            untracked_cache.as_ref(),
            listing,
            paths,
            shared.as_ref().map(|shared| shared as _),
            limits,
            cancel,
        )?;
        if let super::super::UntrackedCacheUse::Reused { directories } = &mut cache_use {
            *directories = observed.cache_reused;
        }
        let mut report = Report {
            untracked_cache: cache_use,
            comparison,
            baseline_tree: tree,
            index_missing: index.is_none(),
            staged,
            unstaged: observed
                .changes
                .into_iter()
                .filter(|change| paths.matches_unchecked(&change.path))
                .collect(),
            unmerged,
            unchecked_gitlinks: gitlinks,
            submodules: Vec::new(),
            untracked_policy,
            raw_untracked: observed.untracked,
            untracked_files,
            untracked: observed.listed,
            boundaries: observed.boundaries,
        };
        finish(&mut report)?;
        before_recheck();
        check(cancel)?;
        if !repo.index_unchanged(index.as_ref(), limits.index)? {
            return Err(Error::Changed(b"index".to_vec()));
        }
        if let Some(head) = head
            && resolve_head(repo)? != head
        {
            return Err(Error::Changed(b"HEAD".to_vec()));
        }
        check(cancel)?;
        Ok(report)
    }

    /// Compares every baseline leaf with the stage-zero entries, leaving conflicted paths to
    /// [`Report::unmerged`].
    fn flattened_staged(
        objects: &Objects,
        tree: Option<ObjectId>,
        entries: &[index::Entry],
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Vec<TreeChange>, Error> {
        let leaves = objects.compare_trees_with(None, tree, limits.trees, cancel)?;
        let mut staged: BTreeMap<Vec<u8>, (Option<TreeValue>, Option<TreeValue>)> = leaves
            .into_iter()
            .map(|change| (change.path, (change.new, None)))
            .collect();
        let mut conflicts = BTreeSet::new();
        for entry in entries {
            check(cancel)?;
            if entry.stage == index::Stage::Normal {
                if !entry.intent_to_add {
                    staged.entry(entry.path.clone()).or_default().1 = Some(value(entry));
                }
            } else {
                conflicts.insert(entry.path.clone());
            }
        }
        Ok(staged
            .into_iter()
            .filter(|(path, (old, new))| old != new && !conflicts.contains(path))
            .map(|(path, (old, new))| TreeChange { path, old, new })
            .collect())
    }

    fn resolve_head(repo: &Repository) -> Result<refs::Resolution, Error> {
        let refs = repo.references()?;
        let name = refs::RefName::HEAD;
        if refs.read(&name)?.is_none() {
            return Err(Error::Unsupported {
                path: b"HEAD".to_vec(),
                reason: "missing HEAD",
            });
        }
        Ok(refs.resolve_with(&name, 32)?)
    }

    fn read_object(
        objects: &Objects,
        id: ObjectId,
        path: &[u8],
        kind: ObjectKind,
        limits: &mut Limits,
    ) -> Result<crate::Object, Error> {
        let mut read = limits.read;
        read.max_object_bytes = read.max_object_bytes.min(limits.max_object_bytes);
        let object = objects
            .read_with(id, read)
            .map_err(|source| Error::Object {
                id,
                path: path.to_vec(),
                source: Box::new(source),
            })?
            .ok_or_else(|| Error::InvalidObject {
                id,
                path: path.to_vec(),
                reason: "missing object",
            })?;
        if object.kind() != kind {
            return Err(Error::InvalidObject {
                id,
                path: path.to_vec(),
                reason: "wrong object kind",
            });
        }
        charge(
            &mut limits.max_object_bytes,
            object.data().len(),
            "object bytes",
        )?;
        Ok(object)
    }
    #[cfg(test)]
    mod tests {
        use std::fs;

        use rstest::rstest;

        use super::*;

        #[rstest]
        #[case::index("index", b"index")]
        #[case::head("HEAD", b"HEAD")]
        fn detects_metadata_change_on_final_reread(#[case] file: &str, #[case] expected: &[u8]) {
            let temp = tempfile::tempdir().unwrap();
            let repo = Repository::init(
                crate::ObjectFormat::Sha1,
                temp.path().join("repo"),
                crate::InitKind::Worktree,
            )
            .unwrap();
            let bytes = replacement(file);
            let result = run_with_checkpoint(
                &repo,
                Baseline::Head,
                Listing::Raw(Untracked::Omit),
                &Pathspec::all(),
                None,
                Limits::default(),
                &AtomicBool::new(false),
                || {
                    fs::write(repo.git_dir().join(file), bytes).unwrap();
                },
            );
            assert!(matches!(result, Err(Error::Changed(path)) if path == expected));
        }

        fn replacement(file: &str) -> Vec<u8> {
            match file {
                "HEAD" => b"ref: refs/heads/different\n".to_vec(),
                _ => index::Index::empty(crate::ObjectFormat::Sha1)
                    .encode(Default::default())
                    .unwrap(),
            }
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn nested_status(
    repo: &Repository,
    untracked: UntrackedFiles,
    environment: &crate::workflow::Environment,
    filters: &mut crate::filter::Pipeline,
    limits: Limits,
    cancel: &AtomicBool,
    finish: impl FnOnce(&mut Report) -> Result<(), crate::submodule::StatusError>,
) -> Result<Report, Error> {
    let rules = repo.ignore_rules(environment).map_err(|error| {
        Error::Submodules(Box::new(crate::submodule::StatusError::AtPath {
            path: vec![],
            source: error.into(),
        }))
    })?;
    supported::run_finishing(
        repo,
        Baseline::Head,
        super::worktree::Listing::Ignoring {
            rules: Box::new(rules.base().clone()),
            files: untracked,
            names: supported::git_names(repo)?,
            environment: Box::new(environment.clone()),
            monitor: None,
            capture: None,
            excludes: Some(rules.untracked_excludes(repo.object_format())),
        },
        &Pathspec::all(),
        Some(filters),
        limits,
        cancel,
        |report| finish(report).map_err(|error| Error::Submodules(Box::new(error))),
        || {},
    )
}
