use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::{RefName, packed};
use crate::{ObjectId, Repository};

/// A files-backend reference store borrowed from an opened [`Repository`].
///
/// Supports SHA-1 references on Unix filesystems. `HEAD`, `refs/bisect/`, `refs/rewritten/`, and
/// `refs/worktree/` use the current worktree's Git directory; other `refs/` names and packed refs
/// use the common directory. Cross-worktree aliases and other pseudorefs are not supported.
/// Filesystem symlinks within these paths are rejected, including legacy symlink HEADs.
///
/// Operations are synchronous. Reads are live, not a snapshot across multiple refs or symbolic
/// hops. Loose files take precedence, including malformed loose files (no fallback on errors).
/// Packed lookup validates the whole file and allocates proportional to its size; there is no
/// configurable byte limit or cache. Peel records are validated but never returned as resolution.
///
/// Writes require a trusted repository on a local filesystem with exclusive file creation and
/// atomic rename semantics. Cooperating writers must honor Git's `.lock` protocol; adversarial
/// path replacement, network filesystems, and changing repository configuration/layout while a
/// handle is in use are outside this contract.
#[derive(Clone, Copy, Debug)]
pub struct References<'a> {
    pub(super) repository: &'a Repository,
}

/// The stored value of one reference, before symbolic resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    /// SHA-1 object identity. Existence and object type are not checked.
    Direct(ObjectId),
    /// Another full reference name; it may be missing or form a cycle.
    Symbolic(RefName),
}

/// A precondition checked under locks against the destination's stored value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Expected {
    /// Accept any valid current value or absence.
    Any,
    /// Require neither a loose nor a packed value; deletion then succeeds without a change.
    Absent,
    /// Require this exact direct ID or symbolic name (without dereferencing it).
    Value(Target),
}

/// The terminal name and optional object identity reached by symbolic resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Resolution {
    /// Last name visited; also identifies the missing target of an unborn/dangling chain.
    pub name: RefName,
    /// `None` means the terminal reference is absent, not that its object is missing.
    pub id: Option<ObjectId>,
}

/// Reference validation, storage, resolution, or update failure.
#[derive(Debug, thiserror::Error)]
pub enum ReferenceError {
    /// Filesystem failure, with its original cause and path.
    #[error("reference I/O at {path}: {source}")]
    Io {
        /// Affected file or directory.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// Packed deletion succeeded, but removing the loose file failed. Packed bytes are not
    /// restored.
    #[error("packed reference removed, but loose deletion failed at {path}: {source}")]
    PackedDeleted {
        /// Loose reference that could not be removed.
        path: PathBuf,
        /// Underlying unlink failure. The current loose value remains under the writer contract.
        #[source]
        source: io::Error,
    },
    /// Invalid on-disk data; a broken loose ref never falls back to a packed value.
    #[error("malformed reference at {path}: {reason}")]
    Malformed {
        /// File containing invalid data.
        path: PathBuf,
        /// Violated format rule.
        reason: &'static str,
    },
    /// Recognized feature outside this implementation's supported boundary.
    #[error("unsupported reference feature: {0}")]
    Unsupported(&'static str),
    /// Another writer owns a lock. No waiting, lock stealing, or automatic retry is performed.
    #[error("reference lock already exists: {0}")]
    Locked(PathBuf),
    /// The destination did not satisfy the caller's precondition.
    #[error("reference expectation did not match; actual target: {actual:?}")]
    Mismatch {
        /// Actual stored value under the lock, or absence.
        actual: Option<Target>,
    },
    /// A symbolic chain revisited a name.
    #[error("symbolic reference cycle at {0:?}")]
    Cycle(RefName),
    /// Resolution would follow more symbolic links than allowed.
    #[error("symbolic reference depth exceeds {0}")]
    Depth(usize),
    /// A reference occupies an ancestor or descendant of the requested name.
    #[error("reference namespace conflict at {0}")]
    Conflict(PathBuf),
    /// Symbolic HEAD must point into `refs/`, not back to the pseudoref HEAD.
    #[error("symbolic HEAD must point into refs/")]
    InvalidHeadTarget,
    /// Zero IDs cannot be stored; use an explicit deletion method instead.
    #[error("zero reference IDs are not supported")]
    ZeroId,
}

impl<'a> References<'a> {
    pub(crate) fn new(repository: &'a Repository) -> Result<Self, ReferenceError> {
        if !cfg!(unix) {
            return Err(ReferenceError::Unsupported(
                "reference storage on non-Unix platforms",
            ));
        }
        Ok(Self { repository })
    }

    /// Reads a stored direct or symbolic target without following symbolic links.
    ///
    /// Returns `None` for an absent name. Packed records are consulted only when the loose file
    /// is absent; per-worktree names never use packed fallback. A directory at the exact name
    /// denotes a namespace conflict, not absence.
    ///
    /// # Errors
    ///
    /// Reports I/O, malformed loose/packed data, unsupported packed traits, and filesystem
    /// symlinks.
    pub fn read(&self, name: &RefName) -> Result<Option<Target>, ReferenceError> {
        let path = self.path(name)?;
        if let Some(bytes) = read_optional(&path)? {
            return parse_loose(&bytes, &path).map(Some);
        }
        if name.per_worktree() {
            return Ok(None);
        }
        Ok(self.packed()?.get(name).copied().map(Target::Direct))
    }

    /// Follows at most `max_depth` symbolic links and returns the terminal name and identity.
    ///
    /// A missing initial name or dangling/unborn target returns `id: None`; a missing object is
    /// not detected. Zero depth permits a direct or missing ref but no symbolic hop. Reads across
    /// hops are not atomic. This does not peel tags or open objects.
    ///
    /// # Errors
    ///
    /// Reports [`ReferenceError::Cycle`], [`ReferenceError::Depth`], or a [`Self::read`] failure.
    pub fn resolve(&self, name: &RefName, max_depth: usize) -> Result<Resolution, ReferenceError> {
        let mut current = name.clone();
        let mut seen = Vec::new();
        loop {
            if seen.contains(&current) {
                return Err(ReferenceError::Cycle(current));
            }
            seen.push(current.clone());
            match self.read(&current)? {
                Some(Target::Symbolic(next)) => {
                    if seen.len() > max_depth {
                        return Err(ReferenceError::Depth(max_depth));
                    }
                    current = next;
                }
                target => {
                    return Ok(Resolution {
                        name: current,
                        id: direct_id(target),
                    });
                }
            }
        }
    }

    /// Replaces the named reference itself, without dereferencing it or writing reflogs.
    ///
    /// `expected` compares its stored target, including any packed value. The new loose file
    /// shadows packed data; packed-refs is never rewritten. Symbolic targets may be dangling or
    /// cyclic, except HEAD itself must point into `refs/`. IDs must be nonzero, but object
    /// existence, type, and fast-forward rules are not
    /// checked. Use [`Self::update_resolved_without_reflog`] to advance a symbolic ref's target.
    ///
    /// This is a low-level publication primitive, **not full `git update-ref` equivalence**.
    /// It ignores `core.logAllRefUpdates`, leaves existing reflogs unchanged, and runs no hooks.
    /// Prior tips gain no new reflog retention or recovery entry; reflog selectors may be stale,
    /// and old objects can become eligible for pruning. Callers must deliberately accept that
    /// behavior. See [`Self::transaction`] for conditional batches with explicit reflog policy.
    ///
    /// Locks `packed-refs.lock`, then `<name>.lock`, using exclusive creation. Checks the
    /// precondition and packed namespace while locked, writes the owned lock, and atomically
    /// renames it over the destination. Existing locks are never overwritten or removed. A failure
    /// before rename preserves existing reference bytes; newly created empty parent directories
    /// may remain. Owned lock cleanup is best effort on errors/unwind; process termination or
    /// cleanup I/O failure may leave stale locks. No file/directory fsync is performed: successful
    /// visibility does not promise survival of a crash or power loss, or durability of objects.
    ///
    /// # Errors
    ///
    /// Reports lock contention, expectation mismatch, namespace conflict (including empty
    /// directories), malformed existing data, unsupported paths, zero IDs, and I/O failures.
    /// No retry is performed. See [`References`] for filesystem/concurrent-writer assumptions.
    pub fn update_without_reflog(
        &self,
        name: &RefName,
        target: Target,
        expected: Expected,
    ) -> Result<(), ReferenceError> {
        if name.as_bytes() == b"HEAD"
            && matches!(&target, Target::Symbolic(next) if next.as_bytes() == b"HEAD")
        {
            return Err(ReferenceError::InvalidHeadTarget);
        }
        validate_target(&target)?;
        let _packed_lock = Lock::acquire(self.repository.common_dir().join("packed-refs"))?;
        let packed = self.packed()?;
        self.check_packed_namespace(name, &packed)?;
        let mut lock = Lock::acquire(self.path(name)?)?;
        check_expected(self.read_locked(name, &packed)?, expected)?;
        lock.publish(&target)
    }

    /// Advances a terminal direct/missing reference while keeping the symbolic chain unchanged.
    ///
    /// Holds `packed-refs.lock` and every visited name's lock through publication, with the same
    /// no-reflog, namespace, cleanup, concurrency and durability contracts as
    /// [`Self::update_without_reflog`]. `expected` applies to the terminal name, so `Absent` can
    /// publish an unborn branch through HEAD. Returns the name that was updated. At most 32
    /// symbolic hops are allowed. Changing a symbolic reference itself requires the other method.
    ///
    /// # Errors
    ///
    /// Reports cycle/depth failures or any error from [`Self::update_without_reflog`]. Acquired
    /// locks are released on failure; no reference is published until the whole chain is locked.
    pub fn update_resolved_without_reflog(
        &self,
        name: &RefName,
        id: ObjectId,
        expected: Expected,
    ) -> Result<RefName, ReferenceError> {
        let target = Target::Direct(id);
        validate_target(&target)?;
        let _packed_lock = Lock::acquire(self.repository.common_dir().join("packed-refs"))?;
        let packed = self.packed()?;
        let (current, actual, mut locks) = self.lock_resolution(name, &packed)?;
        check_expected(actual, expected)?;
        locks.last_mut().unwrap().publish(&target)?;
        Ok(current)
    }

    /// Deletes the named reference itself, without dereferencing it or changing reflogs.
    ///
    /// Compares `expected` with the stored loose-over-packed target under `packed-refs.lock`
    /// and the name's lock. `Any` and `Absent` permit an already absent name. A symbolic value
    /// is compared and removed as a name; its target is untouched. Use
    /// [`Self::delete_resolved_without_reflog`] to delete the terminal ref instead.
    ///
    /// Removes the packed record and its peel line before unlinking the loose file, so deletion
    /// cannot uncover an older packed value. Unrelated packed bytes (including header traits,
    /// order and peel records) are preserved. A separate temporary file publishes packed data
    /// while the packed lock remains held through loose deletion. Both locks are held until
    /// completion. Empty parent directories and existing reflogs remain; hooks are not run.
    /// The no-reflog and filesystem assumptions of [`Self::update_without_reflog`] apply.
    ///
    /// Reads remain live: a reader with previously read packed data can observe a stale value.
    /// This is not a snapshot, multi-ref transaction, or crash-durable operation. A subsequent
    /// writer can recreate the name after locks are released.
    ///
    /// # Errors
    ///
    /// Lock, namespace, malformed-data and expectation failures preserve reference bytes.
    /// Lock acquisition may leave empty parent directories. Packed publication failure preserves
    /// both values. If packed publication succeeds but loose removal fails, returns
    /// [`ReferenceError::PackedDeleted`]: the loose value remains, with no rollback of packed
    /// storage. Other I/O failures use [`ReferenceError::Io`]. Lock/temporary cleanup is best
    /// effort, including after success; cleanup failure or termination can leave stale files.
    /// No fsync, retries, reflog cleanup, or object deletion is performed.
    pub fn delete_without_reflog(
        &self,
        name: &RefName,
        expected: Expected,
    ) -> Result<(), ReferenceError> {
        let packed_lock = Lock::acquire(self.repository.common_dir().join("packed-refs"))?;
        let bytes = read_optional(&packed_lock.destination)?.unwrap_or_default();
        let packed = packed::parse(&bytes, &packed_lock.destination)?;
        self.check_packed_namespace(name, &packed)?;
        let lock = Lock::acquire(self.path(name)?)?;
        check_expected(self.read_locked(name, &packed)?, expected)?;
        delete_locked(&packed_lock, &lock, name, &bytes, &packed)
    }

    /// Deletes the terminal direct/missing ref while preserving every symbolic name in its chain.
    ///
    /// `expected` applies to the terminal stored value. Holds the packed lock and every visited
    /// name's lock through deletion; at most 32 symbolic hops are allowed. Returns the terminal
    /// name, including when already absent. Deleting a branch through HEAD leaves HEAD unborn;
    /// deleting through an alias leaves the alias dangling.
    ///
    /// # Errors
    ///
    /// Reports cycle/depth failures or errors from [`Self::delete_without_reflog`], with the same
    /// packed-first ordering, partial-failure, cleanup, live-reader and no-reflog contracts.
    pub fn delete_resolved_without_reflog(
        &self,
        name: &RefName,
        expected: Expected,
    ) -> Result<RefName, ReferenceError> {
        let packed_lock = Lock::acquire(self.repository.common_dir().join("packed-refs"))?;
        let bytes = read_optional(&packed_lock.destination)?.unwrap_or_default();
        let packed = packed::parse(&bytes, &packed_lock.destination)?;
        let (current, actual, locks) = self.lock_resolution(name, &packed)?;
        check_expected(actual, expected)?;
        delete_locked(
            &packed_lock,
            locks.last().unwrap(),
            &current,
            &bytes,
            &packed,
        )?;
        Ok(current)
    }

    fn lock_resolution(
        &self,
        name: &RefName,
        packed: &packed::Packed,
    ) -> Result<(RefName, Option<Target>, Vec<Lock>), ReferenceError> {
        let mut current = name.clone();
        let mut seen = Vec::new();
        let mut locks = Vec::new();
        loop {
            if seen.contains(&current) {
                return Err(ReferenceError::Cycle(current));
            }
            seen.push(current.clone());
            self.check_packed_namespace(&current, packed)?;
            locks.push(Lock::acquire(self.path(&current)?)?);
            match self.read_locked(&current, packed)? {
                Some(Target::Symbolic(next)) => {
                    if seen.len() > 32 {
                        return Err(ReferenceError::Depth(32));
                    }
                    current = next;
                }
                actual => return Ok((current, actual, locks)),
            }
        }
    }

    pub(super) fn read_locked(
        &self,
        name: &RefName,
        packed: &packed::Packed,
    ) -> Result<Option<Target>, ReferenceError> {
        let path = self.path(name)?;
        if let Some(bytes) = read_optional(&path)? {
            return parse_loose(&bytes, &path).map(Some);
        }
        Ok(packed
            .get(name)
            .filter(|_| !name.per_worktree())
            .copied()
            .map(Target::Direct))
    }

    pub(super) fn packed(&self) -> Result<packed::Packed, ReferenceError> {
        let path = self.repository.common_dir().join("packed-refs");
        let bytes = read_optional(&path)?.unwrap_or_default();
        packed::parse(&bytes, &path)
    }

    pub(super) fn path(&self, name: &RefName) -> Result<PathBuf, ReferenceError> {
        let base = if name.per_worktree() {
            self.repository.git_dir()
        } else {
            self.repository.common_dir()
        };
        #[cfg(unix)]
        let relative = {
            use std::os::unix::ffi::OsStrExt;
            std::ffi::OsStr::from_bytes(name.as_bytes())
        };
        #[cfg(not(unix))]
        let relative = std::ffi::OsStr::new(
            std::str::from_utf8(name.as_bytes())
                .map_err(|_| ReferenceError::Unsupported("non-UTF-8 filesystem name"))?,
        );
        Ok(base.join(relative))
    }

    pub(super) fn check_packed_namespace(
        &self,
        name: &RefName,
        packed: &packed::Packed,
    ) -> Result<(), ReferenceError> {
        if !name.per_worktree()
            && packed
                .keys()
                .any(|other| conflicts(name.as_bytes(), other.as_bytes()))
        {
            return Err(ReferenceError::Conflict(self.path(name)?));
        }
        Ok(())
    }
}

// Publish packed removal while the loose value still masks the old packed value. Keeping the
// packed lock separate from the replacement file excludes packed writers through both steps.
fn delete_locked(
    packed_lock: &Lock,
    loose_lock: &Lock,
    name: &RefName,
    bytes: &[u8],
    packed: &packed::Packed,
) -> Result<(), ReferenceError> {
    let packed_changed = packed.contains_key(name);
    if packed_changed {
        let replacement = packed::without_ref(bytes, name);
        packed_lock.publish_retaining_lock(&replacement)?;
    }
    remove_loose(&loose_lock.destination, packed_changed)
}

pub(super) fn remove_loose(path: &Path, packed_changed: bool) -> Result<(), ReferenceError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) if packed_changed => Err(ReferenceError::PackedDeleted {
            path: path.into(),
            source,
        }),
        Err(source) => Err(io_error(path, source)),
    }
}

pub(super) fn conflicts(a: &[u8], b: &[u8]) -> bool {
    a.strip_prefix(b).is_some_and(|rest| rest.starts_with(b"/"))
        || b.strip_prefix(a).is_some_and(|rest| rest.starts_with(b"/"))
}
fn direct_id(target: Option<Target>) -> Option<ObjectId> {
    match target {
        Some(Target::Direct(id)) => Some(id),
        _ => None,
    }
}
pub(super) fn validate_target(target: &Target) -> Result<(), ReferenceError> {
    if matches!(target, Target::Direct(id) if id.as_bytes() == &[0; 20]) {
        return Err(ReferenceError::ZeroId);
    }
    Ok(())
}
pub(super) fn check_expected(
    actual: Option<Target>,
    expected: Expected,
) -> Result<(), ReferenceError> {
    let matches = match expected {
        Expected::Any => true,
        Expected::Absent => actual.is_none(),
        Expected::Value(value) => actual.as_ref() == Some(&value),
    };
    if !matches {
        return Err(ReferenceError::Mismatch { actual });
    }
    Ok(())
}

pub(super) fn parse_loose(bytes: &[u8], path: &Path) -> Result<Target, ReferenceError> {
    // Git accepts trailing ASCII whitespace, including no final newline.
    let end = bytes
        .iter()
        .rposition(|b| !b.is_ascii_whitespace())
        .map_or(0, |i| i + 1);
    let bytes = &bytes[..end];
    if let Some(name) = bytes.strip_prefix(b"ref: ") {
        let name = RefName::new(name).map_err(|_| malformed(path, "invalid symbolic target"))?;
        return Ok(Target::Symbolic(name));
    }
    packed::parse_id(bytes, path).map(Target::Direct)
}

pub(super) fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, ReferenceError> {
    check_path(path)?;
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(path, source)),
    }
}

// The opened roots are canonical. Refuse symlink traversal below them, and symlink files.
// This is a trusted-directory check, not a race-proof sandbox against malicious path swaps.
pub(super) fn check_path(path: &Path) -> Result<(), ReferenceError> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ReferenceError::Unsupported(
                    "filesystem symlink in reference path",
                ));
            }
            Ok(metadata) if ancestor != path && !metadata.is_dir() => {
                return Err(ReferenceError::Conflict(ancestor.into()));
            }
            Ok(metadata) if ancestor == path && metadata.is_dir() => {
                return Err(ReferenceError::Conflict(path.into()));
            }
            Ok(metadata) if ancestor == path && !metadata.is_file() => {
                return Err(ReferenceError::Unsupported("non-regular reference file"));
            }
            Ok(_) => (),
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.kind() == io::ErrorKind::NotADirectory => {}
            Err(source) => return Err(io_error(ancestor, source)),
        }
    }
    Ok(())
}

pub(super) struct Lock {
    pub(super) destination: PathBuf,
    path: PathBuf,
    file: File,
    published: bool,
}
impl Lock {
    pub(super) fn acquire(destination: PathBuf) -> Result<Self, ReferenceError> {
        check_path(&destination)?;
        let parent = destination.parent().unwrap();
        fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
        let mut path = destination.as_os_str().to_os_string();
        path.push(".lock");
        let path = PathBuf::from(path);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| {
                if source.kind() == io::ErrorKind::AlreadyExists {
                    ReferenceError::Locked(path.clone())
                } else {
                    io_error(&path, source)
                }
            })?;
        Ok(Self {
            destination,
            path,
            file,
            published: false,
        })
    }
    pub(super) fn publish_retaining_lock(&self, bytes: &[u8]) -> Result<(), ReferenceError> {
        let parent = self.destination.parent().unwrap();
        let mut temporary = tempfile::Builder::new()
            .prefix(".girt-packed-")
            .tempfile_in(parent)
            .map_err(|source| io_error(parent, source))?;
        // Match ordinary lock creation permissions, including the process umask.
        let permissions = self
            .file
            .metadata()
            .map_err(|source| io_error(&self.path, source))?
            .permissions();
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|source| io_error(temporary.path(), source))?;
        temporary
            .write_all(bytes)
            .map_err(|source| io_error(temporary.path(), source))?;
        temporary
            .persist(&self.destination)
            .map_err(|error| io_error(&self.destination, error.error))?;
        Ok(())
    }

    fn publish(&mut self, target: &Target) -> Result<(), ReferenceError> {
        let mut bytes = match target {
            Target::Direct(id) => id.to_string().into_bytes(),
            Target::Symbolic(name) => {
                let mut bytes = b"ref: ".to_vec();
                bytes.extend_from_slice(name.as_bytes());
                bytes
            }
        };
        bytes.push(b'\n');
        self.file
            .write_all(&bytes)
            .map_err(|source| io_error(&self.path, source))?;
        fs::rename(&self.path, &self.destination)
            .map_err(|source| io_error(&self.destination, source))?;
        self.published = true;
        Ok(())
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if !self.published {
            let _ = fs::remove_file(&self.path);
        }
    }
}
pub(super) fn io_error(path: &Path, source: io::Error) -> ReferenceError {
    ReferenceError::Io {
        path: path.into(),
        source,
    }
}
pub(super) fn malformed(path: &Path, reason: &'static str) -> ReferenceError {
    ReferenceError::Malformed {
        path: path.into(),
        reason,
    }
}

#[cfg(all(test, unix))]
mod tests {
    use rstest::rstest;

    use super::*;

    fn fixture() -> (tempfile::TempDir, Repository) {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("objects")).unwrap();
        fs::create_dir(temp.path().join("refs")).unwrap();
        fs::write(temp.path().join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        let repository = Repository::open(temp.path()).unwrap();
        (temp, repository)
    }
    fn name(value: &[u8]) -> RefName {
        RefName::new(value).unwrap()
    }
    fn id() -> ObjectId {
        ObjectId::from_bytes([1; 20])
    }

    #[rstest]
    #[case::no_newline(b"1111111111111111111111111111111111111111")]
    #[case::trailing_space(b"1111111111111111111111111111111111111111 \r\n\t")]
    fn parses_loose_ids(#[case] bytes: &[u8]) {
        assert_eq!(
            parse_loose(bytes, Path::new("ref")).unwrap(),
            Target::Direct(ObjectId::from_bytes([0x11; 20]))
        );
    }

    #[rstest]
    #[case::empty(b"")]
    #[case::garbage(b"garbage\n")]
    #[case::zero(b"0000000000000000000000000000000000000000\n")]
    #[case::sha256(b"1111111111111111111111111111111111111111111111111111111111111111\n")]
    #[case::bad_symbol(b"ref: refs/../main\n")]
    #[case::extra_line(b"1111111111111111111111111111111111111111\nextra\n")]
    fn rejects_loose_data(#[case] bytes: &[u8]) {
        assert!(matches!(
            parse_loose(bytes, Path::new("ref")),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[test]
    fn unborn_resolution_identifies_missing_terminal() {
        let (_temp, repo) = fixture();
        let refs = repo.references().unwrap();
        assert_eq!(
            refs.resolve(&name(b"HEAD"), 1).unwrap(),
            Resolution {
                name: name(b"refs/heads/main"),
                id: None
            }
        );
        assert!(matches!(
            refs.resolve(&name(b"HEAD"), 0),
            Err(ReferenceError::Depth(0))
        ));
        assert_eq!(refs.resolve(&name(b"refs/absent"), 0).unwrap().id, None);
    }

    #[test]
    fn cycle_failure_cleans_all_owned_locks() {
        let (_temp, repo) = fixture();
        let refs = repo.references().unwrap();
        refs.update_without_reflog(
            &name(b"refs/heads/main"),
            Target::Symbolic(name(b"HEAD")),
            Expected::Absent,
        )
        .unwrap();
        assert!(matches!(
            refs.resolve(&name(b"HEAD"), 32),
            Err(ReferenceError::Cycle(_))
        ));
        assert!(matches!(
            refs.update_resolved_without_reflog(&name(b"HEAD"), id(), Expected::Any),
            Err(ReferenceError::Cycle(_))
        ));
        assert!(!repo.git_dir().join("HEAD.lock").exists());
        assert!(!repo.git_dir().join("refs/heads/main.lock").exists());
        assert!(!repo.common_dir().join("packed-refs.lock").exists());
    }

    #[test]
    fn mismatch_preserves_bytes_and_cleans_locks() {
        let (_temp, repo) = fixture();
        let refs = repo.references().unwrap();
        let before = fs::read(repo.git_dir().join("HEAD")).unwrap();
        assert!(matches!(
            refs.update_without_reflog(&name(b"HEAD"), Target::Direct(id()), Expected::Absent),
            Err(ReferenceError::Mismatch { .. })
        ));
        assert_eq!(fs::read(repo.git_dir().join("HEAD")).unwrap(), before);
        assert!(!repo.git_dir().join("HEAD.lock").exists());
        assert!(!repo.common_dir().join("packed-refs.lock").exists());
    }

    #[rstest]
    #[case::reference("HEAD.lock")]
    #[case::packed("packed-refs.lock")]
    fn never_steals_existing_lock(#[case] lock_name: &str) {
        let (_temp, repo) = fixture();
        let lock_path = repo.git_dir().join(lock_name);
        fs::write(&lock_path, b"another writer").unwrap();
        assert!(matches!(
            repo.references().unwrap().update_without_reflog(
                &name(b"HEAD"),
                Target::Direct(id()),
                Expected::Any
            ),
            Err(ReferenceError::Locked(_))
        ));
        assert_eq!(fs::read(lock_path).unwrap(), b"another writer");
        assert_eq!(
            fs::read(repo.git_dir().join("HEAD")).unwrap(),
            b"ref: refs/heads/main\n"
        );
    }

    #[test]
    fn write_failure_preserves_destination_and_releases_lock() {
        let (_temp, repo) = fixture();
        let path = repo.git_dir().join("HEAD");
        let mut lock = Lock::acquire(path.clone()).unwrap();
        // A read-only handle injects a deterministic write failure without chmod/root assumptions.
        lock.file = File::open(&lock.path).unwrap();
        assert!(lock.publish(&Target::Direct(id())).is_err());
        drop(lock);
        assert_eq!(fs::read(path).unwrap(), b"ref: refs/heads/main\n");
        assert!(!repo.git_dir().join("HEAD.lock").exists());
    }

    #[test]
    fn failed_rename_preserves_unrelated_data() {
        let (_temp, repo) = fixture();
        let path = repo.git_dir().join("refs/new");
        let mut lock = Lock::acquire(path.clone()).unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("unrelated"), b"keep").unwrap();
        assert!(lock.publish(&Target::Direct(id())).is_err());
        drop(lock);
        assert_eq!(fs::read(path.join("unrelated")).unwrap(), b"keep");
        assert!(!repo.git_dir().join("refs/new.lock").exists());
    }

    #[test]
    fn zero_id_has_no_side_effects() {
        let (_temp, repo) = fixture();
        assert!(matches!(
            repo.references().unwrap().update_without_reflog(
                &name(b"refs/new/deep"),
                Target::Direct(ObjectId::from_bytes([0; 20])),
                Expected::Any
            ),
            Err(ReferenceError::ZeroId)
        ));
        assert!(!repo.git_dir().join("refs/new").exists());
        assert!(!repo.common_dir().join("packed-refs.lock").exists());
    }

    #[cfg(unix)]
    #[rstest]
    #[case::directory("refs/link", "refs/link/a")]
    #[case::file("refs/link", "refs/link")]
    fn rejects_symlinks_without_touching_target(#[case] link: &str, #[case] reference: &str) {
        let (temp, repo) = fixture();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("a"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join(link)).unwrap();
        let refs = repo.references().unwrap();
        assert!(matches!(
            refs.read(&name(reference.as_bytes())),
            Err(ReferenceError::Unsupported(_))
        ));
        assert!(matches!(
            refs.update_without_reflog(
                &name(reference.as_bytes()),
                Target::Direct(id()),
                Expected::Any
            ),
            Err(ReferenceError::Unsupported(_))
        ));
        assert_eq!(fs::read(outside.path().join("a")).unwrap(), b"keep");
    }
}

#[cfg(all(test, unix))]
mod chain_tests {
    use super::*;

    // Independently encode a chain to exercise the update depth limit and every owned lock.
    fn chain(root: &Path, hops: usize) {
        fs::create_dir_all(root.join("objects")).unwrap();
        fs::create_dir_all(root.join("refs/heads")).unwrap();
        fs::write(root.join("HEAD"), b"ref: refs/heads/0\n").unwrap();
        for index in 0..hops {
            fs::write(
                root.join(format!("refs/heads/{index}")),
                format!("ref: refs/heads/{}\n", index + 1),
            )
            .unwrap();
        }
    }

    #[test]
    fn depth_failure_releases_chain_locks() {
        let root = tempfile::tempdir().unwrap();
        chain(root.path(), 33);
        let repo = Repository::open(root.path()).unwrap();
        let refs = repo.references().unwrap();
        let head = RefName::new(b"HEAD").unwrap();
        assert!(matches!(
            refs.update_resolved_without_reflog(
                &head,
                ObjectId::from_bytes([1; 20]),
                Expected::Absent
            ),
            Err(ReferenceError::Depth(32))
        ));
        assert_eq!(refs.resolve(&head, 34).unwrap().id, None);
        assert_eq!(
            fs::read_dir(root.path().join("refs/heads"))
                .unwrap()
                .count(),
            33
        );
        assert!(!root.path().join("HEAD.lock").exists());
        assert!(!root.path().join("packed-refs.lock").exists());
    }

    #[test]
    fn deletion_depth_failure_preserves_chain() {
        let root = tempfile::tempdir().unwrap();
        chain(root.path(), 33);
        let repo = Repository::open(root.path()).unwrap();
        assert!(matches!(
            repo.references()
                .unwrap()
                .delete_resolved_without_reflog(&RefName::new(b"HEAD").unwrap(), Expected::Any),
            Err(ReferenceError::Depth(32))
        ));
        assert_eq!(
            fs::read_dir(root.path().join("refs/heads"))
                .unwrap()
                .count(),
            33
        );
        assert!(!root.path().join("HEAD.lock").exists());
        assert!(!root.path().join("packed-refs.lock").exists());
    }

    #[test]
    fn locked_terminal_preserves_symbolic_chain() {
        let root = tempfile::tempdir().unwrap();
        chain(root.path(), 1);
        fs::write(root.path().join("refs/heads/1.lock"), b"owner").unwrap();
        let repo = Repository::open(root.path()).unwrap();
        let head = RefName::new(b"HEAD").unwrap();
        assert!(matches!(
            repo.references().unwrap().update_resolved_without_reflog(
                &head,
                ObjectId::from_bytes([1; 20]),
                Expected::Absent
            ),
            Err(ReferenceError::Locked(_))
        ));
        assert_eq!(
            fs::read(root.path().join("refs/heads/0")).unwrap(),
            b"ref: refs/heads/1\n"
        );
        assert_eq!(
            fs::read(root.path().join("refs/heads/1.lock")).unwrap(),
            b"owner"
        );
        assert!(!root.path().join("HEAD.lock").exists());
        assert!(!root.path().join("refs/heads/0.lock").exists());
        assert!(!root.path().join("packed-refs.lock").exists());
    }
}

#[cfg(all(test, unix))]
#[path = "delete_tests.rs"]
mod delete_tests;
