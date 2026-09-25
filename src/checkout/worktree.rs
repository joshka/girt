use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rustix::fd::OwnedFd;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, Stat, fstat, openat};

use super::types::{check, refused};
use super::{Action, Applied, Error, Failure, Limits, Report, Stage};
use crate::status::worktree::{
    directory_flags, open_root, optional_stat, repository_marker, same, validate_path,
};
use crate::{EntryMode, ObjectId, ObjectKind, Repository, TreeValue, index};

type Leaves = BTreeMap<Vec<u8>, TreeValue>;
type Hook<'a> = dyn FnMut(&str, &[u8]) -> Result<(), Error> + 'a;

pub(super) fn run(
    repo: &Repository,
    baseline: Option<ObjectId>,
    target: Option<ObjectId>,
    limits: Limits,
    cancel: &AtomicBool,
    hook: &mut Hook<'_>,
) -> Result<Report, Failure> {
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
        let mut plan = Plan::prepare(repo, root, baseline, target, edit, limits, cancel)?;
        hook("prepared", b"")?;
        check(cancel)?;
        report.stage = Stage::Worktree;
        for operation in std::mem::take(&mut plan.operations) {
            hook("before operation", &operation.path)?;
            check(cancel)?;
            plan.apply(&operation, cancel, hook, &mut cleanup, &mut report)?;
            hook("after operation", &report.applied.last().unwrap().path)?;
        }
        report.stage = Stage::Publication;
        hook("before publication", b"")?;
        plan.verify_target(cancel, hook)?;
        check(cancel)?;
        edit.publish()?;
        report.index_published = true;
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

// One bounded plan owns verified payloads and the expected namespace after each operation.
// Directory identity is stable across our own child mutations; leaf metadata/content is not.
struct Plan<'a> {
    root: &'a Path,
    root_stat: Stat,
    metadata: Vec<Stat>,
    expected: BTreeMap<Vec<u8>, Option<Stat>>,
    old: Leaves,
    target: Leaves,
    blobs: BTreeMap<ObjectId, Vec<u8>>,
    operations: Vec<Applied>,
    limits: Limits,
}

impl<'a> Plan<'a> {
    fn prepare(
        repo: &Repository,
        root: &'a Path,
        baseline: Option<ObjectId>,
        target: Option<ObjectId>,
        edit: &mut index::IndexEdit,
        mut limits: Limits,
        cancel: &AtomicBool,
    ) -> Result<Self, Error> {
        let objects = repo.objects(limits.packs)?;
        let old = flatten(&objects, baseline, limits, cancel)?;
        let target = flatten(&objects, target, limits, cancel)?;
        let entries = edit.index().entries();
        if entries.len() != old.len() {
            return Err(refused(b"", "index differs from explicit baseline"));
        }
        for entry in entries {
            check(cancel)?;
            if entry.stage != index::Stage::Normal || old.get(&entry.path) != Some(&value(entry)) {
                return Err(refused(&entry.path, "staged changes or conflict"));
            }
        }
        if edit
            .index()
            .extensions()
            .iter()
            .any(|e| e.signature() != *b"TREE")
        {
            return Err(refused(b"", "unsupported index extension"));
        }
        let mut paths = BTreeSet::new();
        let mut aliases = BTreeMap::new();
        for path in old.keys().chain(target.keys()) {
            validate_path(path)?;
            if path.split(|b| *b == b'/').any(|part| part.len() > 255) {
                return Err(refused(path, "path component exceeds 255 bytes"));
            }
            if path.split(|b| *b == b'/').count() > limits.max_depth.min(128) {
                return Err(Error::Limit("directory depth"));
            }
            for prefix in prefixes(path) {
                if paths.insert(prefix.to_vec()) {
                    charge(&mut limits.max_path_bytes, prefix.len(), "path bytes")?;
                    let folded = prefix.to_ascii_lowercase();
                    if let Some(previous) = aliases.insert(folded, prefix.to_vec())
                        && previous != prefix
                    {
                        return Err(refused(prefix, "case-colliding paths"));
                    }
                }
            }
        }
        let mut blobs = BTreeMap::new();
        for leaf in old.values().chain(target.values()) {
            check(cancel)?;
            if leaf.mode == EntryMode::Gitlink {
                return Err(refused(b"", "gitlinks are unsupported"));
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
                .read(leaf.id, read)?
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
        for leaf in old.values().chain(target.values()) {
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
        let mut target_budget = limits.max_worktree_bytes;
        for leaf in target.values() {
            charge(
                &mut target_budget,
                blobs[&leaf.id].len(),
                "target verification bytes",
            )?;
        }
        // Validate the complete index before any working-tree side effects.
        let replacement = target
            .iter()
            .map(|(path, leaf)| index::Entry::new(path.clone(), index_mode(leaf.mode), leaf.id))
            .collect();
        edit.replace_entries(replacement)?;
        edit.discard_tree_cache();
        let fd = open_root(root)?;
        let root_stat = fstat(&fd).map_err(|e| io(b"", e))?;
        let metadata = [repo.git_dir(), repo.common_dir(), repo.object_dir()]
            .into_iter()
            .map(|path| rustix::fs::stat(path).map_err(|e| io(b"", e)))
            .collect::<Result<_, _>>()?;
        let mut plan = Self {
            root,
            root_stat,
            metadata,
            expected: BTreeMap::new(),
            old,
            target,
            blobs,
            operations: vec![],
            limits,
        };
        for path in paths {
            check(cancel)?;
            let stat = plan.inspect_initial(&path, cancel)?;
            plan.expected.insert(path, stat);
        }
        let mut budget = plan.limits.max_worktree_bytes;
        for (path, leaf) in &plan.old {
            let parent = plan.parent(path, cancel)?;
            plan.verify_leaf(&parent, path, *leaf, &mut budget, cancel)?;
        }
        plan.build_operations(cancel)?;
        plan.check_planned_markers(cancel)?;
        Ok(plan)
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
        self.check_names(&parent, path, cancel)?;
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

    fn build_operations(&mut self, cancel: &AtomicBool) -> Result<(), Error> {
        for (path, old) in &self.old {
            if self.target.get(path) != Some(old) {
                self.operations.push(op(path, Action::Remove));
            }
        }
        let old_dirs = directories(&self.old);
        let target_dirs = directories(&self.target);
        let mut remove_dirs = BTreeSet::new();
        for path in self.target.keys() {
            if self.old.contains_key(path) {
                continue;
            }
            match self.expected[path].as_ref() {
                None => (),
                Some(stat) if kind(stat) == FileType::Directory && old_dirs.contains(path) => {
                    self.check_removable(path, &old_dirs, &mut remove_dirs, cancel)?;
                }
                Some(_) => return Err(refused(path, "untracked obstruction")),
            }
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
            if self.old.get(path) != Some(leaf) {
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

    fn parent(&self, path: &[u8], cancel: &AtomicBool) -> Result<OwnedFd, Error> {
        let mut fd = self.root_fd()?;
        for prefix in prefixes(parent_path(path)) {
            check(cancel)?;
            self.check_names(&fd, prefix, cancel)?;
            let expected = self
                .expected
                .get(prefix)
                .and_then(Option::as_ref)
                .ok_or_else(|| refused(prefix, "missing expected ancestor"))?;
            let child = open_directory(&fd, name(prefix), prefix)?;
            identity(prefix, expected, &fstat(&child).map_err(|e| io(prefix, e))?)?;
            self.guard_directory(&child, prefix)?;
            fd = child;
        }
        Ok(fd)
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

    fn check_names(&self, fd: &OwnedFd, path: &[u8], cancel: &AtomicBool) -> Result<(), Error> {
        let mut budget = self.limits.max_directory_entries;
        let mut exact = false;
        for actual in names(fd, parent_path(path), &mut budget, cancel)? {
            exact |= actual == name(path);
            if cfg!(target_os = "macos") && !actual.is_ascii() {
                return Err(refused(path, "non-ASCII directory names on macOS"));
            }
            if actual.eq_ignore_ascii_case(name(path)) && actual != name(path) {
                return Err(refused(path, "filesystem case alias"));
            }
            if actual.eq_ignore_ascii_case(b".git")
                && (actual != b".git" || !parent_path(path).is_empty())
            {
                return Err(refused(path, "repository metadata marker"));
            }
        }
        if !exact && optional_stat(fd, name(path), path)?.is_some() {
            return Err(refused(path, "filesystem name alias"));
        }
        Ok(())
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
        fd: &OwnedFd,
        path: &[u8],
        leaf: TreeValue,
        budget: &mut usize,
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        self.verify_expected(fd, path)?;
        let stat = optional_stat(fd, name(path), path)?
            .ok_or_else(|| refused(path, "tracked file is missing"))?;
        let max = self.limits.max_file_bytes.min(*budget);
        let bytes = read_leaf(fd, path, &stat, max, cancel)?;
        charge(budget, bytes.len(), "worktree bytes")?;
        let mode = match kind(&stat) {
            FileType::RegularFile if stat.st_mode & 0o100 != 0 => EntryMode::Executable,
            FileType::RegularFile => EntryMode::Blob,
            FileType::Symlink => EntryMode::Symlink,
            _ => return Err(refused(path, "tracked path is not a file or symlink")),
        };
        if mode != leaf.mode || bytes != self.blobs[&leaf.id] {
            return Err(refused(path, "local tracked modification"));
        }
        Ok(())
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
        self.check_names(&fd, path, cancel)?;
        self.verify_expected(&fd, path)?;
        match operation.action {
            Action::Remove => {
                let mut budget = self.limits.max_worktree_bytes;
                self.verify_leaf(&fd, path, self.old[path], &mut budget, cancel)?;
                hook("before delete", path)?;
                self.recheck_boundary(&fd, path, cancel)?;
                rustix::fs::unlinkat(&fd, OsStr::from_bytes(name(path)), AtFlags::empty())
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
                rustix::fs::unlinkat(&fd, OsStr::from_bytes(name(path)), AtFlags::REMOVEDIR)
                    .map_err(|e| io(path, e))?;
                report.applied.push(operation.clone());
                hook("after namespace", path)?;
                self.expected.insert(path.clone(), None);
            }
            Action::CreateDirectory => {
                hook("before mkdir", path)?;
                self.recheck_boundary(&fd, path, cancel)?;
                rustix::fs::mkdirat(
                    &fd,
                    OsStr::from_bytes(name(path)),
                    Mode::from_raw_mode(0o755),
                )
                .map_err(|e| io(path, e))?;
                report.applied.push(operation.clone());
                hook("after namespace", path)?;
                self.expected
                    .insert(path.clone(), optional_stat(&fd, name(path), path)?);
            }
            Action::Install => {
                self.install(&fd, path, cancel, hook, cleanup, report)?;
                self.expected
                    .insert(path.clone(), optional_stat(&fd, name(path), path)?);
            }
        }
        Ok(())
    }

    fn recheck_boundary(
        &self,
        fd: &OwnedFd,
        path: &[u8],
        cancel: &AtomicBool,
    ) -> Result<(), Error> {
        check(cancel)?;
        let reopened = self.parent(path, cancel)?;
        identity(
            parent_path(path),
            &fstat(fd).map_err(|e| io(path, e))?,
            &fstat(&reopened).map_err(|e| io(path, e))?,
        )?;
        self.check_names(fd, path, cancel)?;
        self.verify_expected(fd, path)
    }

    fn install(
        &self,
        fd: &OwnedFd,
        path: &[u8],
        cancel: &AtomicBool,
        hook: &mut Hook<'_>,
        cleanup: &mut Vec<Error>,
        report: &mut Report,
    ) -> Result<(), Error> {
        let leaf = self.target[path];
        let bytes = &self.blobs[&leaf.id];
        let (temporary, mut file) = create_temporary(fd, path, leaf.mode, bytes)?;
        let temporary_path = joined(parent_path(path), temporary.as_bytes());
        let owned = match optional_stat(fd, temporary.as_bytes(), &temporary_path) {
            Ok(Some(stat)) => stat,
            _ => {
                cleanup.push(refused(
                    &temporary_path,
                    "cannot identify created temporary; left in place",
                ));
                return Err(refused(path, "cannot identify checkout temporary"));
            }
        };
        let result: Result<(), Error> = (|| {
            if let Some(file) = file.as_mut() {
                for chunk in bytes.chunks(64 * 1024) {
                    check(cancel)?;
                    hook("before write", path)?;
                    file.write_all(chunk).map_err(|e| io(path, e))?;
                }
                let mode = if leaf.mode == EntryMode::Executable {
                    0o755
                } else {
                    0o644
                };
                rustix::fs::fchmod(&*file, Mode::from_raw_mode(mode)).map_err(|e| io(path, e))?;
            }
            let ready = optional_stat(fd, temporary.as_bytes(), &temporary_path)?
                .ok_or_else(|| refused(&temporary_path, "temporary disappeared"))?;
            hook("before rename", path)?;
            let current = optional_stat(fd, temporary.as_bytes(), &temporary_path)?
                .ok_or_else(|| refused(&temporary_path, "temporary disappeared"))?;
            same(&temporary_path, &ready, &current)?;
            if owned.st_dev != current.st_dev || owned.st_ino != current.st_ino {
                return Err(refused(&temporary_path, "temporary identity changed"));
            }
            self.recheck_boundary(fd, path, cancel)?;
            // Destination is absent after the removal phase. A hard link creates it exclusively;
            // unlike replacing rename, this cannot overwrite a newly appeared obstruction.
            // linkat with empty flags links the symlink itself rather than its target.
            rustix::fs::linkat(
                fd,
                temporary.as_str(),
                fd,
                OsStr::from_bytes(name(path)),
                AtFlags::empty(),
            )
            .map_err(|e| io(path, e))?;
            report.applied.push(op(path, Action::Install));
            hook("after namespace", path)?;
            Ok(())
        })();
        let cleaned = hook("before cleanup", &temporary_path).and_then(|()| {
            let current = optional_stat(fd, temporary.as_bytes(), &temporary_path)?
                .ok_or_else(|| refused(&temporary_path, "owned temporary disappeared"))?;
            if owned.st_dev != current.st_dev
                || owned.st_ino != current.st_ino
                || kind(&owned) != kind(&current)
            {
                return Err(refused(
                    &temporary_path,
                    "temporary was replaced; refusing cleanup",
                ));
            }
            rustix::fs::unlinkat(fd, temporary.as_str(), AtFlags::empty())
                .map_err(|error| io(&temporary_path, error))
        });
        if let Err(error) = cleaned {
            cleanup.push(error);
        }
        result?;
        if !cleanup.is_empty() {
            return Err(refused(path, "owned temporary cleanup failed"));
        }
        Ok(())
    }

    fn verify_target(&self, cancel: &AtomicBool, hook: &mut Hook<'_>) -> Result<(), Error> {
        self.root_fd()?;
        let mut budget = self.limits.max_worktree_bytes;
        for (path, leaf) in &self.target {
            check(cancel)?;
            let fd = self.parent(path, cancel)?;
            self.check_names(&fd, path, cancel)?;
            self.verify_leaf(&fd, path, *leaf, &mut budget, cancel)?;
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

fn flatten(
    objects: &crate::Objects,
    tree: Option<ObjectId>,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Leaves, Error> {
    Ok(objects
        .compare_trees(None, tree, limits.trees, cancel)?
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
        .keys()
        .flat_map(|p| prefixes(parent_path(p)).map(<[u8]>::to_vec))
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
fn create_temporary(
    fd: &OwnedFd,
    path: &[u8],
    mode: EntryMode,
    bytes: &[u8],
) -> Result<(String, Option<File>), Error> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    for _ in 0..128 {
        let temporary = format!(
            ".girt-checkout-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let result = if mode == EntryMode::Symlink {
            rustix::fs::symlinkat(OsStr::from_bytes(bytes), fd, temporary.as_str()).map(|()| None)
        } else {
            openat(
                fd,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map(|fd| Some(File::from(fd)))
        };
        match result {
            Ok(file) => return Ok((temporary, file)),
            Err(rustix::io::Errno::EXIST) => (),
            Err(e) => return Err(io(path, e)),
        }
    }
    Err(refused(path, "temporary name attempts exhausted"))
}

#[cfg(test)]
mod tests;
