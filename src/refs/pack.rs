use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use super::store::{Lock, io_error, parse_loose, read_optional};
use super::{Backend, LockWait, RefName, ReferenceError, References, Target, packed};
use crate::{ObjectId, Objects, PeelError, PeelFailure, PeelLimits};

/// Which loose references [`References::pack`] moves into `packed-refs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackScope {
    /// Only `refs/tags/`, like `git pack-refs` without `--all`.
    ///
    /// As with Git 2.56, a loose file for a name that is already packed elsewhere stays loose.
    Tags,
    /// Every shared direct reference, like `git pack-refs --all`.
    All,
}

/// When [`References::pack`] rewrites `packed-refs`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackTrigger {
    /// Always rewrite, even with no loose candidates.
    Always,
    /// Rewrite only when enough loose candidates exist for the current `packed-refs` size.
    ///
    /// At least 16 candidates are required. The threshold grows by 5 each time the
    /// `packed-refs` file doubles past 12 KiB. This approximates `git pack-refs --auto` as Git
    /// 2.56 behaves; it is not Git's exact heuristic.
    Auto,
}

/// Options for [`References::pack`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PackRefsOptions {
    /// Loose references to pack.
    pub scope: PackScope,
    /// Whether to pack unconditionally or only past the automatic threshold.
    pub trigger: PackTrigger,
    /// How long to wait for `packed-refs.lock`. Loose reference locks are never awaited.
    pub lock_wait: LockWait,
}

impl PackRefsOptions {
    /// Packs every shared direct reference now, like `git pack-refs --all`.
    pub const ALL: Self = Self {
        scope: PackScope::All,
        trigger: PackTrigger::Always,
        lock_wait: LockWait::Immediate,
    };

    /// Packs every shared direct reference once enough are loose, like the pack-refs task of
    /// `git maintenance run --auto`.
    pub const AUTO: Self = Self {
        scope: PackScope::All,
        trigger: PackTrigger::Auto,
        lock_wait: LockWait::Immediate,
    };
}

/// Effects of [`References::pack`].
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackRefsReport {
    /// Whether `packed-refs` was replaced. False when the automatic threshold was not reached.
    pub rewritten: bool,
    /// Loose files whose values were written to `packed-refs`.
    pub packed: usize,
    /// Loose files removed after packing.
    pub pruned: usize,
    /// Packed names whose loose file stays: it was locked or changed after packing, or the host
    /// is not Unix, where pruning is deferred.
    ///
    /// The loose file still takes precedence over its packed record, so every value is unchanged.
    pub retained: Vec<RefName>,
    /// Loose candidates left unpacked because their file is malformed or their object is missing.
    pub skipped: Vec<RefName>,
}

/// A pack failure; `packed-refs` was not replaced unless [`Self::Unconfirmed`] or [`Self::Prune`]
/// is returned.
#[derive(Debug, thiserror::Error)]
pub enum PackRefsError {
    /// Locking, reading or publishing failed before `packed-refs` was replaced.
    #[error("packing references")]
    Reference(#[from] ReferenceError),
    /// An object could not be read while recording peeled values; no files were changed.
    #[error("peeling packed reference {name:?}")]
    Peel {
        /// Reference whose object failed to peel.
        name: RefName,
        /// Underlying peel failure. Missing objects are skipped instead of reported here.
        #[source]
        source: Box<PeelError>,
    },
    /// `packed-refs` was replaced and is visible, but syncing its directory failed.
    ///
    /// No loose file was removed, so every reference keeps a loose copy until a later pack
    /// confirms durability.
    #[error("confirming packed-refs durability")]
    Unconfirmed {
        /// Effects so far: the replacement, with nothing pruned.
        report: Box<PackRefsReport>,
        /// Directory sync failure.
        #[source]
        source: ReferenceError,
    },
    /// `packed-refs` was replaced durably, but removing a loose file failed.
    ///
    /// Every reference keeps its value: an unremoved loose file shadows an equal packed record.
    #[error("pruning packed loose references")]
    Prune {
        /// Effects so far, including the replacement.
        report: Box<PackRefsReport>,
        /// Removal failure.
        #[source]
        source: ReferenceError,
    },
}

const HEADER: &[u8] = b"# pack-refs with: peeled fully-peeled sorted \n";

struct Candidate {
    path: PathBuf,
    id: ObjectId,
}

impl References<'_> {
    /// Moves loose direct references into `packed-refs` and removes the loose files.
    ///
    /// Holds `packed-refs.lock` throughout. The replacement is sorted, records the fully peeled
    /// target of every annotated tag, and is synced to disk with its directory before any loose
    /// file is removed. An existing `^` line is kept when its objects can no longer be read. Each
    /// loose file is then removed under its own lock, only if it still holds the packed value;
    /// a locked or changed file is kept and reported. On non-Unix hosts no loose file is removed,
    /// following the deferral of destructive maintenance there: no nonadmin sync makes the rename
    /// durable first. Every packed name is then reported as retained. Readers observe either the
    /// old state or loose files that shadow equal packed records.
    ///
    /// Symbolic references, `HEAD`, per-worktree references and refs that are not direct stay
    /// loose. Malformed loose files and references to missing objects are skipped, as Git skips
    /// them. Existing packed records are kept, including those whose objects are missing.
    ///
    /// # Errors
    ///
    /// Returns [`ReferenceError::Unsupported`] for reftable. Returns [`ReferenceError::Locked`]
    /// when `packed-refs.lock` remains held, and [`PackRefsError::Peel`] for a corrupt object.
    /// All of these have no effects. [`PackRefsError::Unconfirmed`] reports a visible
    /// replacement whose directory sync failed, with nothing pruned; [`PackRefsError::Prune`]
    /// reports a removal failure after a durable replacement.
    ///
    /// ```
    /// use girt::refs::PackRefsOptions;
    /// use girt::{InitKind, ObjectFormat, PackLimits, Repository};
    /// # let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r"),
    ///     InitKind::Bare,
    /// )?;
    /// let report = repo
    ///     .references()?
    ///     .pack(&repo.objects(PackLimits::default())?, PackRefsOptions::ALL)?;
    /// assert!(report.rewritten);
    /// assert_eq!(report.packed, 0);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn pack(
        &self,
        objects: &Objects,
        options: PackRefsOptions,
    ) -> Result<PackRefsReport, PackRefsError> {
        let _lock_operation = crate::locks::operation();
        if self.reference_backend != Backend::Files {
            return Err(ReferenceError::Unsupported("packing reftable references").into());
        }
        let cancel = AtomicBool::new(false);
        let lock = Lock::acquire_wait(
            self.common_dir.join("packed-refs"),
            options.lock_wait,
            &cancel,
        )?;
        let bytes = read_optional(&lock.destination)?.unwrap_or_default();
        let existing = packed::parse(self.object_format, &bytes, &lock.destination)?;
        let mut report = PackRefsReport::default();
        let mut loose = BTreeMap::new();
        self.collect_candidates(options.scope, &mut loose, &mut report)?;
        if options.trigger == PackTrigger::Auto && loose.len() < auto_threshold(bytes.len()) {
            return Ok(report);
        }

        let mut records = BTreeMap::new();
        for (name, record) in &existing {
            // Keep a recorded peel when the objects needed to recompute it are unavailable.
            let peeled = match peel(objects, name, record.target)? {
                Peel::Known(peeled) => peeled,
                Peel::Missing | Peel::TagTargetMissing => record.peeled_hint,
            };
            records.insert(name.clone(), (record.target, peeled));
        }
        let mut packed_loose = BTreeMap::new();
        for (name, candidate) in loose {
            let peeled = match peel(objects, &name, candidate.id)? {
                Peel::Known(peeled) => peeled,
                Peel::TagTargetMissing => None,
                Peel::Missing => {
                    report.skipped.push(name);
                    continue;
                }
            };
            records.insert(name.clone(), (candidate.id, peeled));
            packed_loose.insert(name, candidate);
        }
        lock.publish_synced(&encode(&records))?;
        report.rewritten = true;
        report.packed = packed_loose.len();
        if !cfg!(unix) {
            // Destructive maintenance is deferred on Windows (roadmap B08): without a nonadmin
            // durability barrier for the rename, keep every loose copy.
            report.retained.extend(packed_loose.into_keys());
            return Ok(report);
        }
        if let Err(source) =
            checkpoint(Checkpoint::Published).and_then(|()| confirm_rename(self.common_dir))
        {
            return Err(PackRefsError::Unconfirmed {
                report: Box::new(report),
                source,
            });
        }

        for (name, candidate) in packed_loose {
            match prune(self.object_format, &candidate) {
                Ok(true) => report.pruned += 1,
                Ok(false) => report.retained.push(name),
                Err(source) => {
                    return Err(PackRefsError::Prune {
                        report: Box::new(report),
                        source,
                    });
                }
            }
        }
        Ok(report)
    }

    fn collect_candidates(
        &self,
        scope: PackScope,
        candidates: &mut BTreeMap<RefName, Candidate>,
        report: &mut PackRefsReport,
    ) -> Result<(), ReferenceError> {
        let mut pending = vec![(self.common_dir.join("refs"), b"refs".to_vec())];
        while let Some((directory, prefix)) = pending.pop() {
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(io_error(&directory, source)),
            };
            for entry in entries {
                let entry = entry.map_err(|source| io_error(&directory, source))?;
                let file_type = entry
                    .file_type()
                    .map_err(|source| io_error(&entry.path(), source))?;
                let Some(component) = component_bytes(&entry.file_name()) else {
                    continue;
                };
                if component.starts_with(b".") || component.ends_with(b".lock") {
                    continue;
                }
                let mut bytes = prefix.clone();
                bytes.push(b'/');
                bytes.extend_from_slice(&component);
                if file_type.is_symlink() {
                    continue;
                }
                // A directory prefix need not be a valid full name itself, such as `refs/tags/a.`.
                if file_type.is_dir() {
                    pending.push((entry.path(), bytes));
                    continue;
                }
                let Ok(name) = RefName::new(&bytes) else {
                    continue;
                };
                if name.per_worktree() {
                    continue;
                }
                let selected = scope == PackScope::All || bytes.starts_with(b"refs/tags/");
                if !file_type.is_file() || !selected {
                    continue;
                }
                let path = entry.path();
                let Some(contents) = read_optional(&path)? else {
                    continue;
                };
                match parse_loose(self.object_format, &contents, &path) {
                    Ok(Target::Direct(id)) => {
                        candidates.insert(name, Candidate { path, id });
                    }
                    Ok(Target::Symbolic(_)) => (),
                    Err(_) => report.skipped.push(name),
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn component_bytes(name: &std::ffi::OsStr) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    Some(name.as_bytes().to_vec())
}

#[cfg(not(unix))]
fn component_bytes(name: &std::ffi::OsStr) -> Option<Vec<u8>> {
    name.to_str().map(|name| name.as_bytes().to_vec())
}

/// Makes the `packed-refs` rename in `directory` durable before loose copies are retired.
#[cfg(unix)]
fn confirm_rename(directory: &std::path::Path) -> Result<(), ReferenceError> {
    super::store::sync_directory(directory)
}

/// Unreachable in practice: [`References::pack`] never prunes on non-Unix hosts.
#[cfg(not(unix))]
fn confirm_rename(_: &std::path::Path) -> Result<(), ReferenceError> {
    Err(ReferenceError::Unsupported(
        "durable packed-refs replacement on this platform",
    ))
}

/// Peel result for one record.
enum Peel {
    /// The fully peeled ID for a tag, or `None` for any other object.
    Known(Option<ObjectId>),
    /// The object itself is missing.
    Missing,
    /// A tag in the chain points to a missing object.
    TagTargetMissing,
}

fn peel(objects: &Objects, name: &RefName, id: ObjectId) -> Result<Peel, PackRefsError> {
    match objects.peel(id, PeelLimits::trusted(), &AtomicBool::new(false)) {
        Ok(peeled) if peeled.tags.is_empty() => Ok(Peel::Known(None)),
        Ok(peeled) => Ok(Peel::Known(Some(peeled.target))),
        Err(error) if matches!(*error.source, PeelFailure::Missing) && error.depth == 0 => {
            Ok(Peel::Missing)
        }
        Err(error) if matches!(*error.source, PeelFailure::Missing) => Ok(Peel::TagTargetMissing),
        Err(source) => Err(PackRefsError::Peel {
            name: name.clone(),
            source: Box::new(source),
        }),
    }
}

/// A point in [`References::pack`] where tests can act or inject a failure.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Checkpoint {
    /// `packed-refs` was renamed; its directory is not yet synced and nothing is pruned.
    Published,
}

#[cfg(not(test))]
fn checkpoint(_: Checkpoint) -> Result<(), ReferenceError> {
    Ok(())
}

#[cfg(test)]
type Hook = Box<dyn FnMut(Checkpoint) -> Result<(), ReferenceError>>;

#[cfg(test)]
thread_local! {
    static HOOK: std::cell::RefCell<Option<Hook>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn checkpoint(at: Checkpoint) -> Result<(), ReferenceError> {
    HOOK.with(|hook| hook.borrow_mut().as_mut().map_or(Ok(()), |hook| hook(at)))
}

fn encode(records: &BTreeMap<RefName, (ObjectId, Option<ObjectId>)>) -> Vec<u8> {
    let mut bytes = HEADER.to_vec();
    for (name, (id, peeled)) in records {
        bytes.extend_from_slice(id.to_string().as_bytes());
        bytes.push(b' ');
        bytes.extend_from_slice(name.as_bytes());
        bytes.push(b'\n');
        if let Some(peeled) = peeled {
            bytes.push(b'^');
            bytes.extend_from_slice(peeled.to_string().as_bytes());
            bytes.push(b'\n');
        }
    }
    bytes
}

/// Removes a packed loose file if it still holds the packed value. Returns whether it was removed.
fn prune(format: crate::ObjectFormat, candidate: &Candidate) -> Result<bool, ReferenceError> {
    let lock = match Lock::acquire(candidate.path.clone()) {
        Ok(lock) => lock,
        Err(ReferenceError::Locked(_)) => return Ok(false),
        Err(error) => return Err(error),
    };
    let Some(bytes) = read_optional(&candidate.path)? else {
        return Ok(true);
    };
    if !matches!(parse_loose(format, &bytes, &candidate.path), Ok(Target::Direct(id)) if id == candidate.id)
    {
        return Ok(false);
    }
    lock.check_owned()?;
    match fs::remove_file(&candidate.path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(source) => Err(io_error(&candidate.path, source)),
    }
}

/// Minimum loose candidate count for [`PackTrigger::Auto`], from the `packed-refs` size.
fn auto_threshold(packed_bytes: usize) -> usize {
    const BASE: f64 = 12_288.0;
    if packed_bytes == 0 {
        return 16;
    }
    let doublings = (packed_bytes as f64 / BASE).log2().floor();
    ((35.0 + 5.0 * doublings).max(16.0)) as usize
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::path::Path;
    #[cfg(unix)]
    use std::process::{Command, Output};

    use rstest::rstest;

    use super::*;
    #[cfg(unix)]
    use crate::{PackLimits, Repository};

    #[cfg(unix)]
    fn git_output(repo: &Path, args: &[&str]) -> Output {
        Command::new("git")
            .current_dir(repo)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "A")
            .env("GIT_AUTHOR_EMAIL", "a@example.com")
            .env("GIT_COMMITTER_NAME", "C")
            .env("GIT_COMMITTER_EMAIL", "c@example.com")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .args(args)
            .output()
            .expect("Git is required for interoperability tests")
    }

    #[cfg(unix)]
    fn git(repo: &Path, args: &[&str]) -> String {
        let output = git_output(repo, args);
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    #[cfg(unix)]
    /// Two commits, `topic` at the first, and an annotated tag; every ref loose.
    fn fixture() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "repo"]);
        let repo = root.path().join("repo");
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "one"]);
        git(&repo, &["branch", "topic"]);
        git(&repo, &["tag", "-a", "-m", "t", "v1"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "two"]);
        let first = git(&repo, &["rev-parse", "HEAD~1"]).trim().to_owned();
        let second = git(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
        (root, repo, first, second)
    }

    #[cfg(unix)]
    fn view(repo: &Path) -> String {
        git(
            repo,
            &[
                "for-each-ref",
                "--format=%(objectname) %(refname) %(symref)",
            ],
        )
    }

    #[cfg(unix)]
    /// Packs with `hook` run after the rename and before the directory sync and pruning.
    fn pack_with_hook(
        repo: &Path,
        hook: impl FnMut(Checkpoint) -> Result<(), ReferenceError> + 'static,
    ) -> Result<PackRefsReport, PackRefsError> {
        HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
        let repository = Repository::open(repo).unwrap();
        let result = repository.references().unwrap().pack(
            &repository.objects(PackLimits::default()).unwrap(),
            PackRefsOptions::ALL,
        );
        HOOK.with(|slot| *slot.borrow_mut() = None);
        result
    }

    #[cfg(unix)]
    #[test]
    fn git_update_between_publish_and_prune_stays_loose_and_shadows() {
        let (_root, repo, first, second) = fixture();
        let hook_repo = repo.clone();
        let report = pack_with_hook(&repo, move |_| {
            git(&hook_repo, &["update-ref", "refs/heads/topic", &second]);
            Ok(())
        })
        .unwrap();
        assert_eq!(report.retained, [RefName::new("refs/heads/topic").unwrap()]);
        let packed = fs::read_to_string(repo.join(".git/packed-refs")).unwrap();
        assert!(packed.contains(&format!("{first} refs/heads/topic\n")));
        assert_eq!(
            git(&repo, &["rev-parse", "topic"]).trim(),
            git(&repo, &["rev-parse", "HEAD"]).trim()
        );
    }

    #[cfg(unix)]
    #[test]
    fn ref_becoming_symbolic_between_publish_and_prune_stays_loose() {
        let (_root, repo, _, _) = fixture();
        let hook_repo = repo.clone();
        let report = pack_with_hook(&repo, move |_| {
            git(
                &hook_repo,
                &["symbolic-ref", "refs/heads/topic", "refs/heads/master"],
            );
            Ok(())
        })
        .unwrap();
        assert_eq!(report.retained, [RefName::new("refs/heads/topic").unwrap()]);
        assert_eq!(
            git(&repo, &["symbolic-ref", "refs/heads/topic"]).trim(),
            "refs/heads/master"
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_deletion_is_refused_while_packing() {
        let (_root, repo, _, _) = fixture();
        let hook_repo = repo.clone();
        pack_with_hook(&repo, move |_| {
            let output = git_output(
                &hook_repo,
                &[
                    "-c",
                    "core.packedRefsTimeout=0",
                    "update-ref",
                    "-d",
                    "refs/tags/v1",
                ],
            );
            assert!(!output.status.success());
            Ok(())
        })
        .unwrap();
        git(&repo, &["show-ref", "--verify", "refs/tags/v1"]);
    }

    #[cfg(unix)]
    #[test]
    fn failure_after_rename_keeps_every_loose_copy_and_reports_replacement() {
        let (_root, repo, _, _) = fixture();
        let before = view(&repo);
        let loose = ["refs/heads/master", "refs/heads/topic", "refs/tags/v1"];
        let hook_repo = repo.clone();
        let hook_before = before.clone();
        let result = pack_with_hook(&repo, move |_| {
            // A crash here leaves the renamed file plus every loose file with equal values.
            assert_eq!(view(&hook_repo), hook_before);
            Err(ReferenceError::Unsupported(
                "injected directory sync failure",
            ))
        });
        let Err(PackRefsError::Unconfirmed { report, .. }) = result else {
            panic!("expected an unconfirmed replacement: {result:?}");
        };
        assert!(report.rewritten);
        assert_eq!(report.pruned, 0);
        for name in loose {
            assert!(repo.join(".git").join(name).exists(), "{name}");
        }
        assert_eq!(view(&repo), before);
        assert!(!repo.join(".git/packed-refs.lock").exists());
    }

    // Observed with `git pack-refs --auto` on Git 2.56.0: the smallest loose tag count packed.
    #[rstest]
    #[case(0, 16)]
    #[case(1_066, 16)]
    #[case(2_596, 20)]
    #[case(5_246, 25)]
    #[case(10_446, 30)]
    #[case(15_646, 35)]
    #[case(26_046, 40)]
    #[case(53_046, 45)]
    #[case(106_046, 50)]
    #[case(212_046, 55)]
    #[case(424_046, 60)]
    #[case(864_046, 65)]
    fn auto_threshold_matches_observed_git(#[case] size: usize, #[case] threshold: usize) {
        assert_eq!(auto_threshold(size), threshold);
    }
}
