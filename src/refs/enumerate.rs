#![warn(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::unreachable,
    clippy::indexing_slicing,
    reason = "parses untrusted repository or network input (#103)"
)]
use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::store::{check_path, io_error, malformed, parse_loose};
use super::{RefName, ReferenceError, ReferenceObservation, References, Target};

/// A full reference name and its stored target, without symbolic resolution or tag peeling.
///
/// Use this owned result for display or subsequent conditional operations. Enumeration order is
/// stable in the returned vector, but filesystem traversal is not one repository-wide instant;
/// preserve an expected value when acting on it.
///
/// ```
/// use girt::refs::{Expected, RefName, Target};
/// use girt::{InitKind, ObjectFormat, Repository};
///
/// let directory = tempfile::tempdir()?;
/// let repo = Repository::init(
///     ObjectFormat::Sha1,
///     directory.path().join("r.git"),
///     InitKind::Bare,
/// )?;
///
/// let id = repo.loose_objects().write_blob(b"tagged manifest")?;
/// let refs = repo.references()?;
/// let name = RefName::new("refs/tags/one")?;
/// refs.update_without_reflog(&name, Target::Direct(id), Expected::Absent)?;
///
/// let listed = refs.list()?;
/// assert_eq!(listed[0].name, name);
/// assert_eq!(listed[0].target, Target::Direct(id));
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reference {
    /// Exact reference-name bytes; enumeration excludes pseudorefs such as HEAD.
    pub name: RefName,
    /// Effective backend value. Symbolic targets may be missing or cyclic.
    pub target: Target,
}

impl References<'_> {
    pub(crate) fn head_controlled(
        &self,
        max_bytes: usize,
        cancel: &AtomicBool,
    ) -> Result<Option<Target>, ReferenceError> {
        let name = RefName::HEAD;
        if self.reference_backend == super::Backend::Reftable {
            return super::reftable::backend::head_controlled(self, cancel);
        }
        let mut budget = ScanBudget {
            entries: 1,
            bytes: max_bytes,
            cancel,
        };
        let path = self.path(&name)?;
        read_limited(&path, &mut budget)?
            .map(|bytes| parse_loose(self.object_format, &bytes, &path))
            .transpose()
    }

    /// Lists all `refs/` names in bytewise name order, without resolving symbolic refs.
    ///
    /// Reftable merges bounded common/private stack snapshots using [`References`]'s contract.
    /// The loose/packed traversal details below describe the files backend.
    ///
    /// Includes branches, tags, other shared namespaces, and the current worktree's private
    /// refs. Excludes HEAD, other pseudorefs, other worktrees' private refs, dot-prefixed entries
    /// and `.lock` entries. Empty directories contribute no refs. Loose targets shadow packed
    /// targets, even if symbolic; a malformed loose target fails instead of falling back.
    ///
    /// Reads and validates packed-refs once, then walks loose files without locks. The owned
    /// result is stable after return but is not a point-in-time snapshot: concurrent creation or
    /// deletion may be included or missed, and values can come from different times. A loose
    /// file that disappears during traversal is ignored; a previously read packed value can
    /// remain in the result. Callers must use an expected value when acting on
    /// these observations. Memory use is proportional to packed data, discovered refs, and
    /// pending directories; there is no configurable resource limit.
    ///
    /// # Errors
    ///
    /// Reports I/O errors, invalid names or targets, conflicting effective names, filesystem
    /// symlinks and non-regular files, or malformed/unsupported packed data. Returns no partial
    /// list on error and does not modify storage.
    ///
    /// ```
    /// use girt::refs::{Expected, RefEdit, RefName, Reflog, Target};
    /// use girt::{Commit, CommitFields, InitKind, ObjectFormat, Repository, Signature, Tree};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let loose = repo.loose_objects();
    /// let person = Signature {
    ///     name: b"A. Writer".to_vec(),
    ///     email: b"a@example.com".to_vec(),
    ///     seconds: 1_700_000_000,
    ///     offset_minutes: 0,
    /// };
    /// let tree = loose.write_tree(&Tree::new(ObjectFormat::Sha1, vec![])?)?;
    /// let id = loose.write_commit(&Commit::new(CommitFields {
    ///     tree,
    ///     parents: vec![],
    ///     author: person.clone(),
    ///     committer: person.clone(),
    ///     extra_headers: vec![],
    ///     message: b"Initial snapshot\n".to_vec(),
    /// })?)?;
    ///
    /// let refs = repo.references()?;
    /// let main = RefName::new("refs/heads/main")?;
    /// let edit = RefEdit::new(
    ///     main.clone(),
    ///     Some(Target::Direct(id)),
    ///     Expected::Absent,
    ///     Reflog::Append {
    ///         committer: person,
    ///         message: b"commit (initial)".to_vec(),
    ///     },
    /// );
    /// refs.transaction(&[edit])?;
    ///
    /// let listed = refs.list()?;
    /// assert_eq!(listed.len(), 1); // HEAD is a pseudoref, excluded from list().
    /// assert_eq!(listed[0].name, main);
    /// assert_eq!(listed[0].target, Target::Direct(id));
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list(&self) -> Result<Vec<Reference>, ReferenceError> {
        self.enumerate(None)
    }

    /// Lists references with aggregate files-backend byte and traversal limits.
    ///
    /// The entry limit covers packed names and loose directory entries before they are retained;
    /// the byte limit covers packed-refs and loose file contents. Reftable uses this handle's
    /// [`super::reftable::StackLimits`] for encoded/decoded data and applies the entry limit to
    /// its bounded merged output. Cached packed snapshots still consume their retained byte and
    /// record counts. A busy packed cache is bypassed, so another reader's unbounded load cannot
    /// delay admission. Cancellation is checked between files and directory entries, and every
    /// batch of 64 packed lines. Individual filesystem reads remain synchronous bounded phases.
    /// A failed inventory returns no partial list and must not justify removing objects.
    ///
    /// # Errors
    ///
    /// Returns [`ReferenceError::Limit`] or [`ReferenceError::Cancelled`] at those boundaries,
    /// alongside the malformed, unsupported and I/O failures of [`Self::list`].
    ///
    /// ```
    /// use girt::refs::{Expected, RefName, Target};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"tagged manifest")?;
    /// let refs = repo.references()?;
    /// let name = RefName::new("refs/tags/one")?;
    /// refs.update_without_reflog(&name, Target::Direct(id), Expected::Absent)?;
    ///
    /// let cancel = std::sync::atomic::AtomicBool::new(false);
    /// assert!(matches!(
    ///     refs.list_controlled(0, 4096, &cancel),
    ///     Err(girt::refs::ReferenceError::Limit(_))
    /// ));
    /// assert_eq!(refs.list_controlled(100, 4096, &cancel)?.len(), 1);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_controlled(
        &self,
        max_entries: usize,
        max_bytes: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<Reference>, ReferenceError> {
        let mut budget = ScanBudget {
            entries: max_entries,
            bytes: max_bytes,
            cancel,
        };
        self.enumerate_with_budget(None, Some(&mut budget))
    }

    /// Lists stored targets with their captured peeled hints under inventory limits.
    ///
    /// Ordering, namespace selection, loose shadowing and live-read guarantees match
    /// [`Self::list`]; limits and cancellation match [`Self::list_controlled`]. Each hint comes
    /// from the same effective record as its target, without a per-name reread. A loose target
    /// replaces the complete packed observation and clears its hint, even when IDs are equal.
    /// Hints do not establish object existence or type; see [`ReferenceObservation`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::list_controlled`], without a partial inventory.
    ///
    /// ```
    /// use std::sync::atomic::AtomicBool;
    ///
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("repo"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let refs = repo.references()?;
    /// let observations =
    ///     refs.list_observations_controlled(100, 1024 * 1024, &AtomicBool::new(false))?;
    /// assert!(observations.is_empty()); // HEAD is not a refs/ inventory entry.
    ///
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_observations_controlled(
        &self,
        max_entries: usize,
        max_bytes: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<ReferenceObservation>, ReferenceError> {
        let mut budget = ScanBudget {
            entries: max_entries,
            bytes: max_bytes,
            cancel,
        };
        self.enumerate_observations(None, Some(&mut budget))
    }

    /// Lists a full name and its descendants, using `/` as the namespace boundary.
    ///
    /// Use `refs/heads` for branches and `refs/tags` for tags. `refs/heads/topic` includes that
    /// exact ref or its descendants, but not `refs/heads/topic-two`. Missing namespaces return
    /// an empty list. Only selected loose paths are read; packed-refs is always fully validated.
    /// Ordering, symbolic behavior, private namespaces and live-read guarantees match
    /// [`Self::list`].
    ///
    /// # Errors
    ///
    /// Reports errors from [`Self::list`] within the selected loose namespace or anywhere in
    /// packed-refs. HEAD is unsupported here; use [`Self::read`] for pseudorefs.
    ///
    /// ```
    /// use girt::refs::{Expected, RefName, Target};
    /// use girt::{InitKind, ObjectId, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     girt::ObjectFormat::Sha1,
    ///     directory.path().join("repo"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let refs = repo.references()?;
    /// let tag = RefName::new(b"refs/tags/example")?;
    /// let value = Target::Direct(ObjectId::Sha1([1; 20]));
    /// // Reference storage does not require the target object to exist.
    /// refs.update_without_reflog(&tag, value.clone(), Expected::Absent)?;
    ///
    /// let tags = refs.list_namespace(&RefName::new(b"refs/tags")?)?;
    /// assert_eq!(tags[0].name, tag);
    ///
    /// refs.delete_without_reflog(&tag, Expected::Value(tags[0].target.clone()))?;
    /// assert!(refs.list()?.is_empty());
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_namespace(&self, namespace: &RefName) -> Result<Vec<Reference>, ReferenceError> {
        if namespace.as_bytes() == b"HEAD" {
            return Err(ReferenceError::Unsupported("enumerating pseudorefs"));
        }
        self.enumerate(Some(namespace))
    }

    /// Lists a namespace like [`Self::list_namespace`], keeping packed peeled hints.
    ///
    /// A hint is the peeled target recorded in packed-refs for an annotated tag, so callers can
    /// avoid reading the tag object. Hints are unverified; loose references have none.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::list_namespace`].
    ///
    /// ```
    /// use girt::refs::{Expected, RefName, Target};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"tagged manifest")?;
    /// let refs = repo.references()?;
    /// let name = RefName::new("refs/tags/one")?;
    /// refs.update_without_reflog(&name, Target::Direct(id), Expected::Absent)?;
    ///
    /// let tags = refs.list_namespace_observations(&RefName::new("refs/tags")?)?;
    /// assert_eq!(tags.len(), 1);
    /// assert_eq!(tags[0].name, name);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_namespace_observations(
        &self,
        namespace: &RefName,
    ) -> Result<Vec<ReferenceObservation>, ReferenceError> {
        if namespace.as_bytes() == b"HEAD" {
            return Err(ReferenceError::Unsupported("enumerating pseudorefs"));
        }
        self.enumerate_observations(Some(namespace), None)
    }

    /// Lists references whose full names start with the bytes of `prefix`.
    ///
    /// Unlike [`Self::list_namespace`], the prefix need not end at a `/`: `refs/heads/fe` selects
    /// `refs/heads/feature` and `refs/heads/fe/x`, and `refs/heads/` selects every branch. Loose
    /// reads are confined to the namespace up to the last `/` in `prefix`; a prefix without one
    /// below `refs/` enumerates every reference before filtering. HEAD is never listed. Ordering,
    /// symbolic behavior and live-read guarantees match [`Self::list`].
    ///
    /// # Errors
    ///
    /// Reports the errors of [`Self::list_namespace`] for the enumerated namespace.
    ///
    /// ```
    /// use girt::refs::{RefEdit, RefName, Reflog};
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// let id = repo.loose_objects().write_blob(b"tip")?;
    /// let refs = repo.references()?;
    /// let edits = ["feature", "fe/x", "main"].map(|branch| {
    ///     Ok(RefEdit::create(
    ///         RefName::branch(branch)?,
    ///         id,
    ///         Reflog::Preserve,
    ///     ))
    /// });
    /// refs.transaction(
    ///     &edits
    ///         .into_iter()
    ///         .collect::<Result<Vec<_>, girt::refs::InvalidRefName>>()?,
    /// )?;
    ///
    /// let names: Vec<_> = refs
    ///     .list_prefix("refs/heads/fe")?
    ///     .into_iter()
    ///     .map(|r| r.name)
    ///     .collect();
    /// assert_eq!(
    ///     names,
    ///     [RefName::branch("fe/x")?, RefName::branch("feature")?]
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_prefix(&self, prefix: impl AsRef<[u8]>) -> Result<Vec<Reference>, ReferenceError> {
        Ok(self
            .list_prefix_observations(prefix)?
            .into_iter()
            .map(|observation| Reference {
                name: observation.name,
                target: observation.target,
            })
            .collect())
    }

    /// Lists references by byte prefix like [`Self::list_prefix`], keeping packed peeled hints.
    ///
    /// Hints follow [`Self::list_namespace_observations`].
    ///
    /// # Errors
    ///
    /// Returns the errors of [`Self::list_prefix`].
    ///
    /// ```
    /// use girt::{InitKind, ObjectFormat, Repository};
    ///
    /// let directory = tempfile::tempdir()?;
    /// let repo = Repository::init(
    ///     ObjectFormat::Sha1,
    ///     directory.path().join("r.git"),
    ///     InitKind::Bare,
    /// )?;
    ///
    /// assert!(
    ///     repo.references()?
    ///         .list_prefix_observations("refs/tags/v")?
    ///         .is_empty()
    /// );
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn list_prefix_observations(
        &self,
        prefix: impl AsRef<[u8]>,
    ) -> Result<Vec<ReferenceObservation>, ReferenceError> {
        let prefix = prefix.as_ref();
        // The directory part of the prefix bounds the loose scan; the rest is a byte filter.
        let namespace = prefix
            .iter()
            .rposition(|byte| *byte == b'/')
            .and_then(|end| prefix.get(..end))
            .and_then(|directory| RefName::new(directory).ok())
            .filter(|namespace| *namespace != RefName::HEAD);
        let mut observations = self.enumerate_observations(namespace.as_ref(), None)?;
        observations.retain(|observation| observation.name.as_bytes().starts_with(prefix));
        Ok(observations)
    }

    fn enumerate(&self, namespace: Option<&RefName>) -> Result<Vec<Reference>, ReferenceError> {
        self.enumerate_with_budget(namespace, None)
    }

    fn enumerate_with_budget(
        &self,
        namespace: Option<&RefName>,
        budget: Option<&mut ScanBudget<'_>>,
    ) -> Result<Vec<Reference>, ReferenceError> {
        Ok(self
            .enumerate_observations(namespace, budget)?
            .into_iter()
            .map(|observation| Reference {
                name: observation.name,
                target: observation.target,
            })
            .collect())
    }

    fn enumerate_observations(
        &self,
        namespace: Option<&RefName>,
        mut budget: Option<&mut ScanBudget<'_>>,
    ) -> Result<Vec<ReferenceObservation>, ReferenceError> {
        if let Some(budget) = budget.as_deref_mut() {
            budget.check()?;
        }
        if self.reference_backend == super::Backend::Reftable {
            let result = super::reftable::backend::list_observations(self, namespace)?;
            if let Some(budget) = budget.as_deref_mut() {
                budget.check()?;
                if result.len() > budget.entries {
                    return Err(ReferenceError::Limit("reference entries"));
                }
            }
            return Ok(result);
        }
        let packed = if let Some(budget) = budget.as_deref_mut() {
            let path = self.common_dir.join("packed-refs");
            let fallback = super::packed::PackedCache::default();
            let cache = self.packed_cache.unwrap_or(&fallback);
            let snapshot = cache.load_controlled(
                self.object_format,
                &path,
                budget.bytes,
                budget.entries,
                budget.cancel,
            )?;
            budget.bytes -= snapshot.bytes.len();
            snapshot
        } else {
            self.packed()?
        };
        if let Some(budget) = budget.as_deref_mut() {
            budget.charge(packed.len())?;
        }
        let mut entries: BTreeMap<_, _> = packed
            .iter()
            .filter(|(name, _)| selected(name.as_bytes(), namespace))
            .map(|(name, record)| {
                (
                    name.clone(),
                    ReferenceObservation {
                        name: name.clone(),
                        target: Target::Direct(record.target),
                        peeled_hint: record.peeled_hint,
                    },
                )
            })
            .collect();
        let separate = self.git_dir != self.common_dir;
        collect_loose(
            self.object_format,
            self.common_dir,
            namespace,
            if separate {
                LooseScope::Shared
            } else {
                LooseScope::All
            },
            &mut entries,
            budget.as_deref_mut(),
        )?;
        if separate {
            collect_loose(
                self.object_format,
                self.git_dir,
                namespace,
                LooseScope::Private,
                &mut entries,
                budget,
            )?;
        }
        // Prefix conflicts are not necessarily adjacent in byte order (a, a.b, a/c).
        for name in entries.keys() {
            for (index, byte) in name.as_bytes().iter().enumerate().skip(5) {
                if *byte == b'/'
                    && name
                        .as_bytes()
                        .get(..index)
                        .and_then(|parent| RefName::new(parent).ok())
                        .is_some_and(|parent| entries.contains_key(&parent))
                {
                    return Err(ReferenceError::Conflict(self.path(name)?));
                }
            }
        }
        Ok(entries.into_values().collect())
    }
}

fn selected(name: &[u8], namespace: Option<&RefName>) -> bool {
    namespace.is_none_or(|namespace| within(name, namespace.as_bytes()))
}

fn within(name: &[u8], namespace: &[u8]) -> bool {
    name == namespace
        || name
            .strip_prefix(namespace)
            .is_some_and(|rest| rest.starts_with(b"/"))
}

fn private(name: &[u8]) -> bool {
    let namespaces = [
        b"refs/bisect".as_slice(),
        b"refs/rewritten",
        b"refs/worktree",
    ];
    namespaces.iter().any(|prefix| within(name, prefix))
}

#[derive(Clone, Copy)]
enum LooseScope {
    All,
    Shared,
    Private,
}

#[cfg(test)]
thread_local! {
    /// Repository roots whose loose references were scanned on this thread, for listing-count
    /// regression tests.
    pub(crate) static LOOSE_SCANS: std::cell::RefCell<Vec<std::path::PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn collect_loose(
    format: crate::ObjectFormat,
    root: &Path,
    namespace: Option<&RefName>,
    scope: LooseScope,
    entries: &mut BTreeMap<RefName, ReferenceObservation>,
    mut budget: Option<&mut ScanBudget<'_>>,
) -> Result<(), ReferenceError> {
    #[cfg(test)]
    LOOSE_SCANS.with(|scans| scans.borrow_mut().push(root.to_owned()));
    let mut pending = vec![(root.join("refs"), b"refs".to_vec(), None::<fs::FileType>)];
    while let Some((path, bytes, file_type)) = pending.pop() {
        if let Some(budget) = budget.as_deref_mut() {
            budget.check()?;
        }
        // Recheck directories before descending, retaining the trusted-directory contract.
        // Leaves use the directory-provided type; their no-follow open checks the current file.
        let file_type = match file_type {
            Some(file_type) if !file_type.is_dir() => file_type,
            _ => match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata.file_type(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(io_error(&path, source)),
            },
        };
        let name = if file_type.is_dir() || file_type.is_symlink() {
            None
        } else {
            RefName::new(&bytes).ok()
        };
        let is_private = if file_type.is_dir() || file_type.is_symlink() {
            private(&bytes)
        } else {
            name.as_ref().is_some_and(RefName::per_worktree)
        };
        let excluded = match scope {
            LooseScope::All => false,
            LooseScope::Shared => is_private,
            LooseScope::Private => !is_private,
        };
        if bytes != b"refs" && excluded {
            continue;
        }
        if file_type.is_symlink() {
            return Err(ReferenceError::Unsupported(
                "filesystem symlink in reference path",
            ));
        }
        if file_type.is_dir() {
            let children = match fs::read_dir(&path) {
                Ok(children) => children,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                Err(source) => return Err(io_error(&path, source)),
            };
            for child in children {
                if let Some(budget) = budget.as_deref_mut() {
                    budget.check()?;
                    budget.charge(1)?;
                }
                let child = child.map_err(|source| io_error(&path, source))?;
                let component = child.file_name();
                // Preserve Unix filename bytes; other hosts require Unicode filesystem names.
                #[cfg(unix)]
                let component = {
                    use std::os::unix::ffi::OsStrExt;
                    component.as_bytes()
                };
                #[cfg(not(unix))]
                let component = component
                    .to_str()
                    .ok_or(ReferenceError::Unsupported("non-UTF-8 filesystem name"))?
                    .as_bytes();
                if component.starts_with(b".") || component.ends_with(b".lock") {
                    continue;
                }
                let mut name = bytes.clone();
                name.push(b'/');
                name.extend_from_slice(component);
                if namespace.is_some_and(|ns| {
                    !within(&name, ns.as_bytes()) && !within(ns.as_bytes(), &name)
                }) {
                    continue;
                }
                let file_type = match child.file_type() {
                    Ok(file_type) => file_type,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(source) => return Err(io_error(&child.path(), source)),
                };
                pending.push((child.path(), name, Some(file_type)));
            }
        } else if selected(&bytes, namespace) {
            let name = name.ok_or_else(|| malformed(&path, "invalid loose reference name"))?;
            // The walk checked the directories; the no-follow leaf open checks its current type.
            if !file_type.is_file() {
                return Err(ReferenceError::Unsupported("non-regular reference file"));
            }
            let value = if let Some(budget) = budget.as_deref_mut() {
                budget.check()?;
                read_limited_walked(&path, budget)?
            } else {
                read_walked(&path)?
            };
            if let Some(bytes) = value {
                let target = parse_loose(format, &bytes, &path)?;
                entries.insert(
                    name.clone(),
                    ReferenceObservation {
                        name,
                        target,
                        peeled_hint: None,
                    },
                );
            }
        }
    }
    Ok(())
}

/// Reads a loose reference file whose path the enumeration walk has already checked.
fn read_walked(path: &Path) -> Result<Option<Vec<u8>>, ReferenceError> {
    let Some(file) = open_walked(path)? else {
        return Ok(None);
    };
    let mut bytes = Vec::new();
    (&file)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    Ok(Some(bytes))
}

/// Opens a reference file the enumeration walk found below directories it already checked.
///
/// The walk's metadata may be stale by the time the leaf is opened, so the open itself refuses a
/// leaf replaced by a symlink and never blocks on a special file, and the opened handle must be a
/// regular file. Hosts without such an open fall back to checking every ancestor first. This keeps
/// the trusted-directory contract of [`check_path`]; it is not a sandbox against ancestor swaps.
pub(super) fn open_walked(path: &Path) -> Result<Option<fs::File>, ReferenceError> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;
        let flags = rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(flags.bits() as i32)
            .open(path)
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let opened = {
        check_path(path)?;
        fs::File::open(path)
    };
    let file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        Err(error) if error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
            return Err(ReferenceError::Unsupported(
                "filesystem symlink in reference path",
            ));
        }
        Err(source) => return Err(io_error(path, source)),
    };
    let metadata = file.metadata().map_err(|source| io_error(path, source))?;
    // Match check_path on hosts whose leaf open cannot open a directory.
    if metadata.is_dir() {
        return Err(ReferenceError::Conflict(path.into()));
    }
    if !metadata.is_file() {
        return Err(ReferenceError::Unsupported("non-regular reference file"));
    }
    Ok(Some(file))
}

struct ScanBudget<'a> {
    entries: usize,
    bytes: usize,
    cancel: &'a AtomicBool,
}

impl ScanBudget<'_> {
    fn check(&self) -> Result<(), ReferenceError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(ReferenceError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn charge(&mut self, entries: usize) -> Result<(), ReferenceError> {
        self.entries = self
            .entries
            .checked_sub(entries)
            .ok_or(ReferenceError::Limit("reference entries"))?;
        Ok(())
    }
}

fn read_limited(
    path: &Path,
    budget: &mut ScanBudget<'_>,
) -> Result<Option<Vec<u8>>, ReferenceError> {
    budget.check()?;
    check_path(path)?;
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    read_bounded(file, path, budget)
}

/// Reads like [`read_limited`] a loose reference whose path the enumeration walk already checked.
fn read_limited_walked(
    path: &Path,
    budget: &mut ScanBudget<'_>,
) -> Result<Option<Vec<u8>>, ReferenceError> {
    let Some(file) = open_walked(path)? else {
        return Ok(None);
    };
    read_bounded(file, path, budget)
}

fn read_bounded(
    file: impl Read,
    path: &Path,
    budget: &mut ScanBudget<'_>,
) -> Result<Option<Vec<u8>>, ReferenceError> {
    let mut bytes = Vec::new();
    file.take(budget.bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| io_error(path, source))?;
    if bytes.len() > budget.bytes {
        return Err(ReferenceError::Limit("reference bytes"));
    }
    budget.bytes -= bytes.len();
    budget.check()?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;
    use crate::{InitKind, ObjectId, Repository};

    fn fixture() -> (tempfile::TempDir, Repository) {
        let root = tempfile::tempdir().unwrap();
        let repo = Repository::init(
            crate::ObjectFormat::Sha1,
            root.path().join("repo"),
            InitKind::Bare,
        )
        .unwrap();
        fs::create_dir_all(repo.git_dir().join("refs/heads")).unwrap();
        fs::write(
            repo.git_dir().join("refs/heads/a"),
            b"ref: refs/heads/missing\n",
        )
        .unwrap();
        fs::write(repo.git_dir().join("packed-refs"), b"1111111111111111111111111111111111111111 refs/heads/a\n2222222222222222222222222222222222222222 refs/tags/z\n").unwrap();
        (root, repo)
    }

    #[test]
    fn lists_shadowing_symbols_and_packed_tags_in_byte_order() {
        let (_root, repo) = fixture();
        let entries = repo.references().unwrap().list().unwrap();
        assert_eq!(
            entries,
            vec![
                Reference {
                    name: RefName::new(b"refs/heads/a").unwrap(),
                    target: Target::Symbolic(RefName::new(b"refs/heads/missing").unwrap())
                },
                Reference {
                    name: RefName::new(b"refs/tags/z").unwrap(),
                    target: Target::Direct(ObjectId::Sha1([0x22; 20]))
                },
            ]
        );
    }

    #[test]
    fn controlled_inventory_rejects_limits_and_cancellation() {
        let (_root, repo) = fixture();
        let refs = repo.references().unwrap();
        let active = AtomicBool::new(false);
        assert_eq!(
            refs.list_controlled(10, 1024, &active).unwrap(),
            refs.list().unwrap()
        );
        assert!(matches!(
            refs.list_controlled(0, 1024, &active),
            Err(ReferenceError::Limit("reference entries"))
        ));
        assert!(matches!(
            refs.list_controlled(10, 0, &active),
            Err(ReferenceError::Limit("reference bytes"))
        ));
        let cancelled = AtomicBool::new(true);
        assert!(matches!(
            refs.list_controlled(10, 1024, &cancelled),
            Err(ReferenceError::Cancelled)
        ));
    }

    #[rstest]
    #[case::branch_namespace(b"refs/heads", 1)]
    #[case::exact(b"refs/heads/a", 1)]
    #[case::not_a_string_prefix(b"refs/head", 0)]
    #[case::missing(b"refs/other", 0)]
    fn filters_at_namespace_boundaries(#[case] namespace: &[u8], #[case] count: usize) {
        let (_root, repo) = fixture();
        assert_eq!(
            repo.references()
                .unwrap()
                .list_namespace(&RefName::new(namespace).unwrap())
                .unwrap()
                .len(),
            count
        );
    }

    #[test]
    fn skips_locks_dot_entries_and_empty_directories() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("refs/heads/a.lock"), b"writer").unwrap();
        fs::write(repo.git_dir().join("refs/heads/.hidden"), b"unrelated").unwrap();
        fs::create_dir(repo.git_dir().join("refs/heads/empty")).unwrap();
        assert_eq!(repo.references().unwrap().list().unwrap().len(), 2);
    }

    #[test]
    fn broken_loose_does_not_fall_back_but_unselected_loose_is_not_read() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("refs/heads/a"), b"broken").unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Malformed { .. })
        ));
        assert_eq!(
            repo.references()
                .unwrap()
                .list_namespace(&RefName::new(b"refs/tags").unwrap())
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            fs::read(repo.git_dir().join("refs/heads/a")).unwrap(),
            b"broken"
        );
    }

    #[test]
    fn filtered_list_still_validates_all_packed_data() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("packed-refs"), b"broken").unwrap();
        assert!(matches!(
            repo.references()
                .unwrap()
                .list_namespace(&RefName::new(b"refs/heads").unwrap()),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    /// Listing walks `refs/` once, checking each directory and file as it goes. It must not walk
    /// every ancestor again for each loose reference. Hosts without a no-follow open keep the
    /// per-reference check by design.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn listing_does_not_recheck_ancestors_per_reference() {
        let (_root, repo) = fixture();
        let id = "1111111111111111111111111111111111111111";
        for n in 0..50 {
            let dir = repo.git_dir().join(format!("refs/tags/group{}", n % 5));
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(format!("t{n}")), format!("{id}\n")).unwrap();
        }
        let refs = repo.references().unwrap();
        crate::refs::store::PATH_CHECKS.with(|checks| checks.set(0));
        let listed = refs.list().unwrap();
        assert!(listed.len() >= 50);
        // Only the shared `packed-refs` file is checked, once, however many loose references exist.
        assert_eq!(
            crate::refs::store::PATH_CHECKS.with(std::cell::Cell::get),
            1
        );
    }

    /// The walk saw a regular file, but by the time the leaf opens it has been replaced. A symlink
    /// is refused without reading its target, and a FIFO is refused without blocking.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn walked_open_refuses_replaced_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::write(&target, b"1111111111111111111111111111111111111111\n").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            read_walked(&link),
            Err(ReferenceError::Unsupported(_))
        ));
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        assert!(matches!(
            read_walked(&fifo),
            Err(ReferenceError::Unsupported(_))
        ));
        let mut budget = ScanBudget {
            entries: 1,
            bytes: 1000,
            cancel: &std::sync::atomic::AtomicBool::new(false),
        };
        assert!(matches!(
            read_limited_walked(&link, &mut budget),
            Err(ReferenceError::Unsupported(_))
        ));
        assert_eq!(read_walked(&dir.path().join("gone")).unwrap(), None);
    }

    #[test]
    fn walked_open_reports_file_replaced_by_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("replaced");
        fs::write(&path, b"ref: refs/heads/main\n").unwrap();
        assert!(fs::symlink_metadata(&path).unwrap().is_file());
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(
            matches!(read_walked(&path), Err(ReferenceError::Conflict(found)) if found == path)
        );
        let mut budget = ScanBudget {
            entries: 1,
            bytes: 1000,
            cancel: &std::sync::atomic::AtomicBool::new(false),
        };
        assert!(
            matches!(read_limited_walked(&path, &mut budget), Err(ReferenceError::Conflict(found)) if found == path)
        );
    }

    /// Reads continue to end of file however few bytes each read returns.
    #[test]
    fn bounded_read_collects_short_reads() {
        struct Trickle<'a>(&'a [u8]);
        impl Read for Trickle<'_> {
            fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
                let Some((&first, rest)) = self.0.split_first() else {
                    return Ok(0);
                };
                output[0] = first;
                self.0 = rest;
                Ok(1)
            }
        }
        let content = b"ref: refs/heads/main\n";
        let mut budget = ScanBudget {
            entries: 1,
            bytes: 1000,
            cancel: &std::sync::atomic::AtomicBool::new(false),
        };
        let read = read_bounded(Trickle(content), Path::new("ref"), &mut budget).unwrap();
        assert_eq!(read.unwrap(), content);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_reference_file_in_listing() {
        let (_root, repo) = fixture();
        let target = repo.git_dir().join("refs/heads/real");
        fs::write(&target, b"1111111111111111111111111111111111111111\n").unwrap();
        std::os::unix::fs::symlink(&target, repo.git_dir().join("refs/heads/alias")).unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Unsupported(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_directory_without_traversing_it() {
        let (_root, repo) = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.git_dir().join("refs/link")).unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Unsupported(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn private_namespace_symlink_is_rejected_in_its_own_worktree() {
        let (_root, repo) = fixture();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), repo.git_dir().join("refs/worktree")).unwrap();
        let mut entries = BTreeMap::new();
        assert!(matches!(
            collect_loose(
                repo.object_format(),
                repo.git_dir(),
                None,
                LooseScope::Private,
                &mut entries,
                None,
            ),
            Err(ReferenceError::Unsupported(_))
        ));
    }

    #[test]
    fn rejects_invalid_loose_name() {
        let (_root, repo) = fixture();
        fs::write(
            repo.git_dir().join("refs/heads/bad name"),
            b"1111111111111111111111111111111111111111\n",
        )
        .unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Malformed { .. })
        ));
    }

    #[test]
    fn rejects_nonadjacent_namespace_conflicts() {
        let (_root, repo) = fixture();
        fs::write(repo.git_dir().join("packed-refs"), b"1111111111111111111111111111111111111111 refs/a\n1111111111111111111111111111111111111111 refs/a.b\n1111111111111111111111111111111111111111 refs/a/c\n").unwrap();
        assert!(matches!(
            repo.references().unwrap().list(),
            Err(ReferenceError::Conflict(_))
        ));
    }

    #[test]
    fn preserves_packed_non_utf8_names() {
        let (_root, repo) = fixture();
        fs::write(
            repo.git_dir().join("packed-refs"),
            b"1111111111111111111111111111111111111111 refs/tags/\xff\n",
        )
        .unwrap();
        let tags = repo
            .references()
            .unwrap()
            .list_namespace(&RefName::new(b"refs/tags").unwrap())
            .unwrap();
        assert_eq!(tags[0].name.as_bytes(), b"refs/tags/\xff");
    }

    #[test]
    fn head_requires_single_name_read() {
        let (_root, repo) = fixture();
        assert!(matches!(
            repo.references().unwrap().list_namespace(&RefName::HEAD),
            Err(ReferenceError::Unsupported(_))
        ));
    }
}
