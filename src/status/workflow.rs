use std::sync::atomic::AtomicBool;

use super::{Baseline, Error, Limits, Report, Untracked};
use crate::Repository;

impl Repository {
    /// Compares a baseline tree with the index, then verifies literal working-tree content.
    ///
    /// This is a **raw-content** API: attributes, filters, EOL conversion, ignores and config
    /// overrides of executable/symlink behavior are not applied. Every result records this policy.
    /// Use [`Untracked::Omit`] for tracked-only inspection, or explicitly request raw untracked
    /// leaves. Assume-valid and stat equality never skip verification. No filesystem writes or
    /// index refresh occur; ordinary reads may update access times according to the filesystem.
    ///
    /// macOS/Linux traversal opens components relative to directory descriptors with no-follow
    /// flags. It never follows symlink ancestors, enters `.git` components or descends into
    /// gitlinks/nested repositories. Exact directory names must match index bytes, including case.
    /// macOS non-ASCII names are unsupported; Linux retains non-UTF-8 bytes. Both reject NUL,
    /// backslashes, colons, empty/dot/parent components and case-insensitive `.git` components.
    /// Windows and other
    /// platforms return unsupported before accessing storage. Worktree files must live on a
    /// filesystem supporting native POSIX modes and symlinks; emulation/normalization is excluded.
    ///
    /// File identity, size, mode, mtime and ctime are compared around reads. Directory metadata is
    /// checked around traversal; HEAD (when requested) and index are reread before returning.
    /// Observed changes fail. These checks cannot detect all changes, ABA replacements, or changes
    /// after inspection: the report is not an atomic snapshot and must never authorize checkout
    /// overwrites. Open directory descriptors pin inspected directories even if concurrently moved.
    /// Mount changes, hostile hardlinks and repository-metadata replacement are outside the trust
    /// boundary. Metadata/object/reference reads inherit the existing storage APIs' assumptions;
    /// in particular reference resolution has a depth bound but no byte budget.
    ///
    /// Work is synchronous. Cancellation is checked between entries, objects, directories and
    /// 64-KiB file-read chunks. One syscall, object read, index parse, tree parse or sort cannot be
    /// interrupted. The caller owns blocking-worker scheduling; there is no wall-clock guarantee.
    /// Memory is bounded by index/tree/pack limits plus visited path records and one file buffer.
    /// Directory traversal retains at most `min(max_depth, 128) + 1` active directory frames.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for bare/unsupported layouts or names, observed concurrent changes,
    /// missing/corrupt/wrong-kind objects, malformed index/HEAD, I/O, cancellation or exceeded
    /// limits. Index blob targets are verified even for missing worktree files and conflicts;
    /// gitlink commits and baseline leaf targets are not resolved. No partial report is returned.
    ///
    /// ```no_run
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::Repository;
    /// use girt::status::{Baseline, Limits, Untracked};
    /// let repository = Repository::open("/path/to/repository")?;
    /// let report = repository.raw_status(
    ///     Baseline::Head,
    ///     Untracked::Omit,
    ///     Limits::default(),
    ///     &AtomicBool::new(false),
    /// )?;
    /// println!(
    ///     "{} staged, {} raw unstaged",
    ///     report.staged.len(),
    ///     report.unstaged.len()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn raw_status(
        &self,
        baseline: Baseline,
        untracked: Untracked,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Error> {
        super::types::check(cancel)?;
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        return supported::run(self, baseline, untracked, limits, cancel);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (baseline, untracked, limits);
            Err(Error::Unsupported {
                path: vec![],
                reason: "descriptor-relative worktree traversal requires macOS/Linux",
            })
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod supported {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;
    use crate::status::types::{charge, check, value};
    use crate::{Commit, ObjectId, ObjectKind, Objects, TreeChange, TreeValue, index, refs};

    pub(super) fn run(
        repo: &Repository,
        baseline: Baseline,
        untracked: Untracked,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Report, Error> {
        run_with_checkpoint(repo, baseline, untracked, limits, cancel, || {})
    }

    fn run_with_checkpoint(
        repo: &Repository,
        baseline: Baseline,
        untracked: Untracked,
        mut limits: Limits,
        cancel: &AtomicBool,
        before_recheck: impl FnOnce(),
    ) -> Result<Report, Error> {
        let root = repo.worktree().ok_or_else(|| Error::Unsupported {
            path: vec![],
            reason: "bare repository has no working tree",
        })?;
        let index = repo.read_index(limits.index)?;
        check(cancel)?;
        let entries = index.as_ref().map_or(&[][..], index::Index::entries);
        for entry in entries {
            check(cancel)?;
            super::super::worktree::validate_path(&entry.path)?;
            charge(&mut limits.max_path_bytes, entry.path.len(), "path bytes")?;
        }
        let objects = repo.objects(limits.packs).map_err(Error::Objects)?;
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
        let leaves = objects.compare_trees(None, tree, limits.trees, cancel)?;
        let mut staged: BTreeMap<Vec<u8>, (Option<TreeValue>, Option<TreeValue>)> = leaves
            .into_iter()
            .map(|change| (change.path, (change.new, None)))
            .collect();
        let mut conflicts = BTreeSet::new();
        let mut unmerged = Vec::new();
        let mut gitlinks = Vec::new();
        for entry in entries {
            check(cancel)?;
            if entry.mode != index::Mode::Gitlink {
                read_object(
                    &objects,
                    entry.id,
                    &entry.path,
                    ObjectKind::Blob,
                    &mut limits,
                )?;
            }
            if entry.stage != index::Stage::Normal {
                conflicts.insert(entry.path.clone());
                unmerged.push(entry.clone());
            } else {
                staged.entry(entry.path.clone()).or_default().1 = Some(value(entry));
                if entry.mode == index::Mode::Gitlink {
                    gitlinks.push(entry.clone());
                }
            }
        }
        let staged = staged
            .into_iter()
            .filter(|(path, (old, new))| old != new && !conflicts.contains(path))
            .map(|(path, (old, new))| TreeChange { path, old, new })
            .collect();
        let observed =
            super::super::worktree::scan(repo, root, entries, untracked, limits, cancel)?;
        before_recheck();
        check(cancel)?;
        if repo.read_index(limits.index)? != index {
            return Err(Error::Changed(b"index".to_vec()));
        }
        if let Some(head) = head
            && resolve_head(repo)? != head
        {
            return Err(Error::Changed(b"HEAD".to_vec()));
        }
        check(cancel)?;
        Ok(Report {
            comparison: super::super::Comparison::RawBytesAndPosixModes,
            baseline_tree: tree,
            index_missing: index.is_none(),
            staged,
            unstaged: observed.changes,
            unmerged,
            unchecked_gitlinks: gitlinks,
            untracked_policy: untracked,
            raw_untracked: observed.untracked,
            boundaries: observed.boundaries,
        })
    }

    fn resolve_head(repo: &Repository) -> Result<refs::Resolution, Error> {
        let refs = repo.references()?;
        let name = refs::RefName::new(b"HEAD").expect("HEAD is a valid name");
        if refs.read(&name)?.is_none() {
            return Err(Error::Unsupported {
                path: b"HEAD".to_vec(),
                reason: "missing HEAD",
            });
        }
        Ok(refs.resolve(&name, 32)?)
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
            .read(id, read)
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
                Untracked::Omit,
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
