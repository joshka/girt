/// Whether composed maintenance consolidates the repository's packs into one.
///
/// Only [`Repository::collect_garbage`](crate::Repository::collect_garbage),
/// [`Repository::collect_garbage_with`](crate::Repository::collect_garbage_with) and
/// [`Repository::run_maintenance`](crate::Repository::run_maintenance) read this choice. Fetch,
/// commit, status and the other routine operations never consolidate, so a caller decides when
/// to pay for rewriting its packs.
///
/// Consolidation publishes one pack holding every retained object, then retires the packs it
/// replaces under the same publish, verify and retire rules as
/// [`Repository::retire_old_packs`](crate::Repository::retire_old_packs). Unlike that operation,
/// it also retires packs newer than [`RetentionPolicy::recent_cutoff`](super::RetentionPolicy),
/// keeping every object they hold. `.keep` packs, promisor packs and packs with other sidecars,
/// such as Git's cruft-pack `.mtimes`, stay in place and are not copied.
///
/// ```
/// use girt::retention::{PackConsolidation, RetentionPolicy};
///
/// // Follow `gc.autoPackLimit`, as Git's automatic gc does.
/// assert_eq!(
///     RetentionPolicy::default().pack_consolidation,
///     PackConsolidation::Configured
/// );
/// // Never rewrite packs during this maintenance call.
/// let policy = RetentionPolicy::default().with_pack_consolidation(PackConsolidation::Never);
/// assert_eq!(policy.pack_consolidation, PackConsolidation::Never);
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum PackConsolidation {
    /// Consolidate when the number of indexed primary packs without a `.keep` file is more than
    /// `gc.autoPackLimit`, as Git counts for `git gc --auto`. Promisor packs count, as in Git.
    /// The limit defaults to Git's 50; zero or a negative value turns consolidation off.
    /// `gc.auto` is not consulted, because girt's maintenance is always an explicit call.
    #[default]
    Configured,
    /// Consolidate whatever the pack count.
    Always,
    /// Never consolidate; packs are rewritten only by phases that already do so.
    Never,
}

// Resolving the choice is needed only where composed maintenance runs.
#[cfg(unix)]
pub(super) use select::SelectError;

#[cfg(unix)]
mod select {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::{fs, io};

    use super::super::RetentionError;
    use super::PackConsolidation;
    use crate::Repository;
    use crate::config::InvalidValue;

    /// Git's default `gc.autoPackLimit`.
    const DEFAULT_PACK_LIMIT: i64 = 50;

    /// Why the consolidation choice could not be resolved.
    #[derive(Debug)]
    pub(in crate::retention) enum SelectError {
        Config(InvalidValue),
        Observe(RetentionError),
        Cancelled,
    }

    impl PackConsolidation {
        /// Resolves the choice against the repository's configuration and current pack count.
        pub(in crate::retention) fn selected(
            self,
            repository: &Repository,
            max_entries: usize,
            cancel: &AtomicBool,
        ) -> Result<bool, SelectError> {
            let limit = match self {
                Self::Always => return Ok(true),
                Self::Never => return Ok(false),
                Self::Configured => configured_limit(repository).map_err(SelectError::Config)?,
            };
            let Some(limit) = limit else {
                return Ok(false);
            };
            let count = counted_packs(repository, max_entries, cancel)?;
            Ok(count > limit)
        }
    }

    // Git reads the key as a 32-bit integer and refuses values it cannot represent.
    fn configured_limit(repository: &Repository) -> Result<Option<usize>, InvalidValue> {
        let config = repository.config();
        let limit = config
            .integer("gc", None, "autoPackLimit")?
            .unwrap_or(DEFAULT_PACK_LIMIT);
        if i32::try_from(limit).is_err() {
            return Err(InvalidValue::new(
                "gc",
                "autoPackLimit",
                config.value("gc", None, "autoPackLimit").flatten(),
            ));
        }
        Ok(usize::try_from(limit).ok().filter(|&limit| limit > 0))
    }

    // Counts indexed packs in the primary pack directory that have no `.keep` file.
    fn counted_packs(
        repository: &Repository,
        max_entries: usize,
        cancel: &AtomicBool,
    ) -> Result<usize, SelectError> {
        let directory = repository.object_dir().join("pack");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(SelectError::Observe(RetentionError::read(error))),
        };
        let mut names = std::collections::BTreeSet::<PathBuf>::new();
        for entry in entries {
            if cancel.load(Ordering::Relaxed) {
                return Err(SelectError::Cancelled);
            }
            if names.len() >= max_entries {
                return Err(SelectError::Observe(
                    RetentionError::PackDirectoryEntryLimit,
                ));
            }
            let entry = entry.map_err(|error| SelectError::Observe(RetentionError::read(error)))?;
            names.insert(entry.path());
        }
        Ok(names
            .iter()
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "idx")
                    && names.contains(&path.with_extension("pack"))
                    && !names.contains(&path.with_extension("keep"))
            })
            .count())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn fixture(config: &str, packs: &[&[&str]]) -> (tempfile::TempDir, Repository) {
            let root = tempfile::tempdir().unwrap();
            let repository = Repository::init(
                crate::ObjectFormat::Sha1,
                root.path().join("r"),
                crate::InitKind::Bare,
            )
            .unwrap();
            let path = repository.git_dir().join("config");
            let mut bytes = fs::read(&path).unwrap();
            bytes.extend_from_slice(config.as_bytes());
            fs::write(&path, bytes).unwrap();
            let directory = repository.object_dir().join("pack");
            fs::create_dir_all(&directory).unwrap();
            for (number, extensions) in packs.iter().enumerate() {
                for extension in *extensions {
                    fs::write(
                        directory.join(format!("pack-{number:040x}.{extension}")),
                        b"",
                    )
                    .unwrap();
                }
            }
            let repository = Repository::open(repository.git_dir()).unwrap();
            (root, repository)
        }

        const PAIR: &[&str] = &["idx", "pack"];

        // Mirrors `git gc --auto` with git 2.56: more than the limit, `.keep` uncounted, promisor
        // counted, unindexed packs invisible, zero or negative disabled, Git integer syntax.
        #[rstest::rstest]
        #[case::above_limit("[gc]\n\tautoPackLimit = 3\n", &[PAIR, PAIR, PAIR, PAIR], true)]
        #[case::at_limit("[gc]\n\tautoPackLimit = 4\n", &[PAIR, PAIR, PAIR, PAIR], false)]
        #[case::keep_uncounted(
            "[gc]\n\tautoPackLimit = 3\n",
            &[PAIR, PAIR, PAIR, &["idx", "pack", "keep"]],
            false
        )]
        #[case::promisor_counted(
            "[gc]\n\tautoPackLimit = 3\n",
            &[PAIR, PAIR, PAIR, &["idx", "pack", "promisor"]],
            true
        )]
        #[case::unindexed_uncounted("[gc]\n\tautoPackLimit = 3\n", &[PAIR, PAIR, PAIR, &["pack"]], false)]
        #[case::zero_disables("[gc]\n\tautoPackLimit = 0\n", &[PAIR, PAIR], false)]
        #[case::negative_disables("[gc]\n\tautoPackLimit = -1\n", &[PAIR, PAIR], false)]
        #[case::unit_suffix("[gc]\n\tautoPackLimit = 1k\n", &[PAIR, PAIR], false)]
        #[case::default_limit("", &[PAIR, PAIR], false)]
        fn configured_selection_matches_git(
            #[case] config: &str,
            #[case] packs: &[&[&str]],
            #[case] expected: bool,
        ) {
            let (_root, repository) = fixture(config, packs);
            let selected = PackConsolidation::Configured
                .selected(&repository, 1000, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(selected, expected);
        }

        // The default of 50 needs 51 counted packs.
        #[rstest::rstest]
        #[case::above(51, true)]
        #[case::at(50, false)]
        fn default_limit_is_fifty(#[case] count: usize, #[case] expected: bool) {
            let (_root, repository) = fixture("", &vec![PAIR; count]);
            let selected = PackConsolidation::Configured
                .selected(&repository, 1000, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(selected, expected);
        }

        // Git refuses these with "bad numeric config value".
        #[rstest::rstest]
        #[case::word("[gc]\n\tautoPackLimit = abc\n")]
        #[case::empty("[gc]\n\tautoPackLimit =\n")]
        #[case::implicit("[gc]\n\tautoPackLimit\n")]
        #[case::bad_unit("[gc]\n\tautoPackLimit = 3x\n")]
        #[case::above_32_bits("[gc]\n\tautoPackLimit = 2147483648\n")]
        #[case::below_32_bits("[gc]\n\tautoPackLimit = -2147483649\n")]
        fn malformed_limit_is_refused(#[case] config: &str) {
            let (_root, repository) = fixture(config, &[PAIR]);
            assert!(matches!(
                PackConsolidation::Configured.selected(&repository, 1000, &AtomicBool::new(false)),
                Err(SelectError::Config(_))
            ));
        }

        // The explicit choices never consult configuration, even a malformed value.
        #[rstest::rstest]
        #[case::always(PackConsolidation::Always, true)]
        #[case::never(PackConsolidation::Never, false)]
        fn explicit_choices_ignore_configuration(
            #[case] choice: PackConsolidation,
            #[case] expected: bool,
        ) {
            let (_root, repository) = fixture("[gc]\n\tautoPackLimit = abc\n", &[PAIR]);
            assert_eq!(
                choice
                    .selected(&repository, 1000, &AtomicBool::new(false))
                    .unwrap(),
                expected
            );
        }
    }
}
