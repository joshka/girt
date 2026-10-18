use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::AtomicBool;

use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, openat};

use super::types::{check, refused};
use super::{Action, Applied, Collision, Error, Failure, Limits, Report, Stage};
use crate::status::worktree::{
    directory_flags, open_root, optional_stat, repository_marker, same, validate_path,
};
use crate::worktree_names::{NamePolicy, Protection};
use crate::{EntryMode, ObjectId, ObjectKind, Repository, TreeValue, index};

type Leaves = BTreeMap<Vec<u8>, TreeValue>;

/// The refusal reason for a file whose local changes a selective discard would overwrite.
pub(crate) const LOCAL_CHANGES: &str = "local changes would be overwritten";

/// A discarding checkout limited to some paths, as `git reset --keep` and `--merge` need.
pub(crate) struct Selection<'a> {
    /// The only paths read or written; every other file is left alone.
    pub(crate) paths: &'a BTreeSet<Vec<u8>>,
    /// Paths whose working file must still match its index entry, checked against the files
    /// this checkout observes; a modified one is refused with [`LOCAL_CHANGES`]. A missing file
    /// is restored.
    pub(crate) clean: &'a BTreeSet<Vec<u8>>,
    /// The index to publish, for the whole target.
    pub(crate) replacement: &'a [index::Entry],
}

/// What checkout does with local changes to tracked paths.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Local {
    /// Refuse staged, conflicted or modified tracked paths; the baseline must match exactly.
    Refuse,
    /// Discard them, as `git reset --hard` does: every path in the current index, at any stage,
    /// is replaced or removed whatever its content, missing tracked files are restored, an
    /// untracked file where the target needs a file or a directory is replaced, and directories
    /// left empty are removed. An untracked directory where the target needs a file is replaced
    /// only when it holds nothing but empty directories; Git deletes untracked contents too. The
    /// baseline tree is unused.
    Discard,
}
type Hook<'a> = dyn FnMut(&str, &[u8]) -> Result<(), Error> + 'a;

#[cfg(test)]
pub(super) fn run(
    repo: &Repository,
    baseline: Option<ObjectId>,
    target: Option<ObjectId>,
    limits: Limits,
    cancel: &AtomicBool,
    hook: &mut Hook<'_>,
) -> Result<Report, Failure> {
    run_observed(
        repo,
        baseline,
        target,
        None,
        limits,
        cancel,
        hook,
        &mut |_, _| {},
    )
}

#[expect(clippy::too_many_arguments, reason = "one checkout's inputs")]
pub(super) fn run_observed(
    repo: &Repository,
    baseline: Option<ObjectId>,
    target: Option<ObjectId>,
    filters: Option<Filters<'_>>,
    limits: Limits,
    cancel: &AtomicBool,
    hook: &mut Hook<'_>,
    progress: &mut impl FnMut(u64, u64),
) -> Result<Report, Failure> {
    run_inner(
        repo,
        [baseline, target],
        filters.is_none() || configured_symlinks(repo)?,
        None,
        filters,
        Local::Refuse,
        None,
        limits,
        cancel,
        hook,
        progress,
    )
}

pub(crate) fn run_with_index(
    repo: &Repository,
    baseline: Option<ObjectId>,
    target: ObjectId,
    entries: &[index::Entry],
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Report, Failure> {
    run_inner(
        repo,
        [baseline, Some(target)],
        true,
        Some(entries),
        None,
        Local::Refuse,
        None,
        limits,
        cancel,
        &mut |_, _| Ok(()),
        &mut |_, _| {},
    )
}

pub(super) fn run_configured(
    repo: &Repository,
    baseline: Option<ObjectId>,
    target: Option<ObjectId>,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Report, Failure> {
    let symlinks = configured_symlinks(repo)?;
    run_inner(
        repo,
        [baseline, target],
        symlinks,
        None,
        None,
        Local::Refuse,
        None,
        limits,
        cancel,
        &mut |_, _| Ok(()),
        &mut |_, _| {},
    )
}

/// Checks out `target` over whatever the index and tracked files hold, as `git reset --hard`
/// does; see [`Local::Discard`].
///
/// With a [`Selection`], files outside its paths are neither read nor written, untracked files
/// in the way are refused rather than replaced, and the index is set to its replacement.
pub(crate) fn run_discarding(
    repo: &Repository,
    target: Option<ObjectId>,
    filters: Option<&mut crate::filter::Pipeline>,
    selection: Option<&Selection<'_>>,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Report, Failure> {
    let symlinks = filters.is_none() || configured_symlinks(repo)?;
    let filters = filters.map(|target| Filters {
        baseline: None,
        target,
    });
    run_inner(
        repo,
        [None, target],
        symlinks,
        selection.map(|selection| selection.replacement),
        filters,
        Local::Discard,
        selection,
        limits,
        cancel,
        &mut |_, _| Ok(()),
        &mut |_, _| {},
    )
}

fn configured_symlinks(repo: &Repository) -> Result<bool, Failure> {
    repo.config()
        .boolean("core", None, "symlinks")
        .map_err(|_| Failure {
            cause: Box::new(refused(b"", "invalid core.symlinks boolean")),
            report: Report::default(),
            cleanup: vec![],
        })
        .map(|setting| setting.unwrap_or(true))
}

#[expect(
    clippy::too_many_arguments,
    reason = "one checkout's inputs and observers"
)]
fn run_inner(
    repo: &Repository,
    roots: [Option<ObjectId>; 2],
    symlinks: bool,
    replacement: Option<&[index::Entry]>,
    filters: Option<Filters<'_>>,
    local: Local,
    selection: Option<&Selection<'_>>,
    limits: Limits,
    cancel: &AtomicBool,
    hook: &mut Hook<'_>,
    progress: &mut impl FnMut(u64, u64),
) -> Result<Report, Failure> {
    let [baseline, target] = roots;
    let mut report = Report::default();
    let mut cleanup = Vec::new();
    let mut lock = None;
    let result: Result<(), Error> = (|| {
        check(cancel)?;
        let root = repo.worktree().ok_or_else(|| {
            refused(
                b"",
                if repo.is_bare() {
                    "bare repository"
                } else {
                    "worktree location is unknown"
                },
            )
        })?;
        let edit = repo.edit_index(limits.index)?;
        lock = Some(edit);
        let edit = lock.as_mut().unwrap();
        let mut plan = Plan::prepare(
            repo,
            root,
            [baseline, target],
            symlinks,
            edit,
            filters,
            local,
            selection,
            limits,
            cancel,
        )?;
        if let Some(entries) = replacement {
            edit.replace_entries(entries.to_vec())?;
        }
        report.unfiltered = std::mem::take(&mut plan.unfiltered);
        report.collisions = std::mem::take(&mut plan.collisions);
        hook("prepared", b"")?;
        check(cancel)?;
        let total = plan.operations.len() as u64;
        progress(0, total);
        report.stage = Stage::Worktree;
        for operation in std::mem::take(&mut plan.operations) {
            hook("before operation", &operation.path)?;
            check(cancel)?;
            plan.apply(&operation, cancel, hook, &mut cleanup, &mut report)?;
            progress(report.applied.len() as u64, total);
            hook("after operation", &report.applied.last().unwrap().path)?;
        }
        report.stage = Stage::Publication;
        hook("before publication", b"")?;
        plan.verify_target(cancel, hook)?;
        check(cancel)?;
        edit.publish()?;
        report.index_published = true;
        progress(total, total);
        Ok(())
    })();
    if let Err(mut cause) = result {
        if let Error::Index(index::StorageError::Cleanup {
            operation,
            cleanup: error,
        }) = cause
        {
            cause = Error::Index(*operation);
            cleanup.push(Error::Index(*error));
        }
        if let Some(edit) = lock
            && let Err(error) = edit.abort()
        {
            cleanup.push(error.into());
        }
        return Err(Failure {
            cause: Box::new(cause),
            report,
            cleanup,
        });
    }
    Ok(report)
}

/// The conversions for each side of a filtered checkout.
pub(super) struct Filters<'a> {
    /// `None` with [`Local::Discard`], which never compares old content.
    pub(super) baseline: Option<&'a mut crate::filter::Pipeline>,
    pub(super) target: &'a mut crate::filter::Pipeline,
}

/// Working-tree bytes by path and blob, for leaves whose converted form differs from the blob.
type Converted = BTreeMap<(Vec<u8>, ObjectId), Vec<u8>>;

/// Which tree a leaf belongs to.
#[derive(Clone, Copy, PartialEq)]
enum Side {
    Baseline = 0,
    Target = 1,
}

// One bounded plan owns verified payloads and the expected namespace after each operation.
// Directory identity is stable across our own child mutations; leaf metadata/content is not.
struct Plan<'a> {
    root: &'a Path,
    symlinks: bool,
    root_stat: Stat,
    metadata: Vec<Stat>,
    expected: BTreeMap<Vec<u8>, Option<Stat>>,
    old: Leaves,
    target: Leaves,
    blobs: BTreeMap<ObjectId, Vec<u8>>,
    /// Working-tree bytes of regular files whose filtered form differs from their blob.
    converted: [Converted; 2],
    unfiltered: Vec<Vec<u8>>,
    operations: Vec<Applied>,
    limits: Limits,
    // Verified descriptors for the most recently traversed directory, root first. Each entry was
    // opened without following links and matched its expected identity and guards when pushed.
    chain: RefCell<Vec<(Vec<u8>, Rc<OwnedFd>)>>,
    // Directory name checks for the current read-only phase, one enumeration per directory.
    // `None` while mutating: exclusive creation and exact-name identity checks guard each
    // operation, and publication enumerates every target directory again.
    listings: RefCell<Option<BTreeMap<Vec<u8>, Listing>>>,
    /// Target paths sharing one name on the volume; `old` and `target` hold one path per set.
    collisions: Vec<Collision>,
    policy: NamePolicy,
    local: Local,
    /// With [`Local::Discard`], old paths whose working file differs from the index, including
    /// untracked files in the target's way.
    dirty: BTreeSet<Vec<u8>>,
    /// With [`Local::Discard`], old paths with no working file.
    missing: BTreeSet<Vec<u8>>,
    /// With [`Local::Discard`], kept files holding the blob's raw bytes rather than the bytes the
    /// target's attributes would write; publication accepts them as they are.
    kept_raw: BTreeSet<Vec<u8>>,
    /// Whether a [`Selection`] limits this discard.
    selective: bool,
    /// A selective discard's paths that must still match their index entries.
    clean: BTreeSet<Vec<u8>>,
}

impl<'a> Plan<'a> {
    #[expect(clippy::too_many_arguments, reason = "one checkout's inputs")]
    fn prepare(
        repo: &Repository,
        root: &'a Path,
        roots: [Option<ObjectId>; 2],
        symlinks: bool,
        edit: &mut index::IndexEdit,
        filters: Option<Filters<'_>>,
        local: Local,
        selection: Option<&Selection<'_>>,
        mut limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let [baseline, target] = roots;
        let objects = repo.objects_with(limits.packs)?;
        let mut target = flatten(&objects, target, limits, cancel)?;
        let entries = edit.index().entries();
        let initial = baseline.is_none() && local == Local::Refuse && entries.is_empty();
        let mut old = match local {
            Local::Refuse => flatten(&objects, baseline, limits, cancel)?,
            Local::Discard => tracked(entries, cancel)?,
        };
        // A discarding checkout limited to some paths leaves every other file alone.
        if let Some(selection) = selection {
            old.retain(|path, _| selection.paths.contains(path));
            target.retain(|path, _| selection.paths.contains(path));
        }
        // Old content is read only where a selective discard must prove a file clean.
        let clean_old: Option<Leaves> = selection.map(|selection| {
            old.iter()
                .filter(|(path, _)| selection.clean.contains(*path))
                .map(|(path, leaf)| (path.clone(), *leaf))
                .collect()
        });
        if local == Local::Refuse && entries.len() != old.len() {
            return Err(refused(b"", "index differs from explicit baseline"));
        }
        for entry in entries.iter().filter(|_| local == Local::Refuse) {
            check(cancel)?;
            if entry.intent_to_add || entry.skip_worktree {
                return Err(refused(
                    &entry.path,
                    "extended index flags require caller policy",
                ));
            }
            if entry.stage != index::Stage::Normal || old.get(&entry.path) != Some(&value(entry)) {
                return Err(refused(&entry.path, "staged changes or conflict"));
            }
        }
        if edit.index().extensions().iter().any(|e| {
            !matches!(
                &e.signature(),
                b"TREE" | b"FSMN" | b"UNTR" | b"EOIE" | b"IEOT"
            )
        }) {
            return Err(refused(b"", "unsupported index extension"));
        }
        let protection = Protection::from_config(repo)
            .map_err(|_| refused(b"", "core.protectNTFS or core.protectHFS is not a boolean"))?;
        let policy = NamePolicy::from_config(repo).map_err(|_| {
            refused(
                b"",
                "core.ignoreCase or core.precomposeUnicode is not a boolean",
            )
        })?;
        for path in old.keys().chain(target.keys()) {
            validate_path(path)?;
            if protection.refuses(path) {
                return Err(refused(path, "repository metadata name alias"));
            }
            if path.split(|b| *b == b'/').any(|part| part.len() > 255) {
                return Err(refused(path, "path component exceeds 255 bytes"));
            }
            if path.split(|b| *b == b'/').count() > limits.max_depth.min(128) {
                return Err(Error::Limit("directory depth"));
            }
        }
        // The working tree holds what the volume can store: one path per set of names it
        // merges. The index keeps every target path.
        let collide =
            |path: Vec<u8>| refused(&path, "file and directory share one name on this volume");
        let old_resolved = policy.resolve(&old).map_err(collide)?;
        let resolved = policy.resolve(&target).map_err(collide)?;
        let collisions = resolved.collisions;
        for collision in &collisions {
            if std::iter::once(&collision.written)
                .chain(&collision.shadowed)
                .any(|path| {
                    target
                        .get(path)
                        .is_some_and(|leaf| leaf.mode == EntryMode::Gitlink)
                })
            {
                return Err(refused(
                    &collision.written,
                    "gitlink shares a name on this volume",
                ));
            }
        }
        let old_disk = old_resolved.disk;
        let target_disk = resolved.disk;
        let sources = [old_resolved.sources, resolved.sources];
        let mut paths = BTreeSet::new();
        let mut aliases = BTreeMap::new();
        for path in old_disk.keys().chain(target_disk.keys()) {
            for prefix in prefixes(path) {
                if paths.insert(prefix.to_vec()) {
                    charge(&mut limits.max_path_bytes, prefix.len(), "path bytes")?;
                    // A spelling change between baseline and target, such as a case-only
                    // rename, is refused rather than ordered through the shared name.
                    let folded: Vec<Vec<u8>> = prefix
                        .split(|b| *b == b'/')
                        .map(|part| policy.key(part))
                        .collect();
                    if let Some(previous) = aliases.insert(folded, prefix.to_vec())
                        && previous != prefix
                    {
                        return Err(refused(prefix, "paths share one name on this volume"));
                    }
                }
            }
        }
        let mut blobs = BTreeMap::new();
        // A full discard never compares old content, so old blobs, which may be absent (as for
        // intent-to-add entries), are not read.
        let compared = match local {
            Local::Refuse => Some(&old),
            Local::Discard => clean_old.as_ref(),
        };
        for leaf in compared
            .into_iter()
            .flat_map(Leaves::values)
            .chain(target.values())
        {
            check(cancel)?;
            if leaf.mode == EntryMode::Gitlink {
                if initial {
                    continue;
                }
                return Err(refused(b"", "gitlink transitions are unsupported"));
            }
            if blobs.contains_key(&leaf.id) {
                continue;
            }
            let mut read = limits.read;
            read.max_object_bytes = read
                .max_object_bytes
                .min(limits.max_object_bytes)
                .min(limits.max_file_bytes);
            let object = objects
                .read_with(leaf.id, read)?
                .ok_or(Error::InvalidBlob(leaf.id))?;
            if object.kind() != ObjectKind::Blob {
                return Err(Error::InvalidBlob(leaf.id));
            }
            charge(
                &mut limits.max_object_bytes,
                object.data().len(),
                "object bytes",
            )?;
            blobs.insert(leaf.id, object.data().to_vec());
        }
        for leaf in compared
            .into_iter()
            .flat_map(Leaves::values)
            .chain(target.values())
        {
            if leaf.mode == EntryMode::Gitlink {
                continue;
            }
            let bytes = &blobs[&leaf.id];
            if leaf.mode == EntryMode::Symlink
                && (bytes.is_empty() || bytes.contains(&0) || bytes.len() > 1024)
            {
                return Err(refused(
                    b"",
                    "symlink target must contain 1..=1024 non-NUL bytes",
                ));
            }
        }
        let mut converted = [BTreeMap::new(), BTreeMap::new()];
        let mut unfiltered = Vec::new();
        if let Some(filters) = filters {
            // A selective discard proves old files clean with the target's conversion, as it has
            // no baseline attributes.
            let selective_old = clean_old.as_ref().filter(|_| local == Local::Discard);
            let mut sides: Vec<(Side, &Leaves, &mut crate::filter::Pipeline)> = Vec::new();
            if let (Local::Refuse, Some(baseline)) = (local, filters.baseline) {
                sides.push((Side::Baseline, &old, baseline));
            }
            sides.push((Side::Target, &target, filters.target));
            for (side, leaves, pipeline) in sides {
                let mut pass = vec![(side, leaves)];
                if side == Side::Target
                    && let Some(clean_old) = selective_old
                {
                    pass.push((Side::Baseline, clean_old));
                }
                for (side, leaves) in pass {
                    for (path, leaf) in leaves {
                        check(cancel)?;
                        if matches!(leaf.mode, EntryMode::Symlink | EntryMode::Gitlink) {
                            continue;
                        }
                        let written = pipeline
                            .to_worktree(path, &blobs[&leaf.id], leaf.id)
                            .map_err(|source| Error::Filter {
                                path: path.clone(),
                                source: Box::new(source),
                            })?;
                        if (written.skipped.is_some() || written.unencoded) && side == Side::Target
                        {
                            unfiltered.push(path.clone());
                        }
                        if let std::borrow::Cow::Owned(bytes) = written.bytes {
                            if bytes.len() > limits.max_file_bytes {
                                return Err(Error::Limit("file bytes"));
                            }
                            converted[side as usize].insert((path.clone(), leaf.id), bytes);
                        }
                    }
                }
            }
        }
        // Filters and attributes see planned paths, as in Git; the plan writes disk paths.
        for (side, sources) in sources.iter().enumerate() {
            for (disk, planned) in sources {
                let leaves = if side == Side::Target as usize {
                    &target_disk
                } else {
                    &old_disk
                };
                let id = leaves[disk].id;
                if let Some(bytes) = converted[side].remove(&(planned.clone(), id)) {
                    converted[side].insert((disk.clone(), id), bytes);
                }
            }
        }
        let mut target_budget = limits.max_worktree_bytes;
        for (path, leaf) in &target_disk {
            if leaf.mode == EntryMode::Gitlink {
                continue;
            }
            let length = converted[Side::Target as usize]
                .get(&(path.clone(), leaf.id))
                .map_or(blobs[&leaf.id].len(), Vec::len);
            charge(&mut target_budget, length, "target verification bytes")?;
        }
        // Validate the complete index before any working-tree side effects.
        let replacement = target
            .iter()
            .map(|(path, leaf)| index::Entry::new(path.clone(), index_mode(leaf.mode), leaf.id))
            .collect();
        edit.replace_entries(replacement)?;
        edit.discard_checkout_caches();
        let fd = open_root(root)?;
        let root_stat = fstat(&fd).map_err(|e| io(b"", e))?;
        let metadata = [repo.git_dir(), repo.common_dir(), repo.object_dir()]
            .into_iter()
            .map(|path| rustix::fs::stat(path).map_err(|e| io(b"", e)))
            .collect::<Result<_, _>>()?;
        let mut plan = Self {
            root,
            symlinks,
            root_stat,
            metadata,
            expected: BTreeMap::new(),
            old: old_disk,
            target: target_disk,
            collisions,
            policy,
            blobs,
            converted,
            unfiltered,
            operations: vec![],
            limits,
            chain: RefCell::default(),
            listings: RefCell::new(Some(BTreeMap::new())),
            local,
            dirty: BTreeSet::new(),
            missing: BTreeSet::new(),
            kept_raw: BTreeSet::new(),
            selective: selection.is_some(),
            clean: selection
                .map(|selection| selection.clean.clone())
                .unwrap_or_default(),
        };
        for path in paths {
            check(cancel)?;
            let stat = plan.inspect_initial(&path, cancel)?;
            plan.expected.insert(path, stat);
        }
        if local == Local::Discard {
            plan.observe_discarded(cancel)?;
        } else {
            let mut budget = plan.limits.max_worktree_bytes;
            for (path, leaf) in &plan.old {
                let parent = plan.parent(path, cancel)?;
                plan.verify_leaf(Side::Baseline, &parent, path, *leaf, &mut budget, cancel)?;
            }
        }
        plan.build_operations(cancel)?;
        plan.check_planned_markers(cancel)?;
        plan.begin_phase(false);
        Ok(plan)
    }

    /// The bytes `leaf` has in the working tree at `path` on `side` of the checkout.
    fn content(&self, side: Side, path: &[u8], leaf: TreeValue) -> &[u8] {
        self.converted[side as usize]
            .get(&(path.to_vec(), leaf.id))
            .unwrap_or(&self.blobs[&leaf.id])
    }

    // Each phase reopens the root from `/`, so root substitution between phases is detected.
    fn begin_phase(&self, enumerate: bool) {
        self.chain.borrow_mut().clear();
        *self.listings.borrow_mut() = enumerate.then(BTreeMap::new);
    }

    fn inspect_initial(&mut self, path: &[u8], cancel: &AtomicBool) -> Result<Option<Stat>, Error> {
        let parent_path = parent_path(path);
        if !parent_path.is_empty() {
            match self.expected.get(parent_path).and_then(Option::as_ref) {
                None => return Ok(None),
                Some(stat) if kind(stat) != FileType::Directory => return Ok(None),
                _ => (),
            }
        }
        let parent = self.parent(path, cancel)?;
        self.check_name(&parent, path, cancel)?;
        let stat = optional_stat(&parent, name(path), path)?;
        if let Some(stat) = &stat
            && kind(stat) == FileType::Directory
        {
            let child = open_directory(&parent, name(path), path)?;
            self.guard_directory(&child, path)?;
            identity(path, stat, &fstat(&child).map_err(|e| io(path, e))?)?;
        }
        Ok(stat)
    }

    /// Records which old leaves are missing or modified instead of refusing them, and adopts
    /// untracked files where the target needs a file so they are replaced.
    fn observe_discarded(&mut self, cancel: &AtomicBool) -> Result<(), Error> {
        // Untracked files where the target needs a file or a directory are replaced, as Git
        // replaces them in a hard reset; their adopted value is never compared. A selective
        // discard refuses them instead, as Git's keep and merge resets do.
        let blocked: Vec<(Vec<u8>, TreeValue)> = if self.selective {
            Vec::new()
        } else {
            self.target
                .iter()
                .flat_map(|(path, leaf)| {
                    prefixes(path)
                        .map(|prefix| prefix.to_vec())
                        .map(move |prefix| (prefix, *leaf))
                })
                .filter(|(path, _)| !self.old.contains_key(path))
                .filter(|(path, _)| {
                    self.expected
                        .get(path)
                        .copied()
                        .flatten()
                        .is_some_and(|stat| {
                            matches!(kind(&stat), FileType::RegularFile | FileType::Symlink)
                        })
                })
                .collect()
        };
        for (path, leaf) in blocked {
            let value = TreeValue {
                id: leaf.id,
                mode: EntryMode::Blob,
            };
            self.old.entry(path.clone()).or_insert(value);
            self.dirty.insert(path);
        }
        let mut budget = self.limits.max_worktree_bytes;
        let old: Vec<_> = self.old.iter().map(|(p, l)| (p.clone(), *l)).collect();
        for (path, leaf) in old {
            check(cancel)?;
            let Some(stat) = self.expected[&path] else {
                self.missing.insert(path);
                continue;
            };
            match kind(&stat) {
                FileType::RegularFile | FileType::Symlink => {}
                _ => return Err(refused(&path, "tracked path is not a file or symlink")),
            }
            // This observation authorizes the overwrite: later steps recheck the same identity.
            if self.clean.contains(&path) {
                let parent = self.parent(&path, cancel)?;
                if self
                    .verify_leaf(Side::Baseline, &parent, &path, leaf, &mut budget, cancel)
                    .is_err()
                {
                    return Err(refused(&path, LOCAL_CHANGES));
                }
            }
            if self.dirty.contains(&path) || !self.target.contains_key(&path) {
                continue;
            }
            if self.target.get(&path) != Some(&leaf) {
                self.dirty.insert(path);
                continue;
            }
            // Only the target's blob is loaded; the index entry equals it here. A file holding the
            // blob's bytes, or the bytes the target's attributes would write, is clean: Git keeps a
            // clean file when only its attributes change.
            let parent = self.parent(&path, cancel)?;
            match self.read_tracked(&parent, &path, leaf, &mut budget, cancel) {
                Ok((mode, bytes)) if mode == leaf.mode => {
                    if bytes != self.content(Side::Target, &path, leaf) {
                        if bytes == self.blobs[&leaf.id] {
                            self.kept_raw.insert(path);
                        } else {
                            self.dirty.insert(path);
                        }
                    }
                }
                _ => {
                    self.dirty.insert(path);
                }
            }
        }
        Ok(())
    }

    fn build_operations(&mut self, cancel: &AtomicBool) -> Result<(), Error> {
        for (path, old) in &self.old {
            let changed = self.target.get(path) != Some(old) || self.dirty.contains(path);
            if changed && !self.missing.contains(path) {
                self.operations.push(op(path, Action::Remove));
            }
        }
        let old_dirs = directories(&self.old);
        let target_dirs = directories(&self.target);
        let mut remove_dirs = BTreeSet::new();
        for path in self.target.keys() {
            if self.target[path].mode == EntryMode::Gitlink {
                if self.expected[path]
                    .as_ref()
                    .is_some_and(|stat| kind(stat) == FileType::Directory)
                {
                    let parent = self.parent(path, cancel)?;
                    self.verify_empty_gitlink(&parent, path, cancel)?;
                }
                continue;
            }
            if self.old.contains_key(path) {
                continue;
            }
            match self.expected[path].as_ref() {
                None => (),
                Some(stat) if kind(stat) == FileType::Directory && old_dirs.contains(path) => {
                    self.check_removable(path, &old_dirs, &mut remove_dirs, cancel)?;
                }
                // Discarding replaces an untracked directory in the way only when it holds
                // nothing but empty directories; Git would delete untracked files too.
                Some(stat)
                    if kind(stat) == FileType::Directory
                        && self.local == Local::Discard
                        && !self.selective =>
                {
                    let mut empty = Vec::new();
                    if let Some(untracked) = self.untracked_content(path, &mut empty, cancel)? {
                        return Err(refused(
                            &untracked,
                            "untracked file in a directory where the target has a file",
                        ));
                    }
                    for (directory, stat) in empty {
                        self.expected.insert(directory.clone(), Some(stat));
                        remove_dirs.insert(directory);
                    }
                    remove_dirs.insert(path.clone());
                }
                Some(_) => return Err(refused(path, "untracked obstruction")),
            }
        }
        if self.local == Local::Discard {
            self.plan_emptied_directories(&old_dirs, &target_dirs, &mut remove_dirs, cancel)?;
        }
        let mut remove_dirs: Vec<_> = remove_dirs.into_iter().collect();
        remove_dirs.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
        self.operations.extend(
            remove_dirs
                .iter()
                .map(|path| op(path, Action::RemoveDirectory)),
        );
        for path in target_dirs {
            match self.expected[&path].as_ref() {
                Some(stat) if kind(stat) == FileType::Directory => (),
                Some(_) if !self.old.contains_key(&path) => {
                    return Err(refused(&path, "untracked ancestor obstruction"));
                }
                _ => self.operations.push(op(&path, Action::CreateDirectory)),
            }
        }
        for (path, leaf) in &self.target {
            if leaf.mode == EntryMode::Gitlink {
                continue;
            }
            if self.old.get(path) != Some(leaf)
                || self.dirty.contains(path)
                || self.missing.contains(path)
            {
                self.operations.push(op(path, Action::Install));
            }
        }
        Ok(())
    }

    // The live guard intentionally recognizes marker names even when they are ordinary files.
    // Project their presence through the actual operation order so our own output cannot activate
    // that guard after an earlier mutation. Counts retain distinct ASCII aliases on case-sensitive
    // filesystems; conservative folding also covers case-insensitive destinations.
    fn check_planned_markers(&self, cancel: &AtomicBool) -> Result<(), Error> {
        let mut directory_paths = directories(&self.old);
        directory_paths.extend(directories(&self.target));
        let mut markers = BTreeMap::new();
        for path in directory_paths {
            check(cancel)?;
            let mut counts = [0usize; 3];
            if self.expected[&path]
                .as_ref()
                .is_some_and(|stat| kind(stat) == FileType::Directory)
            {
                let parent = self.parent(&path, cancel)?;
                let fd = open_directory(&parent, name(&path), &path)?;
                self.guard_directory(&fd, &path)?;
                identity(
                    &path,
                    self.expected[&path].as_ref().unwrap(),
                    &fstat(&fd).map_err(|e| io(&path, e))?,
                )?;
                let mut budget = self.limits.max_directory_entries;
                for child in names(&fd, &path, &mut budget, cancel)? {
                    check(cancel)?;
                    if let Some(slot) = marker_slot(&child) {
                        counts[slot] += 1;
                    }
                }
            }
            reject_marker_set(&path, &counts)?;
            markers.insert(path, counts);
        }
        for operation in &self.operations {
            check(cancel)?;
            if let Some(slot) = marker_slot(name(&operation.path))
                && let Some(counts) = markers.get_mut(parent_path(&operation.path))
            {
                match operation.action {
                    Action::Remove | Action::RemoveDirectory => {
                        counts[slot] = counts[slot].checked_sub(1).ok_or_else(|| {
                            refused(&operation.path, "marker disappeared during preparation")
                        })?;
                    }
                    Action::CreateDirectory | Action::Install => counts[slot] += 1,
                }
                reject_marker_set(parent_path(&operation.path), counts)?;
            }
            if operation.action == Action::CreateDirectory {
                markers.insert(operation.path.clone(), [0; 3]);
            }
        }
        Ok(())
    }

    /// With [`Local::Discard`], removes directories that removing old files leaves empty, as
    /// `git reset --hard` does. A directory holding anything else, such as an untracked file,
    /// stays.
    fn plan_emptied_directories(
        &self,
        old_dirs: &BTreeSet<Vec<u8>>,
        target_dirs: &BTreeSet<Vec<u8>>,
        remove: &mut BTreeSet<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let gone: BTreeSet<&[u8]> = self
            .old
            .keys()
            .filter(|path| !self.target.contains_key(*path) && !self.missing.contains(*path))
            .map(Vec::as_slice)
            .collect();
        let candidate = |dir: &[u8]| {
            !target_dirs.contains(dir)
                && !self.target.contains_key(dir)
                && self
                    .expected
                    .get(dir)
                    .copied()
                    .flatten()
                    .is_some_and(|stat| kind(&stat) == FileType::Directory)
        };
        for dir in old_dirs {
            check(cancel)?;
            let parent = parent_path(dir);
            if !candidate(dir) || (!parent.is_empty() && candidate(parent)) {
                continue;
            }
            self.emptied(dir, &gone, old_dirs, remove, cancel)?;
        }
        Ok(())
    }

    /// Whether removing `gone` leaves `dir` empty; every directory it empties is added to
    /// `remove`, even when `dir` itself keeps other content.
    fn emptied(
        &self,
        dir: &[u8],
        gone: &BTreeSet<&[u8]>,
        old_dirs: &BTreeSet<Vec<u8>>,
        remove: &mut BTreeSet<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> Result<bool, Error> {
        let parent = self.parent(dir, cancel)?;
        let fd = open_directory(&parent, name(dir), dir)?;
        self.guard_directory(&fd, dir)?;
        let mut budget = self.limits.max_directory_entries;
        let mut empty = true;
        for child in names(&fd, dir, &mut budget, cancel)? {
            let child_path = joined(dir, &child);
            let child_empty = if old_dirs.contains(&child_path) {
                self.emptied(&child_path, gone, old_dirs, remove, cancel)?
            } else {
                gone.contains(child_path.as_slice())
            };
            empty &= child_empty;
        }
        if empty {
            remove.insert(dir.to_vec());
        }
        Ok(empty)
    }

    /// The first entry under the untracked directory `dir` that is not a directory, or `None`
    /// when it holds only empty directories, which are then added to `empty` with their
    /// identities so their removal is checked like any planned path.
    fn untracked_content(
        &self,
        dir: &[u8],
        empty: &mut Vec<(Vec<u8>, Stat)>,
        cancel: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, Error> {
        let parent = self.parent(dir, cancel)?;
        let fd = open_directory(&parent, name(dir), dir)?;
        self.untracked_content_in(&fd, dir, empty, cancel)
    }

    fn untracked_content_in(
        &self,
        fd: &OwnedFd,
        dir: &[u8],
        empty: &mut Vec<(Vec<u8>, Stat)>,
        cancel: &AtomicBool,
    ) -> Result<Option<Vec<u8>>, Error> {
        self.guard_directory(fd, dir)?;
        let mut budget = self.limits.max_directory_entries;
        for child in names(fd, dir, &mut budget, cancel)? {
            check(cancel)?;
            let child_path = joined(dir, &child);
            let stat = optional_stat(fd, &child, &child_path)?;
            let Some(stat) = stat.filter(|stat| kind(stat) == FileType::Directory) else {
                return Ok(Some(child_path));
            };
            let child_fd = open_directory(fd, &child, &child_path)?;
            identity(
                &child_path,
                &stat,
                &fstat(&child_fd).map_err(|e| io(&child_path, e))?,
            )?;
            if let Some(found) = self.untracked_content_in(&child_fd, &child_path, empty, cancel)? {
                return Ok(Some(found));
            }
            empty.push((child_path, stat));
        }
        Ok(None)
    }

    fn check_removable(
        &self,
        path: &[u8],
        old_dirs: &BTreeSet<Vec<u8>>,
        remove: &mut BTreeSet<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let parent = self.parent(path, cancel)?;
        let fd = open_directory(&parent, name(path), path)?;
        self.guard_directory(&fd, path)?;
        let mut budget = self.limits.max_directory_entries;
        for child in names(&fd, path, &mut budget, cancel)? {
            let child_path = joined(path, &child);
            if old_dirs.contains(&child_path) {
                self.check_removable(&child_path, old_dirs, remove, cancel)?;
            } else if !self.old.contains_key(&child_path) {
                return Err(refused(
                    &child_path,
                    "untracked content prevents directory transition",
                ));
            }
        }
        remove.insert(path.to_vec());
        Ok(())
    }

    fn root_fd(&self) -> Result<OwnedFd, Error> {
        let fd = open_root(self.root)?;
        identity(b"", &self.root_stat, &fstat(&fd).map_err(|e| io(b"", e))?)?;
        self.guard_directory(&fd, b"")?;
        Ok(fd)
    }

    // Reuses the verified descriptors shared with the previous path and opens the rest.
    fn parent(&self, path: &[u8], cancel: &AtomicBool) -> Result<Rc<OwnedFd>, Error> {
        let directory = parent_path(path);
        let mut chain = self.chain.borrow_mut();
        let shared = chain
            .iter()
            .take_while(|(prefix, _)| within(directory, prefix))
            .count();
        chain.truncate(shared);
        if chain.is_empty() {
            chain.push((Vec::new(), Rc::new(self.root_fd()?)));
        }
        for prefix in prefixes(directory).skip(chain.len() - 1) {
            check(cancel)?;
            let fd = Rc::clone(&chain.last().unwrap().1);
            self.check_name(&fd, prefix, cancel)?;
            let child = self.open_expected(&fd, prefix)?;
            chain.push((prefix.to_vec(), Rc::new(child)));
        }
        Ok(Rc::clone(&chain.last().unwrap().1))
    }

    // Opens `prefix` below `fd` and requires the identity recorded for it, then applies guards.
    fn open_expected(&self, fd: &OwnedFd, prefix: &[u8]) -> Result<OwnedFd, Error> {
        let expected = self
            .expected
            .get(prefix)
            .and_then(Option::as_ref)
            .ok_or_else(|| refused(prefix, "missing expected ancestor"))?;
        let child = open_directory(fd, name(prefix), prefix)?;
        identity(prefix, expected, &fstat(&child).map_err(|e| io(prefix, e))?)?;
        self.guard_directory(&child, prefix)?;
        Ok(child)
    }

    // Drops cached descriptors at or below a directory this checkout removed.
    fn forget_directory(&self, path: &[u8]) {
        let mut chain = self.chain.borrow_mut();
        if let Some(n) = chain
            .iter()
            .position(|(prefix, _)| !prefix.is_empty() && within(prefix, path))
        {
            chain.truncate(n);
        }
    }

    fn guard_directory(&self, fd: &OwnedFd, path: &[u8]) -> Result<(), Error> {
        let stat = fstat(fd).map_err(|e| io(path, e))?;
        if self
            .metadata
            .iter()
            .any(|s| s.st_dev == stat.st_dev && s.st_ino == stat.st_ino)
        {
            return Err(refused(path, "repository metadata alias"));
        }
        if !path.is_empty() && repository_marker(fd, path)? {
            return Err(refused(path, "nested repository"));
        }
        Ok(())
    }

    // Applies the enumerated-directory name policy when a read-only phase is active.
    fn check_name(&self, fd: &OwnedFd, path: &[u8], cancel: &AtomicBool) -> Result<(), Error> {
        let mut listings = self.listings.borrow_mut();
        let Some(listings) = listings.as_mut() else {
            return Ok(());
        };
        let directory = parent_path(path);
        if !listings.contains_key(directory) {
            let mut budget = self.limits.max_directory_entries;
            let listing = Listing::new(names(fd, directory, &mut budget, cancel)?, self.policy);
            listings.insert(directory.to_vec(), listing);
        }
        listings[directory].check(fd, path)
    }

    fn verify_expected(&self, fd: &OwnedFd, path: &[u8]) -> Result<(), Error> {
        let current = optional_stat(fd, name(path), path)?;
        match (self.expected[path].as_ref(), current.as_ref()) {
            (None, None) => Ok(()),
            (Some(before), Some(after)) if kind(before) == FileType::Directory => {
                identity(path, before, after)
            }
            (Some(before), Some(after)) => same(path, before, after).map_err(Into::into),
            _ => Err(refused(path, "path changed since preparation")),
        }
    }

    fn verify_leaf(
        &self,
        side: Side,
        fd: &OwnedFd,
        path: &[u8],
        leaf: TreeValue,
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let (mode, bytes) = self.read_tracked(fd, path, leaf, budget, cancel)?;
        if mode != leaf.mode || bytes != self.content(side, path, leaf) {
            return Err(refused(path, "local tracked modification"));
        }
        Ok(())
    }

    /// The mode and bytes of the tracked file at `path`, checked against its expected identity.
    fn read_tracked(
        &self,
        fd: &OwnedFd,
        path: &[u8],
        leaf: TreeValue,
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<(EntryMode, Vec<u8>), Error> {
        self.verify_expected(fd, path)?;
        let stat = optional_stat(fd, name(path), path)?
            .ok_or_else(|| refused(path, "tracked file is missing"))?;
        let max = self.limits.max_file_bytes.min(*budget);
        let bytes = read_leaf(fd, path, &stat, max, cancel)?;
        charge(budget, bytes.len(), "worktree bytes")?;
        let mode = match kind(&stat) {
            FileType::RegularFile if !self.symlinks && leaf.mode == EntryMode::Symlink => {
                EntryMode::Symlink
            }
            FileType::RegularFile if stat.st_mode & 0o100 != 0 => EntryMode::Executable,
            FileType::RegularFile => EntryMode::Blob,
            FileType::Symlink => EntryMode::Symlink,
            _ => return Err(refused(path, "tracked path is not a file or symlink")),
        };
        Ok((mode, bytes))
    }

    fn apply(
        &mut self,
        operation: &Applied,
        cancel: &AtomicBool,
        hook: &mut Hook<'_>,
        cleanup: &mut Vec<Error>,
        report: &mut Report,
    ) -> Result<(), Error> {
        let path = &operation.path;
        let fd = self.parent(path, cancel)?;
        self.verify_expected(&fd, path)?;
        match operation.action {
            Action::Remove => {
                if self.local == Local::Discard {
                    // The file's identity still matches preparation (checked above); its content
                    // is discarded, so only its kind is rechecked.
                    let stat = optional_stat(&fd, name(path), path)?
                        .ok_or_else(|| refused(path, "tracked file is missing"))?;
                    if !matches!(kind(&stat), FileType::RegularFile | FileType::Symlink) {
                        return Err(refused(path, "tracked path is not a file or symlink"));
                    }
                } else {
                    let mut budget = self.limits.max_worktree_bytes;
                    self.verify_leaf(
                        Side::Baseline,
                        &fd,
                        path,
                        self.old[path],
                        &mut budget,
                        cancel,
                    )?;
                }
                hook("before delete", path)?;
                self.recheck_boundary(&fd, path, cancel)?;
                rustix::fs::unlinkat(&*fd, OsStr::from_bytes(name(path)), AtFlags::empty())
                    .map_err(|e| io(path, e))?;
                report.applied.push(operation.clone());
                hook("after namespace", path)?;
                self.expected.insert(path.clone(), None);
            }
            Action::RemoveDirectory => {
                let child = open_directory(&fd, name(path), path)?;
                self.guard_directory(&child, path)?;
                if !names(
                    &child,
                    path,
                    &mut { self.limits.max_directory_entries },
                    cancel,
                )?
                .is_empty()
                {
                    return Err(refused(path, "directory is no longer empty"));
                }
                hook("before delete", path)?;
                self.recheck_boundary(&fd, path, cancel)?;
                self.forget_directory(path);
                rustix::fs::unlinkat(&*fd, OsStr::from_bytes(name(path)), AtFlags::REMOVEDIR)
                    .map_err(|e| io(path, e))?;
                report.applied.push(operation.clone());
                hook("after namespace", path)?;
                self.expected.insert(path.clone(), None);
            }
            Action::CreateDirectory => {
                hook("before mkdir", path)?;
                self.recheck_boundary(&fd, path, cancel)?;
                rustix::fs::mkdirat(
                    &*fd,
                    OsStr::from_bytes(name(path)),
                    Mode::from_raw_mode(0o755),
                )
                .map_err(|e| created(path, e))?;
                report.applied.push(operation.clone());
                hook("after namespace", path)?;
                self.expected
                    .insert(path.clone(), optional_stat(&fd, name(path), path)?);
            }
            Action::Install => {
                let stat = self.install(&fd, path, cancel, hook, cleanup, report)?;
                self.expected.insert(path.clone(), Some(stat));
            }
        }
        Ok(())
    }

    // Reopens the ancestors from the verified root immediately before a namespace mutation,
    // without enumerating directories: the live guards and identities must still hold, and the
    // exact name must still match the plan. Exclusive creation covers names that appeared since.
    fn recheck_boundary(
        &self,
        fd: &OwnedFd,
        path: &[u8],
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        check(cancel)?;
        let root = Rc::clone(&self.chain.borrow()[0].1);
        // The cached root descriptor follows a moved root, so require the root path to still
        // name it before mutating anything beneath it.
        let current = rustix::fs::stat(self.root).map_err(|e| io(b"", e))?;
        identity(b"", &self.root_stat, &current)?;
        self.guard_directory(&root, b"")?;
        let mut reopened = None;
        for prefix in prefixes(parent_path(path)) {
            check(cancel)?;
            let child = self.open_expected(reopened.as_ref().unwrap_or(&*root), prefix)?;
            reopened = Some(child);
        }
        identity(
            parent_path(path),
            &fstat(fd).map_err(|e| io(path, e))?,
            &fstat(reopened.as_ref().unwrap_or(&*root)).map_err(|e| io(path, e))?,
        )?;
        self.verify_expected(fd, path)
    }

    // Creates the leaf exclusively at its final name, so it can never replace an entry that
    // appeared after preparation. Until its content is complete it is not reported as applied;
    // a failure removes it again only while it is still the entry this call created.
    fn install(
        &self,
        fd: &OwnedFd,
        path: &[u8],
        cancel: &AtomicBool,
        hook: &mut Hook<'_>,
        cleanup: &mut Vec<Error>,
        report: &mut Report,
    ) -> Result<Stat, Error> {
        let leaf = self.target[path];
        let bytes = self.content(Side::Target, path, leaf);
        hook("before create", path)?;
        self.recheck_boundary(fd, path, cancel)?;
        let (owned, file) = create_leaf(
            fd,
            path,
            if !self.symlinks && leaf.mode == EntryMode::Symlink {
                EntryMode::Blob
            } else {
                leaf.mode
            },
            bytes,
        )?;
        let result: Result<Stat, Error> = (|| {
            let Some(mut file) = file else {
                return Ok(owned);
            };
            for chunk in bytes.chunks(64 * 1024) {
                check(cancel)?;
                hook("before write", path)?;
                file.write_all(chunk).map_err(|e| io(path, e))?;
            }
            // The umask can narrow the requested permissions; restore them exactly if it did.
            if owned.st_mode & 0o7777 != permissions(leaf.mode) {
                rustix::fs::fchmod(&file, Mode::from_raw_mode(permissions(leaf.mode)))
                    .map_err(|e| io(path, e))?;
            }
            fstat(&file).map_err(|e| io(path, e))
        })();
        match result {
            Ok(stat) => {
                report.applied.push(op(path, Action::Install));
                hook("after namespace", path)?;
                Ok(stat)
            }
            Err(error) => {
                if let Err(failed) = remove_owned(fd, path, &owned, hook) {
                    cleanup.push(failed);
                }
                Err(error)
            }
        }
    }

    // A gitlink is an external commit identity, never a blob or a repository we may enter.
    // Initial checkout only creates its empty directory and verifies that it stayed empty.
    fn verify_empty_gitlink(
        &self,
        parent: &OwnedFd,
        path: &[u8],
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        let directory = self.open_expected(parent, path)?;
        let mut budget = self.limits.max_directory_entries;
        if !names(&directory, path, &mut budget, cancel)?.is_empty() {
            return Err(refused(path, "initial gitlink directory is not empty"));
        }
        Ok(())
    }

    fn verify_target(&self, cancel: &AtomicBool, hook: &mut Hook<'_>) -> Result<(), Error> {
        self.begin_phase(true);
        let mut budget = self.limits.max_worktree_bytes;
        for (path, leaf) in &self.target {
            check(cancel)?;
            let fd = self.parent(path, cancel)?;
            self.check_name(&fd, path, cancel)?;
            if leaf.mode == EntryMode::Gitlink {
                self.verify_empty_gitlink(&fd, path, cancel)?;
                continue;
            }
            if self.kept_raw.contains(path) {
                let (mode, bytes) = self.read_tracked(&fd, path, *leaf, &mut budget, cancel)?;
                if mode != leaf.mode || bytes != self.blobs[&leaf.id] {
                    return Err(refused(path, "local tracked modification"));
                }
                continue;
            }
            self.verify_leaf(Side::Target, &fd, path, *leaf, &mut budget, cancel)?;
        }
        // Removed paths not reused as directories/leaves must still be absent.
        for (path, stat) in &self.expected {
            check(cancel)?;
            hook("verify expected", path)?;
            check(cancel)?;
            if stat.is_none() && !has_descendant(&self.target, path) {
                // An absent ancestor makes its old descendants absent as well.
                if prefixes(parent_path(path)).any(|p| {
                    self.expected
                        .get(p)
                        .is_some_and(|s| s.as_ref().is_none_or(|s| kind(s) != FileType::Directory))
                }) {
                    continue;
                }
                let fd = self.parent(path, cancel)?;
                self.verify_expected(&fd, path)?;
            }
        }
        Ok(())
    }
}

// Name policy for one enumerated directory: exact names grouped by the volume's name key, plus
// refusals that apply to every name checked in it.
struct Listing {
    policy: NamePolicy,
    folded: BTreeMap<Vec<u8>, Vec<Vec<u8>>>,
    metadata_marker: bool,
}

impl Listing {
    fn new(names: Vec<Vec<u8>>, policy: NamePolicy) -> Self {
        let mut listing = Self {
            policy,
            folded: BTreeMap::new(),
            metadata_marker: false,
        };
        for actual in names {
            listing.metadata_marker |= actual.eq_ignore_ascii_case(b".git") && actual != b".git";
            listing
                .folded
                .entry(policy.key(&actual))
                .or_default()
                .push(actual);
        }
        listing
    }

    fn check(&self, fd: &OwnedFd, path: &[u8]) -> Result<(), Error> {
        let wanted = name(path);
        let same_name = self.folded.get(&self.policy.key(wanted));
        if same_name.is_some_and(|names| names.iter().any(|actual| actual != wanted)) {
            return Err(refused(
                path,
                "existing entry shares this name on the volume",
            ));
        }
        let git = self
            .folded
            .get(&self.policy.key(b".git"))
            .is_some_and(|names| names.iter().any(|actual| actual == b".git"));
        if self.metadata_marker || (git && !parent_path(path).is_empty()) {
            return Err(refused(path, "repository metadata marker"));
        }
        // The volume may merge names the policy keeps apart; its own lookup is the authority.
        let exact = same_name.is_some_and(|names| names.iter().any(|actual| actual == wanted));
        if !exact && optional_stat(fd, wanted, path)?.is_some() {
            return Err(refused(path, "filesystem name alias"));
        }
        Ok(())
    }
}

// One lower-bound lookup replaces a full target scan for each removed path. The slash
// distinguishes descendants from neighboring names such as "a-other" or "ab".
fn has_descendant(leaves: &Leaves, path: &[u8]) -> bool {
    let prefix = joined(path, b"");
    leaves
        .range::<[u8], _>((
            std::ops::Bound::Included(prefix.as_slice()),
            std::ops::Bound::Unbounded,
        ))
        .next()
        .is_some_and(|(candidate, _)| candidate.starts_with(&prefix))
}

fn marker_slot(name: &[u8]) -> Option<usize> {
    [b"HEAD".as_slice(), b"objects", b"refs"]
        .iter()
        .position(|marker| name.eq_ignore_ascii_case(marker))
}

fn reject_marker_set(path: &[u8], counts: &[usize; 3]) -> Result<(), Error> {
    if counts.iter().all(|count| *count != 0) {
        return Err(refused(path, "planned nested repository markers"));
    }
    Ok(())
}

/// Every path the index tracks, at any stage, for [`Local::Discard`].
fn tracked(entries: &[index::Entry], cancel: &AtomicBool) -> Result<Leaves, Error> {
    let mut leaves = Leaves::new();
    for entry in entries {
        check(cancel)?;
        if entry.skip_worktree || entry.mode == index::Mode::SparseDirectory {
            return Err(refused(
                &entry.path,
                "sparse checkout entries require caller policy",
            ));
        }
        leaves
            .entry(entry.path.clone())
            .or_insert_with(|| value(entry));
    }
    Ok(leaves)
}

fn flatten(
    objects: &crate::Objects,
    tree: Option<ObjectId>,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Leaves, Error> {
    Ok(objects
        .compare_trees_with(None, tree, limits.trees, cancel)?
        .into_iter()
        .map(|change| (change.path, change.new.unwrap()))
        .collect())
}
fn value(entry: &index::Entry) -> TreeValue {
    TreeValue {
        id: entry.id,
        mode: match entry.mode {
            index::Mode::Regular => EntryMode::Blob,
            index::Mode::Executable => EntryMode::Executable,
            index::Mode::Symlink => EntryMode::Symlink,
            index::Mode::SparseDirectory => crate::EntryMode::Tree,
            index::Mode::Gitlink => EntryMode::Gitlink,
        },
    }
}
fn index_mode(mode: EntryMode) -> index::Mode {
    match mode {
        EntryMode::Blob => index::Mode::Regular,
        EntryMode::Executable => index::Mode::Executable,
        EntryMode::Symlink => index::Mode::Symlink,
        _ => index::Mode::Gitlink,
    }
}
fn op(path: &[u8], action: Action) -> Applied {
    Applied {
        path: path.to_vec(),
        action,
    }
}
fn parent_path(path: &[u8]) -> &[u8] {
    path.iter()
        .rposition(|b| *b == b'/')
        .map_or(b"", |n| &path[..n])
}
fn name(path: &[u8]) -> &[u8] {
    path.rsplit(|b| *b == b'/').next().unwrap()
}
fn prefixes(path: &[u8]) -> impl Iterator<Item = &[u8]> {
    (1..=path.len())
        .filter(|n| *n == path.len() || path[*n] == b'/')
        .map(|n| &path[..n])
}
fn directories(leaves: &Leaves) -> BTreeSet<Vec<u8>> {
    leaves
        .iter()
        .flat_map(|(path, leaf)| {
            let directory = if leaf.mode == EntryMode::Gitlink {
                path.as_slice()
            } else {
                parent_path(path)
            };
            prefixes(directory).map(<[u8]>::to_vec)
        })
        .collect()
}

fn joined(parent: &[u8], name: &[u8]) -> Vec<u8> {
    let mut path = parent.to_vec();
    if !path.is_empty() {
        path.push(b'/');
    }
    path.extend_from_slice(name);
    path
}
fn kind(stat: &Stat) -> FileType {
    FileType::from_raw_mode(stat.st_mode)
}
fn identity(path: &[u8], a: &Stat, b: &Stat) -> Result<(), Error> {
    if a.st_dev != b.st_dev || a.st_ino != b.st_ino || a.st_mode != b.st_mode {
        Err(refused(path, "directory identity changed"))
    } else {
        Ok(())
    }
}
fn open_directory(fd: &OwnedFd, name: &[u8], path: &[u8]) -> Result<OwnedFd, Error> {
    openat(
        fd,
        OsStr::from_bytes(name),
        directory_flags(),
        Mode::empty(),
    )
    .map_err(|e| io(path, e))
}
fn charge(remaining: &mut usize, count: usize, label: &'static str) -> Result<(), Error> {
    *remaining = remaining.checked_sub(count).ok_or(Error::Limit(label))?;
    Ok(())
}
fn io(path: &[u8], source: impl Into<std::io::Error>) -> Error {
    Error::Io {
        path: path.to_vec(),
        source: source.into(),
    }
}
fn names(
    fd: &OwnedFd,
    path: &[u8],
    budget: &mut usize,
    cancel: &AtomicBool,
) -> Result<Vec<Vec<u8>>, Error> {
    let mut names = Vec::new();
    for entry in Dir::read_from(fd).map_err(|e| io(path, e))? {
        check(cancel)?;
        let entry = entry.map_err(|e| io(path, e))?;
        let name = entry.file_name().to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        charge(budget, 1, "directory entries")?;
        if name.len() > 255 {
            return Err(Error::Limit("directory name bytes"));
        }
        names.push(name.to_vec());
    }
    Ok(names)
}
fn read_leaf(
    fd: &OwnedFd,
    path: &[u8],
    stat: &Stat,
    max: usize,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let bytes = match kind(stat) {
        FileType::Symlink => {
            let mut bytes = vec![0; max.min(1024).saturating_add(1)];
            let n =
                rustix::fs::readlinkat_raw(fd, OsStr::from_bytes(name(path)), bytes.as_mut_slice())
                    .map_err(|e| io(path, e))?;
            if n == bytes.len() {
                return Err(Error::Limit("symlink bytes"));
            }
            bytes.truncate(n);
            bytes
        }
        FileType::RegularFile => {
            let opened = openat(
                fd,
                OsStr::from_bytes(name(path)),
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(|e| io(path, e))?;
            same(path, stat, &fstat(&opened).map_err(|e| io(path, e))?)?;
            let mut file = File::from(opened);
            let mut bytes = Vec::new();
            let mut chunk = [0; 64 * 1024];
            loop {
                check(cancel)?;
                let remaining = max - bytes.len();
                let n = file
                    .read(&mut chunk[..remaining.saturating_add(1).min(64 * 1024)])
                    .map_err(|e| io(path, e))?;
                if n > remaining {
                    return Err(Error::Limit("file bytes"));
                }
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&chunk[..n]);
            }
            same(path, stat, &fstat(&file).map_err(|e| io(path, e))?)?;
            bytes
        }
        _ => return Err(refused(path, "not a regular file or symlink")),
    };
    let after =
        optional_stat(fd, name(path), path)?.ok_or_else(|| refused(path, "file disappeared"))?;
    same(path, stat, &after)?;
    Ok(bytes)
}
// Exclusive creation fails on any existing entry, including a case or normalization alias.
fn create_leaf(
    fd: &OwnedFd,
    path: &[u8],
    mode: EntryMode,
    bytes: &[u8],
) -> Result<(Stat, Option<File>), Error> {
    let leaf = OsStr::from_bytes(name(path));
    if mode == EntryMode::Symlink {
        rustix::fs::symlinkat(OsStr::from_bytes(bytes), fd, leaf).map_err(|e| created(path, e))?;
        let stat = optional_stat(fd, name(path), path)?
            .ok_or_else(|| refused(path, "created symlink disappeared"))?;
        return Ok((stat, None));
    }
    let file = openat(
        fd,
        leaf,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(permissions(mode)),
    )
    .map_err(|e| created(path, e))?;
    let stat = fstat(&file).map_err(|e| io(path, e))?;
    Ok((stat, Some(File::from(file))))
}

fn permissions(mode: EntryMode) -> rustix::fs::RawMode {
    if mode == EntryMode::Executable {
        0o755
    } else {
        0o644
    }
}

// Removes an incomplete leaf only while its name still refers to the entry this checkout created.
fn remove_owned(fd: &OwnedFd, path: &[u8], owned: &Stat, hook: &mut Hook<'_>) -> Result<(), Error> {
    hook("before cleanup", path)?;
    let current = optional_stat(fd, name(path), path)?
        .ok_or_else(|| refused(path, "created leaf disappeared"))?;
    if owned.st_dev != current.st_dev
        || owned.st_ino != current.st_ino
        || kind(owned) != kind(&current)
    {
        return Err(refused(path, "created leaf was replaced; refusing cleanup"));
    }
    rustix::fs::unlinkat(fd, OsStr::from_bytes(name(path)), AtFlags::empty())
        .map_err(|error| io(path, error))
}

fn created(path: &[u8], error: rustix::io::Errno) -> Error {
    if error == rustix::io::Errno::EXIST {
        refused(path, "path appeared since preparation")
    } else {
        io(path, error)
    }
}

// Whether `ancestor` is `directory` itself or one of its ancestors; the root is empty.
fn within(directory: &[u8], ancestor: &[u8]) -> bool {
    ancestor.is_empty()
        || directory
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.is_empty() || rest[0] == b'/')
}

#[cfg(test)]
mod tests;
